//! Writing a table out to an Excel workbook.
//!
//! Values keep their kind on the way out: numbers become numeric cells so a
//! spreadsheet can sum and sort them, booleans stay booleans, and NULLs become
//! empty cells rather than the text "NULL". Dates and JSON are written as text,
//! which is unambiguous and round-trips through the import path.

use std::time::Instant;

use rust_xlsxwriter::Workbook;
use sqlx::{Executor, Row};
use serde::{Deserialize, Serialize};

use crate::data::stream::{StreamPlan, TableStream};
use crate::db::value::{ColumnKind, Value};
use crate::db::Pool;
use crate::dialect::Dialect;
use crate::error::{AppError, AppResult};
use crate::jobs::ProgressReporter;
use crate::schema::model::TableStructure;

/// Excel's hard limit for one worksheet.
pub const MAX_ROWS: u64 = 1_048_576;
/// Excel's own limit on a sheet name.
const MAX_SHEET_NAME: usize = 31;
/// Rows fetched per round trip.
const PAGE: usize = 2_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportColumn {
    /// Column on the table.
    pub column: String,
    /// Header written to the sheet; defaults to the column name.
    #[serde(default)]
    pub header: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    #[serde(default)]
    pub sheet_name: Option<String>,
    /// Stop after this many rows; 0 or absent means "as many as Excel allows".
    #[serde(default)]
    pub max_rows: Option<u64>,
    /// Write the header row.
    #[serde(default = "default_true")]
    pub include_header: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            sheet_name: None,
            max_rows: None,
            include_header: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOutcome {
    pub rows: u64,
    pub columns: usize,
    pub path: String,
    pub sheet: String,
    /// True when a row limit cut the export short.
    pub truncated: bool,
    pub cancelled: bool,
    pub elapsed_ms: u64,
    pub warnings: Vec<String>,
}

fn sheet_name(requested: Option<&str>, table: &str) -> String {
    let raw = requested.map(str::trim).filter(|name| !name.is_empty());
    let mut name = raw.unwrap_or(table).to_string();
    // Excel rejects these characters and caps the length.
    for bad in [':', '\\', '/', '?', '*', '[', ']'] {
        name = name.replace(bad, "_");
    }
    if name.chars().count() > MAX_SHEET_NAME {
        name = name.chars().take(MAX_SHEET_NAME).collect();
    }
    if name.is_empty() {
        name = "Sheet1".to_string();
    }
    name
}

/// Resolve the requested columns against the table, filling in default headers.
fn resolve_columns(
    structure: &TableStructure,
    requested: &[ExportColumn],
) -> AppResult<Vec<(usize, String)>> {
    if requested.is_empty() {
        return Err(AppError::msg("请至少选择一列"));
    }
    let mut out = Vec::with_capacity(requested.len());
    for entry in requested {
        let index = structure
            .columns
            .iter()
            .position(|column| column.name == entry.column)
            .or_else(|| {
                structure
                    .columns
                    .iter()
                    .position(|column| column.name.eq_ignore_ascii_case(entry.column.trim()))
            })
            .ok_or_else(|| {
                AppError::msg(format!("表 {} 没有字段 {}", structure.name, entry.column))
            })?;
        let header = entry
            .header
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&structure.columns[index].name)
            .to_string();
        out.push((index, header));
    }
    Ok(out)
}

/// How one value reaches the sheet.
#[derive(Debug)]
enum Cell {
    Empty,
    Text(String),
    Number(f64),
    Boolean(bool),
}

fn cell_of(value: &Value) -> Cell {
    match value {
        Value::Null => Cell::Empty,
        Value::Bool(flag) => Cell::Boolean(*flag),
        // Numeric cells, so Excel can compute with them.
        Value::Int(number) => Cell::Number(*number as f64),
        Value::Uint(number) => Cell::Number(*number as f64),
        Value::Float(number) => {
            if number.is_finite() {
                Cell::Number(*number)
            } else {
                Cell::Text(value.display().unwrap_or_default())
            }
        }
        Value::Decimal(text) => match text.parse::<f64>() {
            // A decimal that survives the round trip becomes a number; one with
            // more precision than a double keeps its exact text instead.
            Ok(parsed) if parsed.is_finite() && format_decimal_round_trips(text, parsed) => {
                Cell::Number(parsed)
            }
            _ => Cell::Text(text.clone()),
        },
        // Anything else keeps the display form: dates, times, JSON, base64 and
        // types PostgreSQL had to cast to text.
        _ => Cell::Text(value.display().unwrap_or_default()),
    }
}

/// Whether writing `parsed` back out yields the same decimal text.
///
/// Without this a `DECIMAL(30,10)` would silently lose digits in the sheet.
fn format_decimal_round_trips(text: &str, parsed: f64) -> bool {
    let rendered = format!("{parsed}");
    let normalize = |value: &str| {
        let trimmed = value.trim().trim_start_matches('+');
        let trimmed = trimmed.trim_start_matches('-');
        let trimmed = trimmed.trim_start_matches('0');
        let trimmed = if trimmed.starts_with('.') {
            format!("0{trimmed}")
        } else {
            trimmed.to_string()
        };
        trimmed.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    normalize(&rendered) == normalize(text)
}

/// One column of an in-memory result set being exported.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRowColumn {
    pub name: String,
    #[serde(default)]
    pub header: Option<String>,
    /// The kind the console decoded the column as, so numbers stay numeric.
    pub kind: ColumnKind,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRowsRequest {
    pub path: String,
    #[serde(default)]
    pub sheet_name: Option<String>,
    pub columns: Vec<ExportRowColumn>,
    /// Cell text per row; `None` is NULL.
    pub rows: Vec<Vec<Option<String>>>,
    #[serde(default = "default_true")]
    pub include_header: bool,
}

/// Write an in-memory result set to a workbook.
///
/// Used for query results, which exist only in the console and cannot be read
/// back from a table. The values arrive as text with the column's kind, so a
/// number is still written as a numeric cell.
pub async fn export_rows(request: &ExportRowsRequest) -> AppResult<ExportOutcome> {
    let started = Instant::now();
    if request.columns.is_empty() {
        return Err(AppError::msg("请至少选择一列"));
    }
    let name = sheet_name(request.sheet_name.as_deref(), "查询结果");

    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet
        .set_name(&name)
        .map_err(|error| AppError::msg(format!("工作表名 {name} 不可用：{error}")))?;

    let mut row: u32 = 0;
    if request.include_header {
        for (position, column) in request.columns.iter().enumerate() {
            let header = column
                .header
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or(&column.name);
            sheet
                .write_string(row, position as u16, header)
                .map_err(|error| AppError::msg(format!("写入表头失败：{error}")))?;
        }
        row += 1;
    }

    for values in &request.rows {
        for (position, column) in request.columns.iter().enumerate() {
            let cell = match values.get(position).and_then(|value| value.as_deref()) {
                None => Cell::Empty,
                Some(text) => cell_from_text(text, column.kind),
            };
            let column_index = position as u16;
            let result = match cell {
                Cell::Empty => Ok(()),
                Cell::Text(text) => sheet.write_string(row, column_index, &text).map(|_| ()),
                Cell::Number(number) => sheet.write_number(row, column_index, number).map(|_| ()),
                Cell::Boolean(flag) => sheet.write_boolean(row, column_index, flag).map(|_| ()),
            };
            result.map_err(|error| {
                AppError::msg(format!("写入第 {} 行失败：{error}", row + 1))
            })?;
        }
        row += 1;
    }

    let path = request.path.clone();
    let save_path = path.clone();
    tokio::task::spawn_blocking(move || workbook.save(&save_path))
        .await
        .map_err(|error| AppError::msg(format!("保存失败：{error}")))?
        .map_err(|error| AppError::msg(format!("保存 {path} 失败：{error}")))?;

    Ok(ExportOutcome {
        rows: request.rows.len() as u64,
        columns: request.columns.len(),
        path,
        sheet: name,
        truncated: false,
        cancelled: false,
        elapsed_ms: started.elapsed().as_millis() as u64,
        warnings: Vec::new(),
    })
}

/// Rebuild a cell from the console's text plus the column's kind.
///
/// A value that does not parse as its declared kind is written as text rather
/// than dropped: showing it is always better than showing nothing.
fn cell_from_text(text: &str, kind: ColumnKind) -> Cell {
    match kind {
        ColumnKind::Int | ColumnKind::Uint => match text.parse::<i64>() {
            Ok(number) => Cell::Number(number as f64),
            Err(_) => match text.parse::<u64>() {
                Ok(number) => Cell::Number(number as f64),
                Err(_) => Cell::Text(text.to_string()),
            },
        },
        ColumnKind::Float | ColumnKind::Decimal => match text.parse::<f64>() {
            Ok(number) if number.is_finite() => Cell::Number(number),
            _ => Cell::Text(text.to_string()),
        },
        ColumnKind::Bool => match text {
            "1" | "true" | "TRUE" | "t" => Cell::Boolean(true),
            "0" | "false" | "FALSE" | "f" => Cell::Boolean(false),
            _ => Cell::Text(text.to_string()),
        },
        _ => Cell::Text(text.to_string()),
    }
}

/// Map the requested column names onto positions in the result set.
fn resolve_query_columns(
    metas: &[crate::db::DescribedColumn],
    columns: &[ExportColumn],
) -> AppResult<Vec<(usize, String)>> {
    if columns.is_empty() {
        return Ok(metas
            .iter()
            .enumerate()
            .map(|(index, meta)| (index, meta.name.clone()))
            .collect());
    }
    let mut resolved = Vec::with_capacity(columns.len());
    for column in columns {
        // The caller usually passes the result metadata's own spelling, but
        // tolerating quotes and case differences costs nothing and avoids an
        // export failing over how a name was written.
        let wanted = column.column.trim().trim_matches(['"', '`', '[', ']']);
        let index = metas
            .iter()
            .position(|meta| meta.name.eq_ignore_ascii_case(wanted))
            .or_else(|| metas.iter().position(|meta| meta.name == column.column))
            .ok_or_else(|| AppError::msg(format!("查询结果里没有 {} 这一列", column.column)))?;
        let header = column
            .header
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(&column.column)
            .to_string();
        resolved.push((index, header));
    }
    Ok(resolved)
}

/// Export the result of a query, streaming straight into the workbook.
///
/// The console only keeps the first few thousand rows, so the export does not
/// use what is on screen: it re-runs the query in pages and writes each page as
/// it arrives. That is the only way an export can be larger than the console's
/// own limit, and it is also why the file reflects the data as of the export
/// rather than as of the last run.
pub async fn export_query(
    pool: &Pool,
    namespace: &str,
    sql: &str,
    columns: &[ExportColumn],
    options: &ExportOptions,
    path: &str,
    reporter: &ProgressReporter,
) -> AppResult<ExportOutcome> {
    use crate::db::{decode_mysql_row_lenient, decode_pg_row_lenient, describe_mysql, describe_pg};

    let started = Instant::now();
    let inner = sql.trim().trim_end_matches(';');
    if inner.is_empty() {
        return Err(AppError::msg("没有可导出的语句"));
    }
    let cap = options.max_rows.unwrap_or(MAX_ROWS).min(MAX_ROWS);
    if cap == 0 {
        return Err(AppError::msg("导出行数上限为 0"));
    }
    let requested = sheet_name(options.sheet_name.as_deref(), "查询结果");
    let page_size: usize = 1_000;

    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet
        .set_name(&requested)
        .map_err(|error| AppError::msg(format!("工作表名 {requested} 不可用：{error}")))?;

    let mut written: u64 = 0;
    let mut row_index: u32 = 0;
    let mut truncated = false;
    let mut cancelled = false;
    let mut warnings: Vec<String> = Vec::new();

    // The two engines need their own decode path, and a reborrowed connection
    // cannot satisfy sqlx's `Executor` bounds across an await, so the paging
    // loop is written once and instantiated per engine — the same shape the
    // console uses.
    macro_rules! pump {
        ($tx:expr, $describe:ident, $decode:ident) => {{
            let mut offset: u64 = 0;
            let mut kinds: Vec<ColumnKind> = Vec::new();
            let mut resolved: Vec<(usize, String)> = Vec::new();
            let mut head_written = false;
            loop {
                if reporter.is_cancelled() {
                    cancelled = true;
                    break;
                }
                let remaining = cap.saturating_sub(written);
                if remaining == 0 {
                    truncated = true;
                    break;
                }
                let limit = (remaining as usize).min(page_size).max(1);
                let mut wrapped =
                    format!("SELECT * FROM ({inner}) AS datasync_export LIMIT {limit}");
                if offset > 0 {
                    wrapped.push_str(&format!(" OFFSET {offset}"));
                }
                let rows = match $tx.fetch_all(sqlx::query(&wrapped)).await {
                    Ok(rows) => rows,
                    Err(error) => {
                        return Err(AppError::msg(format!("读取查询结果失败：{error}")))
                    }
                };
                if rows.is_empty() {
                    break;
                }
                if !head_written {
                    // Column types come from the result metadata, never from the
                    // values themselves.
                    let metas = $describe(rows[0].columns());
                    kinds = metas.iter().map(|meta| meta.kind).collect();
                    resolved = resolve_query_columns(&metas, columns)?;
                    if options.include_header {
                        for (position, (_, header)) in resolved.iter().enumerate() {
                            sheet
                                .write_string(0, position as u16, header)
                                .map_err(|error| {
                                    AppError::msg(format!("写入表头失败：{error}"))
                                })?;
                        }
                        row_index = 1;
                    }
                    head_written = true;
                }
                let fetched = rows.len();
                for row in rows.iter() {
                    let values = $decode(row, &kinds);
                    for (position, (index, _)) in resolved.iter().enumerate() {
                        let cell = values.get(*index).map(cell_of).unwrap_or(Cell::Empty);
                        let result = match cell {
                            Cell::Empty => Ok(()),
                            Cell::Text(text) => {
                                sheet.write_string(row_index, position as u16, &text).map(|_| ())
                            }
                            Cell::Number(number) => sheet
                                .write_number(row_index, position as u16, number)
                                .map(|_| ()),
                            Cell::Boolean(flag) => sheet
                                .write_boolean(row_index, position as u16, flag)
                                .map(|_| ()),
                        };
                        result.map_err(|error| {
                            AppError::msg(format!("写入第 {} 行失败：{error}", row_index + 1))
                        })?;
                    }
                    row_index += 1;
                    written += 1;
                    if written >= cap {
                        truncated = true;
                        break;
                    }
                }
                if truncated {
                    break;
                }
                offset += fetched as u64;
                reporter.tick(
                    "export",
                    "导出查询结果",
                    written,
                    None,
                    format!("已写入 {written} 行"),
                );
                if fetched < limit {
                    break;
                }
            }
        }};
    }

    match pool {
        Pool::MySql(pool) => {
            let mut tx = pool.begin().await?;
            if !namespace.trim().is_empty() {
                let use_db = format!("USE {}", Dialect::Mysql.quote(namespace));
                tx.execute(sqlx::raw_sql(&use_db))
                    .await
                    .map_err(|error| format!("切换到 {namespace} 失败: {error}"))
                    .map_err(AppError::msg)?;
            }
            pump!(tx, describe_mysql, decode_mysql_row_lenient);
            // Read-only: nothing here should be committed.
            let _ = tx.rollback().await;
        }
        Pool::Postgres(pool) => {
            let mut tx = pool.begin().await?;
            if !namespace.trim().is_empty() {
                let path_sql = format!(
                    "SET LOCAL search_path TO {}",
                    Dialect::Postgres.quote(namespace)
                );
                tx.execute(sqlx::raw_sql(&path_sql))
                    .await
                    .map_err(|error| format!("切换到 {namespace} 失败: {error}"))
                    .map_err(AppError::msg)?;
            }
            pump!(tx, describe_pg, decode_pg_row_lenient);
            let _ = tx.rollback().await;
        }
    }

    if written == 0 {
        warnings.push("查询没有返回任何行".to_string());
    }
    if truncated {
        warnings.push(format!("已达到 {cap} 行的上限，其余行未导出"));
    }

    let save_path = path.to_string();
    let path_text = path.to_string();
    tokio::task::spawn_blocking(move || workbook.save(&save_path))
        .await
        .map_err(|error| AppError::msg(format!("保存失败：{error}")))?
        .map_err(|error| AppError::msg(format!("保存 {path_text} 失败：{error}")))?;

    Ok(ExportOutcome {
        rows: written,
        columns: columns.len(),
        path: path.to_string(),
        sheet: requested,
        truncated,
        cancelled,
        elapsed_ms: started.elapsed().as_millis() as u64,
        warnings,
    })
}

/// Where pages of rows come from.
enum Pages<'a> {
    /// Keyset paging over the primary key: no `OFFSET`, so the cost does not
    /// grow with how far into the table the export has read.
    Stream(Box<TableStream<'a>>),
    /// Fallback for a table without a usable key.
    Offset { offset: u64 },
}

/// Read the table and write it to `path`.
pub async fn export(
    pool: &Pool,
    namespace: &str,
    structure: &TableStructure,
    columns: &[ExportColumn],
    options: &ExportOptions,
    path: &str,
    reporter: &ProgressReporter,
) -> AppResult<ExportOutcome> {
    let started = Instant::now();
    let dialect = pool.dialect();
    let selected = resolve_columns(structure, columns)?;
    let name = sheet_name(options.sheet_name.as_deref(), &structure.name);
    let limit = options
        .max_rows
        .filter(|value| *value > 0)
        .unwrap_or(MAX_ROWS)
        .min(MAX_ROWS);

    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet
        .set_name(&name)
        .map_err(|error| AppError::msg(format!("工作表名 {name} 不可用：{error}")))?;

    let mut row: u32 = 0;
    if options.include_header {
        for (position, (_, header)) in selected.iter().enumerate() {
            sheet
                .write_string(row, position as u16, header)
                .map_err(|error| AppError::msg(format!("写入表头失败：{error}")))?;
        }
        row += 1;
    }

    reporter.force("export", "导出数据", 0, None, "正在读取数据".to_string());

    // Keyset paging needs a NOT NULL key, which a primary key normally is.
    let usable_key = !structure.primary_key.is_empty()
        && structure
            .primary_key
            .iter()
            .all(|key| structure.column(key).map(|c| !c.nullable).unwrap_or(false));
    let mut page_source = if usable_key {
        let plan = StreamPlan::build(dialect, structure, &structure.primary_key)?;
        Pages::Stream(Box::new(TableStream::new(
            pool,
            dialect,
            namespace,
            &structure.name,
            plan,
            PAGE,
        )))
    } else {
        Pages::Offset { offset: 0 }
    };

    let mut written = 0u64;
    let mut truncated = false;

    loop {
        if reporter.is_cancelled() {
            return Err(AppError::msg("导出已取消"));
        }
        if written >= limit {
            truncated = true;
            break;
        }
        let want = (limit - written).min(PAGE as u64) as usize;
        let rows = match &mut page_source {
            Pages::Stream(stream) => {
                let mut rows = Vec::with_capacity(want);
                while rows.len() < want {
                    match stream.next_row().await? {
                        Some(row) => rows.push(row),
                        None => break,
                    }
                }
                rows
            }
            Pages::Offset { offset } => {
                let fetched = fetch_offset_page(
                    pool, dialect, namespace, structure, want, *offset,
                )
                .await?;
                *offset += fetched.len() as u64;
                fetched
            }
        };
        if rows.is_empty() {
            break;
        }
        for values in &rows {
            if written >= limit {
                truncated = true;
                break;
            }
            write_row(sheet, row, values, &selected, structure)?;
            row += 1;
            written += 1;
        }
        reporter.tick(
            "export",
            "导出数据",
            written,
            None,
            format!("已写入 {written} 行"),
        );
        if rows.len() < want {
            break;
        }
    }

    let mut warnings = Vec::new();
    if truncated {
        warnings.push(format!("已达到行数上限（{limit} 行），导出内容被截断"));
    }

    let destination = path.to_string();
    // File IO is blocking; keeping it off the async worker matters for a large
    // workbook.
    let save_path = destination.clone();
    tokio::task::spawn_blocking(move || workbook.save(&save_path))
        .await
        .map_err(|error| AppError::msg(format!("保存失败：{error}")))?
        .map_err(|error| AppError::msg(format!("保存 {destination} 失败：{error}")))?;

    reporter.finish("done", "导出完成", "导出结束");
    Ok(ExportOutcome {
        rows: written,
        columns: selected.len(),
        path: destination,
        sheet: name,
        truncated,
        cancelled: false,
        elapsed_ms: started.elapsed().as_millis() as u64,
        warnings,
    })
}

/// Write one row into the sheet.
fn write_row(
    sheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    values: &[Value],
    selected: &[(usize, String)],
    structure: &TableStructure,
) -> AppResult<()> {
    for (position, (index, _)) in selected.iter().enumerate() {
        let cell = values
            .get(*index)
            .map(cell_of)
            .unwrap_or(Cell::Empty);
        let column = position as u16;
        let result = match cell {
            // A blank cell, exactly like an empty spreadsheet cell.
            Cell::Empty => Ok(()),
            Cell::Text(text) => sheet.write_string(row, column, &text).map(|_| ()),
            Cell::Number(number) => sheet.write_number(row, column, number).map(|_| ()),
            Cell::Boolean(flag) => sheet.write_boolean(row, column, flag).map(|_| ()),
        };
        result.map_err(|error| {
            AppError::msg(format!(
                "写入 {} 的第 {} 行失败：{error}",
                structure.name,
                row + 1
            ))
        })?;
    }
    Ok(())
}

/// One `LIMIT`/`OFFSET` page, used when the table has no usable key.
async fn fetch_offset_page(
    pool: &Pool,
    dialect: Dialect,
    namespace: &str,
    structure: &TableStructure,
    limit: usize,
    offset: u64,
) -> AppResult<Vec<Vec<Value>>> {
    // Every column is read because values are addressed by position; only the
    // selected ones are written out.
    let select_list = crate::data::select_list(dialect, structure).join(", ");
    let mut sql = format!(
        "SELECT {select_list} FROM {}",
        dialect.qualified(Some(namespace), &structure.name)
    );
    if !structure.primary_key.is_empty() {
        let keys = structure
            .primary_key
            .iter()
            .map(|key| dialect.quote(key))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" ORDER BY {keys}"));
    }
    sql.push_str(&format!(" LIMIT {limit}"));
    if offset > 0 {
        sql.push_str(&format!(" OFFSET {offset}"));
    }
    let kinds: Vec<ColumnKind> = structure
        .columns
        .iter()
        .map(|column| column.kind())
        .collect();
    pool.fetch(&sql, &[], &kinds).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::model::ColumnInfo;

    fn column(name: &str, data_type: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            ordinal: 1,
            data_type: data_type.into(),
            full_type: data_type.into(),
            nullable: true,
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

    fn structure() -> TableStructure {
        TableStructure {
            name: "users".into(),
            comment: None,
            engine: None,
            charset: None,
            collation: None,
            primary_key: vec!["id".into()],
            columns: vec![
                column("id", "int"),
                column("name", "varchar"),
                column("amount", "decimal"),
            ],
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
        }
    }

    fn export_column(column: &str, header: Option<&str>) -> ExportColumn {
        ExportColumn {
            column: column.into(),
            header: header.map(str::to_string),
        }
    }

    #[test]
    fn headers_default_to_the_column_name_and_can_be_renamed() {
        let resolved = resolve_columns(
            &structure(),
            &[
                export_column("id", None),
                export_column("name", Some("  姓名  ")),
            ],
        )
        .unwrap();
        assert_eq!(resolved[0], (0, "id".to_string()));
        assert_eq!(resolved[1], (1, "姓名".to_string()), "表头应去掉空白");

        // An all-whitespace header falls back rather than producing a blank one.
        let resolved =
            resolve_columns(&structure(), &[export_column("name", Some("   "))]).unwrap();
        assert_eq!(resolved[0].1, "name");

        // Unknown columns are refused before anything is written.
        let error =
            resolve_columns(&structure(), &[export_column("nope", None)]).unwrap_err();
        assert!(error.to_string().contains("没有字段"), "{error}");
        assert!(resolve_columns(&structure(), &[]).is_err());
    }

    #[test]
    fn sheet_names_are_made_acceptable_to_excel() {
        assert_eq!(sheet_name(None, "users"), "users");
        assert_eq!(sheet_name(Some("  报表  "), "users"), "报表");
        // Characters Excel rejects.
        assert_eq!(sheet_name(Some("a/b:c*d?e[f]g\\h"), "t"), "a_b_c_d_e_f_g_h");
        // Empty and over-long names are repaired.
        assert_eq!(sheet_name(Some("   "), "users"), "users");
        assert_eq!(sheet_name(Some(""), "users"), "users");
        assert_eq!(
            sheet_name(Some(&"x".repeat(60)), "t").chars().count(),
            MAX_SHEET_NAME
        );
    }

    #[test]
    fn values_keep_their_kind_on_the_way_out() {
        assert!(matches!(cell_of(&Value::Int(42)), Cell::Number(n) if n == 42.0));
        assert!(matches!(cell_of(&Value::Float(1.5)), Cell::Number(n) if n == 1.5));
        assert!(matches!(cell_of(&Value::Bool(true)), Cell::Boolean(true)));
        assert!(matches!(cell_of(&Value::Null), Cell::Empty));
        assert!(matches!(cell_of(&Value::Str("hi".into())), Cell::Text(t) if t == "hi"));
        // Dates stay text, which the importer parses back.
        assert!(
            matches!(cell_of(&Value::Date("2024-03-05".into())), Cell::Text(t) if t == "2024-03-05")
        );
        // A non-finite float cannot be a numeric cell.
        assert!(matches!(cell_of(&Value::Float(f64::NAN)), Cell::Text(_)));
    }

    #[test]
    fn decimals_that_fit_a_double_become_numbers_and_the_rest_stay_text() {
        assert!(matches!(cell_of(&Value::Decimal("10.50".into())), Cell::Number(n) if n == 10.5));
        assert!(matches!(cell_of(&Value::Decimal("20".into())), Cell::Number(n) if n == 20.0));
        // More precision than a double holds: keeping the digits matters more
        // than being numeric.
        let long = "12345678901234567890.12345678901234567890";
        assert!(
            matches!(cell_of(&Value::Decimal(long.into())), Cell::Text(t) if t == long),
            "超出双精度的小数应保留原文而不是丢位数"
        );
    }
}
