pub mod convert;
pub mod import;
pub mod plan;
pub mod stream;
pub mod view;

use serde::{Deserialize, Serialize};

use crate::db::value::{Cell, ColumnKind, Value};
use crate::db::Pool;
use crate::dialect::Dialect;
use crate::error::AppResult;
use crate::schema::model::TableStructure;

/// Column mapping between the source and target table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnPair {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableEndpoint {
    pub connection_id: String,
    #[serde(default)]
    pub namespace: String,
    pub table: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyValue {
    pub column: String,
    pub value: Cell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellChange {
    pub column: String,
    pub before: Cell,
    pub after: Cell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowChange {
    pub key: Vec<KeyValue>,
    pub cells: Vec<CellChange>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataDiffSummary {
    pub source_rows: usize,
    pub target_rows: usize,
    pub inserts: usize,
    pub updates: usize,
    pub deletes: usize,
    pub unchanged: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataDiffResult {
    pub plan_id: String,
    pub columns: Vec<ColumnPair>,
    pub key_columns: Vec<ColumnPair>,
    pub summary: DataDiffSummary,
    pub insert_preview: Vec<RowChange>,
    pub update_preview: Vec<RowChange>,
    pub delete_preview: Vec<RowChange>,
    pub preview_truncated: bool,
    pub sql_preview: Vec<String>,
    pub warnings: Vec<String>,
}

pub const PREVIEW_ROWS: usize = 200;
pub const SQL_PREVIEW_LIMIT: usize = 300;

/// A resolved read plan for one table.
pub struct ReadPlan {
    pub select_sql: String,
    pub kinds: Vec<ColumnKind>,
}

/// The projection for a table read.
///
/// PostgreSQL types the driver cannot decode natively are cast to text and
/// aliased back, so the result set always carries the original column names.
pub fn select_list(dialect: Dialect, structure: &TableStructure) -> Vec<String> {
    structure
        .columns
        .iter()
        .map(|column| {
            let quoted = dialect.quote(&column.name);
            if dialect == Dialect::Postgres && column.needs_text_cast() {
                format!("{quoted}::text AS {quoted}")
            } else {
                quoted
            }
        })
        .collect()
}
pub async fn read_rows(pool: &Pool, plan: &ReadPlan) -> AppResult<Vec<Vec<Value>>> {
    pool.fetch(&plan.select_sql, &[], &plan.kinds).await
}

/// Index of a column name inside a row vector.
pub fn column_index(columns: &[String], name: &str) -> Option<usize> {
    columns.iter().position(|c| c == name)
}

/// Build the DML statements for one action.
pub fn build_insert_statements(
    target_dialect: Dialect,
    namespace: &str,
    table: &str,
    source_columns: &[String],
    target_columns: &[String],
    target_kinds: &[ColumnKind],
    column_pairs: &[ColumnPair],
    rows: &[Vec<Value>],
    batch_size: usize,
) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }
    let q = |name: &str| target_dialect.quote(name);

    // Values are read from the *source* row but written against the *target*
    // column list, and the two orders are independent (a column may have been
    // added in the middle on one side). Resolve each pair to its indices once
    // so the INSERT list and the value tuple always agree.
    let mut pairs: Vec<(usize, usize)> = Vec::with_capacity(column_pairs.len());
    for pair in column_pairs {
        let Some(source_idx) = column_index(source_columns, &pair.source) else {
            continue;
        };
        let Some(target_idx) = column_index(target_columns, &pair.target) else {
            continue;
        };
        pairs.push((source_idx, target_idx));
    }
    if pairs.is_empty() {
        return Vec::new();
    }

    let column_list = pairs
        .iter()
        .map(|(_, target_idx)| q(&target_columns[*target_idx]))
        .collect::<Vec<_>>()
        .join(", ");
    let target = target_dialect.qualified(Some(namespace), table);
    let batch = batch_size.max(1);

    let mut statements = Vec::new();
    for chunk in rows.chunks(batch) {
        let values: Vec<String> = chunk
            .iter()
            .map(|row| {
                let cells: Vec<String> = pairs
                    .iter()
                    .map(|(source_idx, target_idx)| {
                        let value = row.get(*source_idx).cloned().unwrap_or(Value::Null);
                        let json_column =
                            matches!(target_kinds.get(*target_idx), Some(ColumnKind::Json));
                        value.literal(target_dialect, json_column)
                    })
                    .collect();
                format!("({})", cells.join(", "))
            })
            .collect();
        statements.push(format!(
            "INSERT INTO {target} ({column_list}) VALUES {};",
            values.join(", ")
        ));
    }
    statements
}

pub fn build_update_statements(
    target_dialect: Dialect,
    namespace: &str,
    table: &str,
    target_columns: &[String],
    target_kinds: &[ColumnKind],
    key_pairs: &[ColumnPair],
    updates: &[(Vec<Value>, Vec<(usize, Value)>)],
) -> Vec<String> {
    let q = |name: &str| target_dialect.quote(name);
    let target = target_dialect.qualified(Some(namespace), table);
    let key_indexes: Vec<(usize, &str)> = key_pairs
        .iter()
        .filter_map(|p| column_index(target_columns, &p.target).map(|i| (i, p.target.as_str())))
        .collect();

    let mut statements = Vec::new();
    for (target_row, changes) in updates {
        let assignments: Vec<String> = changes
            .iter()
            .map(|(idx, value)| {
                let json_column = matches!(target_kinds.get(*idx), Some(ColumnKind::Json));
                format!(
                    "{} = {}",
                    q(&target_columns[*idx]),
                    value.literal(target_dialect, json_column)
                )
            })
            .collect();
        let conditions: Vec<String> = key_indexes
            .iter()
            .map(|(idx, name)| {
                let value = target_row.get(*idx).cloned().unwrap_or(Value::Null);
                let json_column = matches!(target_kinds.get(*idx), Some(ColumnKind::Json));
                format!("{} = {}", q(name), value.literal(target_dialect, json_column))
            })
            .collect();
        statements.push(format!(
            "UPDATE {target} SET {} WHERE {};",
            assignments.join(", "),
            conditions.join(" AND ")
        ));
    }
    statements
}

pub fn build_delete_statements(
    target_dialect: Dialect,
    namespace: &str,
    table: &str,
    target_columns: &[String],
    target_kinds: &[ColumnKind],
    key_pairs: &[ColumnPair],
    rows: &[Vec<Value>],
) -> Vec<String> {
    let q = |name: &str| target_dialect.quote(name);
    let target = target_dialect.qualified(Some(namespace), table);
    let key_indexes: Vec<(usize, &str)> = key_pairs
        .iter()
        .filter_map(|p| column_index(target_columns, &p.target).map(|i| (i, p.target.as_str())))
        .collect();

    rows.iter()
        .map(|row| {
            let conditions: Vec<String> = key_indexes
                .iter()
                .map(|(idx, name)| {
                    let value = row.get(*idx).cloned().unwrap_or(Value::Null);
                    let json_column = matches!(target_kinds.get(*idx), Some(ColumnKind::Json));
                    format!("{} = {}", q(name), value.literal(target_dialect, json_column))
                })
                .collect();
            format!("DELETE FROM {target} WHERE {};", conditions.join(" AND "))
        })
        .collect()
}

/// Preview rows for INSERT: the "after" side comes from the source row.
pub fn preview_inserts(
    source_columns: &[String],
    column_pairs: &[ColumnPair],
    key_pairs: &[ColumnPair],
    rows: &[Vec<Value>],
) -> Vec<RowChange> {
    rows.iter()
        .take(PREVIEW_ROWS)
        .map(|row| RowChange {
            key: key_preview(source_columns, key_pairs, row, true),
            cells: column_pairs
                .iter()
                .map(|pair| CellChange {
                    column: pair.target.clone(),
                    before: Cell::null(),
                    after: column_index(source_columns, &pair.source)
                        .and_then(|idx| row.get(idx))
                        .cloned()
                        .unwrap_or(Value::Null)
                        .to_cell(),
                })
                .collect(),
        })
        .collect()
}

/// Preview rows for UPDATE.
pub fn preview_updates(
    target_columns: &[String],
    key_pairs: &[ColumnPair],
    rows: &[(Vec<Value>, Vec<(usize, Value)>)],
) -> Vec<RowChange> {
    rows.iter()
        .take(PREVIEW_ROWS)
        .map(|(target_row, changes)| RowChange {
            key: key_preview(target_columns, key_pairs, target_row, false),
            cells: changes
                .iter()
                .map(|(idx, value)| CellChange {
                    column: target_columns[*idx].clone(),
                    before: target_row.get(*idx).cloned().unwrap_or(Value::Null).to_cell(),
                    after: value.to_cell(),
                })
                .collect(),
        })
        .collect()
}

/// Preview rows for DELETE.
pub fn preview_deletes(
    target_columns: &[String],
    column_pairs: &[ColumnPair],
    key_pairs: &[ColumnPair],
    rows: &[Vec<Value>],
) -> Vec<RowChange> {
    rows.iter()
        .take(PREVIEW_ROWS)
        .map(|row| RowChange {
            key: key_preview(target_columns, key_pairs, row, false),
            cells: column_pairs
                .iter()
                .map(|pair| CellChange {
                    column: pair.target.clone(),
                    before: column_index(target_columns, &pair.target)
                        .and_then(|idx| row.get(idx))
                        .cloned()
                        .unwrap_or(Value::Null)
                        .to_cell(),
                    after: Cell::null(),
                })
                .collect(),
        })
        .collect()
}

/// Key values as displayed in the preview list. `side_is_source` selects which
/// side of the mapping the column names come from.
fn key_preview(
    columns: &[String],
    key_pairs: &[ColumnPair],
    row: &[Value],
    side_is_source: bool,
) -> Vec<KeyValue> {
    key_pairs
        .iter()
        .filter_map(|pair| {
            let name = if side_is_source { &pair.source } else { &pair.target };
            column_index(columns, name).map(|idx| KeyValue {
                column: name.clone(),
                value: row.get(idx).cloned().unwrap_or(Value::Null).to_cell(),
            })
        })
        .collect()
}
