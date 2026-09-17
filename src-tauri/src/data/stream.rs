//! Ordered, chunked reading of a table for streaming comparison.
//!
//! Both sides of a comparison are read in primary-key order with keyset
//! pagination, so the merge join only ever holds one page per side in memory
//! regardless of table size.

use std::cmp::Ordering;
use std::collections::VecDeque;

use bigdecimal::BigDecimal;

use crate::db::value::{ColumnKind, Value};
use crate::db::Pool;
use crate::dialect::Dialect;
use crate::error::{AppError, AppResult};
use crate::schema::model::TableStructure;

/// How a single column is selected and ordered.
#[derive(Debug, Clone)]
pub struct StreamColumn {
    pub name: String,
    /// Expression used both in the SELECT list and in ORDER BY.
    pub expression: String,
    pub kind: ColumnKind,
    /// Textual columns need an explicit byte-order collation so that both
    /// engines order them identically.
    pub byte_ordered: bool,
}

#[derive(Debug, Clone)]
pub struct StreamPlan {
    pub columns: Vec<StreamColumn>,
    /// Indexes into `columns` that form the ordering key, in key order.
    pub key_indexes: Vec<usize>,
}

impl StreamPlan {
    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|c| c.name.clone()).collect()
    }

    pub fn kinds(&self) -> Vec<ColumnKind> {
        self.columns.iter().map(|c| c.kind).collect()
    }

    /// Build the SELECT list and ordering for one side of a comparison.
    pub fn build(
        dialect: Dialect,
        structure: &TableStructure,
        key_columns: &[String],
    ) -> AppResult<StreamPlan> {
        let mut columns = Vec::with_capacity(structure.columns.len());
        for column in &structure.columns {
            let quoted = dialect.quote(&column.name);
            // PostgreSQL cannot decode some types natively; those are read as
            // text so the query never fails outright.
            let (expression, textual) = if dialect == Dialect::Postgres
                && column.needs_text_cast()
            {
                (format!("{quoted}::text"), true)
            } else {
                (quoted, false)
            };
            let kind = column.kind();
            let byte_ordered =
                textual || matches!(kind, ColumnKind::Str | ColumnKind::Text);
            columns.push(StreamColumn {
                name: column.name.clone(),
                expression,
                kind,
                byte_ordered,
            });
        }

        let mut key_indexes = Vec::with_capacity(key_columns.len());
        for key in key_columns {
            let index = columns
                .iter()
                .position(|c| &c.name == key)
                .ok_or_else(|| {
                    AppError::msg(format!("表 {} 中找不到对比字段 {key}", structure.name))
                })?;
            if structure
                .column(key)
                .map(|c| c.nullable)
                .unwrap_or(false)
            {
                return Err(AppError::msg(format!(
                    "对比字段 {key} 允许为空，无法用于流式对比。请改选一个非空字段（通常用主键）。",
                    key = key
                )));
            }
            key_indexes.push(index);
        }
        if key_indexes.is_empty() {
            return Err(AppError::msg("缺少对比字段，无法进行数据对比"));
        }

        Ok(StreamPlan {
            columns,
            key_indexes,
        })
    }
}

/// The ORDER BY expression for a column, forced into byte order when textual so
/// that MySQL and PostgreSQL agree on the ordering.
fn order_expression(dialect: Dialect, column: &StreamColumn) -> String {
    if !column.byte_ordered {
        return column.expression.clone();
    }
    match dialect {
        // `BINARY` casts make MySQL compare raw bytes instead of using the
        // column collation.
        Dialect::Mysql => format!("CAST({} AS BINARY)", column.expression),
        // The `C` collation is byte order in PostgreSQL.
        Dialect::Postgres => format!("{} COLLATE \"C\"", column.expression),
    }
}

fn order_literal(dialect: Dialect, column: &StreamColumn, value: &Value) -> String {
    let literal = value.literal(dialect, false);
    if !column.byte_ordered {
        return literal;
    }
    match dialect {
        Dialect::Mysql => format!("CAST({literal} AS BINARY)"),
        // A quoted literal is already `text`, and the comparison borrows the
        // collation from the left-hand expression.
        Dialect::Postgres => literal,
    }
}

/// A forward-only cursor over one table, ordered by the key columns.
pub struct TableStream<'a> {
    pool: &'a Pool,
    dialect: Dialect,
    table_ref: String,
    plan: StreamPlan,
    order_exprs: Vec<String>,
    batch: usize,
    buffer: VecDeque<Vec<Value>>,
    exhausted: bool,
    last_key: Option<Vec<Value>>,
    /// Rows fetched so far, for progress reporting.
    pub scanned: u64,
}

impl<'a> TableStream<'a> {
    pub fn new(
        pool: &'a Pool,
        dialect: Dialect,
        namespace: &str,
        table: &str,
        plan: StreamPlan,
        batch: usize,
    ) -> TableStream<'a> {
        let order_exprs = plan
            .key_indexes
            .iter()
            .map(|index| order_expression(dialect, &plan.columns[*index]))
            .collect();
        TableStream {
            pool,
            dialect,
            table_ref: dialect.qualified(Some(namespace), table),
            plan,
            order_exprs,
            batch: batch.clamp(1, 20_000),
            buffer: VecDeque::new(),
            exhausted: false,
            last_key: None,
            scanned: 0,
        }
    }

    /// The key of the row that will be returned next, if any.
    pub async fn peek_key(&mut self) -> AppResult<Option<Vec<Value>>> {
        self.ensure_buffered().await?;
        Ok(self.buffer.front().map(|row| self.key_of(row)))
    }

    pub async fn next_row(&mut self) -> AppResult<Option<Vec<Value>>> {
        self.ensure_buffered().await?;
        match self.buffer.pop_front() {
            Some(row) => {
                self.last_key = Some(self.key_of(&row));
                Ok(Some(row))
            }
            None => Ok(None),
        }
    }

    fn key_of(&self, row: &[Value]) -> Vec<Value> {
        self.plan
            .key_indexes
            .iter()
            .map(|index| row.get(*index).cloned().unwrap_or(Value::Null))
            .collect()
    }

    async fn ensure_buffered(&mut self) -> AppResult<()> {
        if !self.buffer.is_empty() || self.exhausted {
            return Ok(());
        }
        let sql = self.page_sql();
        let kinds = self.plan.kinds();
        let rows = self.pool.fetch(&sql, &[], &kinds).await?;
        if rows.len() < self.batch {
            self.exhausted = true;
        }
        if rows.is_empty() {
            return Ok(());
        }
        self.scanned += rows.len() as u64;
        self.buffer.extend(rows);
        Ok(())
    }

    /// Keyset pagination SQL: `WHERE (k1, k2) > (v1, v2)` written out as an
    /// expanded OR chain, which every supported engine accepts.
    fn page_sql(&self) -> String {
        let select_list = self
            .plan
            .columns
            .iter()
            .map(|column| {
                if column.expression == self.dialect.quote(&column.name) {
                    column.expression.clone()
                } else {
                    format!("{} AS {}", column.expression, self.dialect.quote(&column.name))
                }
            })
            .collect::<Vec<_>>()
            .join(", ");

        let mut sql = format!("SELECT {select_list} FROM {}", self.table_ref);

        if let Some(previous) = &self.last_key {
            let key_columns: Vec<&StreamColumn> = self
                .plan
                .key_indexes
                .iter()
                .map(|index| &self.plan.columns[*index])
                .collect();
            let mut clauses = Vec::with_capacity(previous.len());
            for position in 0..previous.len() {
                let mut parts = Vec::with_capacity(position + 1);
                for equal in 0..position {
                    parts.push(format!(
                        "{} = {}",
                        self.order_exprs[equal],
                        order_literal(self.dialect, key_columns[equal], &previous[equal])
                    ));
                }
                parts.push(format!(
                    "{} > {}",
                    self.order_exprs[position],
                    order_literal(self.dialect, key_columns[position], &previous[position])
                ));
                clauses.push(format!("({})", parts.join(" AND ")));
            }
            sql.push_str(&format!(" WHERE {}", clauses.join(" OR ")));
        }

        sql.push_str(&format!(
            " ORDER BY {} LIMIT {}",
            self.order_exprs.join(", "),
            self.batch
        ));
        sql
    }
}

/// Lexicographic comparison of two key tuples.
pub fn compare_key_tuples(left: &[Value], right: &[Value]) -> Ordering {
    for (a, b) in left.iter().zip(right.iter()) {
        let ordering = compare_keys(a, b);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

/// Compare two key values with the same ordering the databases were asked for.
pub fn compare_keys(left: &Value, right: &Value) -> Ordering {
    use Value::*;
    match (left, right) {
        (Null, Null) => Ordering::Equal,
        (Null, _) => Ordering::Less,
        (_, Null) => Ordering::Greater,
        (Bool(a), Bool(b)) => a.cmp(b),
        (Bytes(a), Bytes(b)) => a.cmp(b),
        (Int(_) | Uint(_) | Float(_) | Decimal(_), Int(_) | Uint(_) | Float(_) | Decimal(_)) => {
            match (as_decimal(left), as_decimal(right)) {
                (Some(a), Some(b)) => a.cmp(&b),
                _ => left.canonical().cmp(&right.canonical()),
            }
        }
        // Textual and temporal values were read in byte order by both engines.
        _ => value_bytes(left).cmp(&value_bytes(right)),
    }
}

fn as_decimal(value: &Value) -> Option<BigDecimal> {
    match value {
        Value::Int(v) => Some(BigDecimal::from(*v)),
        Value::Uint(v) => Some(BigDecimal::from(*v)),
        Value::Float(v) => BigDecimal::try_from(*v).ok(),
        Value::Decimal(v) => v.parse().ok(),
        _ => None,
    }
}

fn value_bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::Str(text) | Value::Date(text) | Value::Time(text) | Value::DateTime(text) => {
            text.as_bytes().to_vec()
        }
        Value::Bytes(bytes) => bytes.clone(),
        Value::Bool(v) => vec![u8::from(*v)],
        Value::Json(v) => v.to_string().into_bytes(),
        Value::Null => Vec::new(),
        other => other.canonical().into_bytes(),
    }
}
