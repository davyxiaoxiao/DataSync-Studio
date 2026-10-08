//! Building the data grid's `WHERE` and `ORDER BY` clauses, and the single-row
//! `UPDATE` behind inline cell editing.
//!
//! Everything the user types is validated here before it reaches the server:
//! filter values go through the same type conversion the Excel import uses, and
//! `LIKE` patterns have their wildcards escaped so searching for `100%` does not
//! match everything.

use serde::{Deserialize, Serialize};

use crate::data::convert::cell_literal;
use crate::dialect::Dialect;
use crate::error::{AppError, AppResult};
use crate::schema::model::{ColumnInfo, TableStructure};

/// Comparison used by one search condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Contains,
    StartsWith,
    EndsWith,
    IsNull,
    NotNull,
    In,
}

impl FilterOp {
    /// Whether the operator needs a value from the user.
    pub fn needs_value(self) -> bool {
        !matches!(self, FilterOp::IsNull | FilterOp::NotNull)
    }

    fn sql_operator(self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::Ne => "<>",
            FilterOp::Gt => ">",
            FilterOp::Ge => ">=",
            FilterOp::Lt => "<",
            FilterOp::Le => "<=",
            _ => "=",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterCondition {
    pub column: String,
    pub operator: FilterOp,
    #[serde(default)]
    pub value: Option<String>,
}

/// One level of the grid's sort order.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortSpec {
    pub column: String,
    #[serde(default)]
    pub descending: bool,
}

/// Escape the `LIKE` wildcards so a literal search stays literal.
fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 4);
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Find a column by name, tolerating the casing the user typed.
fn resolve<'a>(structure: &'a TableStructure, name: &str) -> AppResult<&'a ColumnInfo> {
    let trimmed = name.trim();
    structure
        .columns
        .iter()
        .find(|column| column.name == trimmed)
        .or_else(|| {
            structure
                .columns
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(trimmed))
        })
        .ok_or_else(|| AppError::msg(format!("表 {} 没有字段 {}", structure.name, name)))
}

/// Render one condition as SQL.
fn condition_sql(
    dialect: Dialect,
    structure: &TableStructure,
    filter: &FilterCondition,
) -> AppResult<String> {
    let column = resolve(structure, &filter.column)?;
    let quoted = dialect.quote(&column.name);
    let kind = column.kind();

    let raw = filter.value.as_deref().unwrap_or("").trim().to_string();
    if filter.operator.needs_value() && raw.is_empty() {
        return Err(AppError::msg(format!(
            "字段 {} 的条件缺少值",
            column.name
        )));
    }

    let clause = match filter.operator {
        FilterOp::IsNull => format!("{quoted} IS NULL"),
        FilterOp::NotNull => format!("{quoted} IS NOT NULL"),
        FilterOp::Contains | FilterOp::StartsWith | FilterOp::EndsWith => {
            let escaped = escape_like(&raw);
            let pattern = match filter.operator {
                FilterOp::Contains => format!("%{escaped}%"),
                FilterOp::StartsWith => format!("{escaped}%"),
                _ => format!("%{escaped}"),
            };
            // The escape character is itself dialect-quoted: MySQL would read a
            // bare `'\'` as an unterminated string.
            format!(
                "{quoted} LIKE {} ESCAPE {}",
                dialect.string_literal(&pattern),
                dialect.string_literal("\\")
            )
        }
        FilterOp::In => {
            let items: Vec<&str> = raw
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .collect();
            if items.is_empty() {
                return Err(AppError::msg(format!(
                    "字段 {} 的 IN 条件没有列出任何值",
                    column.name
                )));
            }
            let literals = items
                .iter()
                .map(|item| {
                    cell_literal(item, column, kind, dialect)
                        .map_err(|reason| AppError::msg(format!("字段 {}：{reason}", column.name)))
                })
                .collect::<AppResult<Vec<String>>>()?;
            format!("{quoted} IN ({})", literals.join(", "))
        }
        operator => {
            let literal = cell_literal(&raw, column, kind, dialect)
                .map_err(|reason| AppError::msg(format!("字段 {}：{reason}", column.name)))?;
            if literal == "NULL" {
                // Comparing against NULL with `=` never matches, which would
                // silently return nothing. Say what the user probably meant.
                return Err(AppError::msg(format!(
                    "字段 {} 的条件值为空，请改用「为 NULL」或填写一个值",
                    column.name
                )));
            }
            format!("{quoted} {} {literal}", operator.sql_operator())
        }
    };
    Ok(clause)
}

/// Combine the search conditions into a `WHERE` clause, or `None` when there is
/// nothing to filter on.
pub fn build_where(
    dialect: Dialect,
    structure: &TableStructure,
    filters: &[FilterCondition],
) -> AppResult<Option<String>> {
    if filters.is_empty() {
        return Ok(None);
    }
    let clauses = filters
        .iter()
        .map(|filter| condition_sql(dialect, structure, filter))
        .collect::<AppResult<Vec<String>>>()?;
    Ok(Some(clauses.join(" AND ")))
}

/// Build the `ORDER BY` clause.
///
/// The primary key is appended as a tiebreaker: without it, rows with equal sort
/// values have no defined order and paging can repeat or skip them.
pub fn build_order_by(
    dialect: Dialect,
    structure: &TableStructure,
    sort: &[SortSpec],
    primary_key: &[String],
) -> AppResult<String> {
    let mut parts = Vec::new();
    let mut used: Vec<String> = Vec::new();
    for spec in sort {
        let column = resolve(structure, &spec.column)?;
        used.push(column.name.clone());
        parts.push(format!(
            "{} {}",
            dialect.quote(&column.name),
            if spec.descending { "DESC" } else { "ASC" }
        ));
    }
    for key in primary_key {
        if used.iter().any(|name| name.eq_ignore_ascii_case(key)) {
            continue;
        }
        parts.push(format!("{} ASC", dialect.quote(key)));
    }
    if parts.is_empty() {
        return Ok(String::new());
    }
    Ok(format!(" ORDER BY {}", parts.join(", ")))
}

/// A row is identified by its primary key values.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowKeyValue {
    pub column: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// A single cell edit.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellEdit {
    pub column: String,
    /// The new value; `None` means NULL.
    #[serde(default)]
    pub value: Option<String>,
    /// The value on screen before the edit, used to detect a concurrent change.
    #[serde(default)]
    pub previous: Option<String>,
}

/// Build the `UPDATE` for one cell.
///
/// The old value takes part in the `WHERE`, so a row that changed underneath the
/// grid is not silently overwritten — the statement simply matches nothing and
/// the caller reports the conflict.
pub fn build_cell_update(
    dialect: Dialect,
    namespace: &str,
    table: &str,
    structure: &TableStructure,
    key: &[RowKeyValue],
    edit: &CellEdit,
) -> AppResult<String> {
    if structure.primary_key.is_empty() {
        return Err(AppError::msg(format!(
            "表 {table} 没有主键，无法定位要修改的行"
        )));
    }

    let column = resolve(structure, &edit.column)?;
    if !column.is_writable() {
        return Err(AppError::msg(format!(
            "字段 {} 是生成列，不能直接修改",
            column.name
        )));
    }

    // Every key column has to be supplied, or the UPDATE could hit other rows.
    let mut key_clauses = Vec::new();
    for key_column in &structure.primary_key {
        let supplied = key
            .iter()
            .find(|entry| entry.column.eq_ignore_ascii_case(key_column))
            .ok_or_else(|| {
                AppError::msg(format!("缺少主键字段 {key_column} 的值，无法定位该行"))
            })?;
        let info = resolve(structure, key_column)?;
        let literal = match_literal(dialect, info, supplied.value.as_deref())?;
        key_clauses.push(format!(
            "{} {} {literal}",
            dialect.quote(&info.name),
            dialect.not_distinct_from()
        ));
    }

    let new_literal = write_literal(dialect, column, edit.value.as_deref())?;
    // The guard is always present: the grid knows what it displayed, and
    // matching on it turns a concurrent change into "no rows updated" instead of
    // a silent overwrite. The NULL-safe comparison matters here because a plain
    // `= NULL` never matches anything.
    key_clauses.push(format!(
        "{} {} {}",
        dialect.quote(&column.name),
        dialect.not_distinct_from(),
        match_literal(dialect, column, edit.previous.as_deref())?
    ));

    Ok(format!(
        "UPDATE {} SET {} = {new_literal} WHERE {};",
        dialect.qualified(Some(namespace), table),
        dialect.quote(&column.name),
        key_clauses.join(" AND ")
    ))
}

/// The value to write, honouring nullability.
///
/// An empty input means NULL, which is refused for a NOT NULL column with a
/// message saying what to do instead of letting the server reject the statement.
fn write_literal(
    dialect: Dialect,
    column: &ColumnInfo,
    value: Option<&str>,
) -> AppResult<String> {
    match value {
        None => {
            if column.nullable {
                Ok("NULL".to_string())
            } else {
                Err(AppError::msg(format!(
                    "字段 {} 不允许为空，请填写一个值",
                    column.name
                )))
            }
        }
        Some(text) => cell_literal(text, column, column.kind(), dialect)
            .map_err(|reason| AppError::msg(format!("字段 {}：{reason}", column.name))),
    }
}

/// The value a row is matched on.
///
/// A cell that was NULL on screen compares as NULL, and an empty cell is the
/// same thing from the grid's point of view. Unlike writing, this never needs to
/// respect nullability: it only has to describe what was displayed.
fn match_literal(
    dialect: Dialect,
    column: &ColumnInfo,
    value: Option<&str>,
) -> AppResult<String> {
    match value {
        None => Ok("NULL".to_string()),
        Some(text) if text.trim().is_empty() => Ok("NULL".to_string()),
        Some(text) => cell_literal(text, column, column.kind(), dialect)
            .map_err(|reason| AppError::msg(format!("字段 {}：{reason}", column.name))),
    }
}

/// The two statements the grid needs: the page itself and the filtered total.
#[derive(Debug, Clone)]
pub struct GridQuery {
    pub select_sql: String,
    pub count_sql: String,
}

/// Build the page query and the matching count query.
///
/// `limit` is expected to already include the extra row used to detect whether
/// another page exists.
pub fn build_grid_query(
    dialect: Dialect,
    namespace: &str,
    table: &str,
    structure: &TableStructure,
    filters: &[FilterCondition],
    sort: &[SortSpec],
    limit: u64,
    offset: u64,
) -> AppResult<GridQuery> {
    let qualified = dialect.qualified(Some(namespace), table);
    let where_clause = build_where(dialect, structure, filters)?;
    let order_by = build_order_by(dialect, structure, sort, &structure.primary_key)?;
    let predicate = where_clause
        .as_ref()
        .map(|clause| format!(" WHERE {clause}"))
        .unwrap_or_default();

    let select_list = crate::data::select_list(dialect, structure);
    let mut select_sql = format!(
        "SELECT {} FROM {qualified}{predicate}{order_by} LIMIT {limit}",
        select_list.join(", ")
    );
    if offset > 0 {
        select_sql.push_str(&format!(" OFFSET {offset}"));
    }
    Ok(GridQuery {
        select_sql,
        count_sql: format!("SELECT COUNT(*) FROM {qualified}{predicate}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::model::{ColumnInfo, TableStructure};

    fn column(name: &str, data_type: &str, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            ordinal: 1,
            data_type: data_type.into(),
            full_type: data_type.into(),
            nullable,
            default: None,
            is_auto_increment: false,
            auto_kind: None,
            is_generated: false,
            generation_expression: None,
            generation_kind: None,
            is_unsigned: false,
            comment: None,
            charset: None,
            collation: None,
            udt_name: None,
        }
    }

    fn users() -> TableStructure {
        TableStructure {
            name: "users".into(),
            comment: None,
            engine: None,
            charset: None,
            collation: None,
            primary_key: vec!["id".into()],
            columns: vec![
                column("id", "int", false),
                column("name", "varchar", true),
                column("age", "int", true),
                column("created_at", "datetime", true),
            ],
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
        }
    }

    fn filter(column: &str, operator: FilterOp, value: Option<&str>) -> FilterCondition {
        FilterCondition {
            column: column.into(),
            operator,
            value: value.map(str::to_string),
        }
    }

    fn where_of(dialect: Dialect, filters: &[FilterCondition]) -> String {
        build_where(dialect, &users(), filters)
            .unwrap()
            .unwrap_or_else(|| "<none>".to_string())
    }

    #[test]
    fn conditions_are_combined_with_and() {
        let sql = where_of(
            Dialect::Mysql,
            &[
                filter("name", FilterOp::Eq, Some("amy")),
                filter("age", FilterOp::Ge, Some("18")),
            ],
        );
        assert_eq!(sql, "`name` = 'amy' AND `age` >= 18");
    }

    #[test]
    fn no_conditions_means_no_where_clause() {
        assert_eq!(build_where(Dialect::Mysql, &users(), &[]).unwrap(), None);
    }

    #[test]
    fn values_are_converted_according_to_the_column_type() {
        // A number compared against a number must not be quoted.
        assert_eq!(where_of(Dialect::Postgres, &[filter("age", FilterOp::Gt, Some("30"))]),
                   "\"age\" > 30");
        // A date is normalised, so several spellings work.
        assert_eq!(
            where_of(Dialect::Mysql, &[filter("created_at", FilterOp::Lt, Some("2024/3/5"))]),
            "`created_at` < '2024-03-05 00:00:00'"
        );
        // An unparsable value is refused rather than sent to the server.
        let error = build_where(Dialect::Mysql, &users(), &[filter("age", FilterOp::Eq, Some("abc"))])
            .unwrap_err();
        assert!(error.to_string().contains("不是整数"), "{error}");
    }
    #[test]
    fn like_wildcards_in_the_users_text_are_escaped() {
        // Raw strings keep the expectations readable: what is written here is
        // exactly the SQL the server receives.
        assert_eq!(
            where_of(Dialect::Mysql, &[filter("name", FilterOp::Contains, Some("100%"))]),
            r"`name` LIKE '%100\\%%' ESCAPE '\\'",
            "通配符要转义，且 MySQL 的转义符本身也要再转义一次"
        );
        assert_eq!(
            where_of(Dialect::Postgres, &[filter("name", FilterOp::Contains, Some("100%"))]),
            r#""name" LIKE '%100\%%' ESCAPE '\'"#,
            "PostgreSQL 不做反斜杠反转义"
        );
        assert_eq!(
            where_of(Dialect::Mysql, &[filter("name", FilterOp::StartsWith, Some("a_b"))]),
            r"`name` LIKE 'a\\_b%' ESCAPE '\\'"
        );
        assert_eq!(
            where_of(Dialect::Postgres, &[filter("name", FilterOp::EndsWith, Some("back\\slash"))]),
            r#""name" LIKE '%back\\slash' ESCAPE '\'"#
        )
    }

    #[test]
    fn null_operators_need_no_value() {
        assert_eq!(
            where_of(Dialect::Mysql, &[filter("name", FilterOp::IsNull, None)]),
            "`name` IS NULL"
        );
        assert_eq!(
            where_of(Dialect::Postgres, &[filter("name", FilterOp::NotNull, None)]),
            "\"name\" IS NOT NULL"
        );
        // An empty value elsewhere is a mistake worth reporting.
        let error = build_where(Dialect::Mysql, &users(), &[filter("name", FilterOp::Eq, Some("  "))])
            .unwrap_err();
        assert!(error.to_string().contains("缺少值"), "{error}");
    }

    #[test]
    fn an_empty_value_with_a_comparison_points_at_the_null_operator() {
        // `= NULL` never matches, so silently producing it would look like
        // "the table is empty".
        let error = build_where(Dialect::Mysql, &users(), &[filter("name", FilterOp::Eq, Some(""))])
            .unwrap_err();
        assert!(error.to_string().contains("缺少值"), "{error}");
    }

    #[test]
    fn in_lists_are_split_and_converted_per_item() {
        assert_eq!(
            where_of(Dialect::Mysql, &[filter("age", FilterOp::In, Some("1, 2 ,3"))]),
            "`age` IN (1, 2, 3)"
        );
        let error = build_where(Dialect::Mysql, &users(), &[filter("age", FilterOp::In, Some("1,x"))])
            .unwrap_err();
        assert!(error.to_string().contains("不是整数"), "{error}");
    }

    #[test]
    fn an_unknown_column_is_reported() {
        let error = build_where(Dialect::Mysql, &users(), &[filter("nope", FilterOp::Eq, Some("1"))])
            .unwrap_err();
        assert!(error.to_string().contains("没有字段"), "{error}");
        // Casing that differs from the server's is still accepted.
        assert_eq!(
            where_of(Dialect::Mysql, &[filter("NAME", FilterOp::Eq, Some("amy"))]),
            "`name` = 'amy'"
        );
    }

    #[test]
    fn the_primary_key_is_appended_as_a_paging_tiebreaker() {
        let order = build_order_by(
            Dialect::Postgres,
            &users(),
            &[SortSpec {
                column: "name".into(),
                descending: true,
            }],
            &["id".into()],
        )
        .unwrap();
        assert_eq!(order, " ORDER BY \"name\" DESC, \"id\" ASC");

        // Sorting by the key itself must not repeat it.
        let order = build_order_by(
            Dialect::Mysql,
            &users(),
            &[SortSpec {
                column: "id".into(),
                descending: true,
            }],
            &["id".into()],
        )
        .unwrap();
        assert_eq!(order, " ORDER BY `id` DESC");

        // With nothing selected the grid still pages deterministically.
        let order = build_order_by(Dialect::Mysql, &users(), &[], &["id".into()]).unwrap();
        assert_eq!(order, " ORDER BY `id` ASC");
    }

    #[test]
    fn the_grid_query_carries_the_filter_sort_and_page() {
        let query = build_grid_query(
            Dialect::Mysql,
            "app",
            "users",
            &users(),
            &[filter("name", FilterOp::Contains, Some("am"))],
            &[SortSpec {
                column: "name".into(),
                descending: false,
            }],
            3,
            6,
        )
        .unwrap();
        assert!(query.select_sql.contains("FROM `app`.`users`"), "{}", query.select_sql);
        assert!(query.select_sql.contains("WHERE `name` LIKE '%am%'"), "{}", query.select_sql);
        assert!(query.select_sql.contains("ORDER BY `name` ASC, `id` ASC"), "{}", query.select_sql);
        assert!(query.select_sql.ends_with("LIMIT 3 OFFSET 6"), "{}", query.select_sql);
        // The count uses the same filter but no ordering or paging.
        assert_eq!(
            query.count_sql,
            r"SELECT COUNT(*) FROM `app`.`users` WHERE `name` LIKE '%am%' ESCAPE '\\'",
            "计数必须沿用同一套筛选条件"
        );
    }

    #[test]
    fn postgres_casts_undecodable_columns_to_text_in_the_projection() {
        let mut structure = users();
        structure.columns.push(column("tags", "ARRAY", true));
        let query = build_grid_query(
            Dialect::Postgres,
            "app",
            "users",
            &structure,
            &[],
            &[],
            10,
            0,
        )
        .unwrap();
        assert!(query.select_sql.contains("\"tags\"::text AS \"tags\""), "{}", query.select_sql);
    }

    fn edit(column: &str, value: Option<&str>, previous: Option<&str>) -> CellEdit {
        CellEdit {
            column: column.into(),
            value: value.map(str::to_string),
            previous: previous.map(str::to_string),
        }
    }

    fn key(pairs: &[(&str, Option<&str>)]) -> Vec<RowKeyValue> {
        pairs
            .iter()
            .map(|(column, value)| RowKeyValue {
                column: (*column).to_string(),
                value: value.map(str::to_string),
            })
            .collect()
    }

    #[test]
    fn a_cell_update_matches_the_row_by_key_and_previous_value() {
        let sql = build_cell_update(
            Dialect::Mysql,
            "app",
            "users",
            &users(),
            &key(&[("id", Some("1"))]),
            &edit("name", Some("bob"), Some("amy")),
        )
        .unwrap();
        assert_eq!(
            sql,
            "UPDATE `app`.`users` SET `name` = 'bob' WHERE `id` <=> 1 AND `name` <=> 'amy';"
        );
    }

    #[test]
    fn null_handling_is_null_safe_on_both_engines() {
        // Reading a NULL and writing a value back must match the NULL row.
        let sql = build_cell_update(
            Dialect::Postgres,
            "app",
            "users",
            &users(),
            &key(&[("id", Some("1"))]),
            &edit("name", Some("bob"), None),
        )
        .unwrap();
        assert_eq!(
            sql,
            "UPDATE \"app\".\"users\" SET \"name\" = 'bob' \
             WHERE \"id\" IS NOT DISTINCT FROM 1 \
             AND \"name\" IS NOT DISTINCT FROM NULL;"
        );
        // Clearing a value writes NULL.
        let sql = build_cell_update(
            Dialect::Postgres,
            "app",
            "users",
            &users(),
            &key(&[("id", Some("1"))]),
            &edit("name", None, Some("bob")),
        )
        .unwrap();
        assert!(sql.contains("SET \"name\" = NULL"), "{sql}");
    }

    #[test]
    fn editing_is_refused_without_a_key_or_for_generated_columns() {
        let mut structure = users();
        structure.primary_key.clear();
        let error = build_cell_update(
            Dialect::Mysql,
            "app",
            "users",
            &structure,
            &key(&[("id", Some("1"))]),
            &edit("name", Some("bob"), Some("amy")),
        )
        .unwrap_err();
        assert!(error.to_string().contains("没有主键"), "{error}");

        let mut structure = users();
        structure.columns[1].is_generated = true;
        structure.columns[1].generation_expression = Some("upper(id)".into());
        let error = build_cell_update(
            Dialect::Mysql,
            "app",
            "users",
            &structure,
            &key(&[("id", Some("1"))]),
            &edit("name", Some("bob"), Some("amy")),
        )
        .unwrap_err();
        assert!(error.to_string().contains("生成列"), "{error}");

        // A missing key value would make the UPDATE match the wrong rows.
        let error = build_cell_update(
            Dialect::Mysql,
            "app",
            "users",
            &users(),
            &key(&[]),
            &edit("name", Some("bob"), Some("amy")),
        )
        .unwrap_err();
        assert!(error.to_string().contains("缺少主键字段"), "{error}");
    }
}
