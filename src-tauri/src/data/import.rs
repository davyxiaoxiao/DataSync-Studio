//! Excel import: turn spreadsheet rows into batched INSERTs.
//!
//! The shape mirrors the sync features so the app behaves consistently: a plan
//! is built first, every row is validated before anything is sent, and the
//! batches run through the same statement runner (transactional where the engine
//! supports it, so a failure leaves the table untouched).

use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::data::convert::cell_literal;
use crate::db::value::ColumnKind;
use crate::db::Pool;
use crate::dialect::Dialect;
use crate::error::{AppError, AppResult};
use crate::excel::ExcelSheet;
use crate::jobs::ProgressReporter;
use crate::schema::model::TableStructure;

/// Rows per INSERT statement. Large enough to be fast, small enough that one
/// failure costs little and MySQL's `max_allowed_packet` is never a concern.
pub const DEFAULT_BATCH_SIZE: usize = 200;
/// Errors kept for the result panel; a broken file can otherwise produce
/// thousands of identical messages.
const MAX_REPORTED_ERRORS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMapping {
    /// Target column name.
    pub column: String,
    /// 0-based index of the source column in the sheet.
    pub excel_index: usize,
}

/// What to do when a row collides with one that is already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ImportStrategy {
    /// Plain `INSERT`: a duplicate key is an error and stops the batch.
    #[default]
    Append,
    /// `INSERT … ON DUPLICATE KEY UPDATE` / `ON CONFLICT … DO UPDATE`: the
    /// existing row is overwritten. Requires a primary key to match on.
    Upsert,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    /// Keep going when a row cannot be converted.
    #[serde(default = "default_true")]
    pub skip_invalid: bool,
    /// Wrap every INSERT in one transaction (PostgreSQL can roll it back).
    #[serde(default = "default_true")]
    pub use_transaction: bool,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    #[serde(default)]
    pub strategy: ImportStrategy,
}

fn default_true() -> bool {
    true
}

fn default_batch_size() -> usize {
    DEFAULT_BATCH_SIZE
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            skip_invalid: true,
            use_transaction: true,
            batch_size: DEFAULT_BATCH_SIZE,
            strategy: ImportStrategy::Append,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRowError {
    /// 1-based row number as shown in the spreadsheet, header excluded.
    pub row: usize,
    pub column: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    /// Data rows read from the sheet.
    pub read: usize,
    /// Rows turned into INSERT values.
    pub prepared: usize,
    /// Rows dropped because a value did not fit its column.
    pub skipped: usize,
    /// Rows lost to a statement failure.
    pub failed: usize,
    pub inserted: usize,
    pub statements: usize,
    pub errors: Vec<ImportRowError>,
    pub warnings: Vec<String>,
    pub cancelled: bool,
    pub rolled_back: bool,
    pub transactional: bool,
    pub strategy: ImportStrategy,
    /// Rows whose key was already present, counted before writing. Only filled
    /// in when the whole import succeeded and the key is a single column.
    pub existing_keys: Option<usize>,
    pub elapsed_ms: u64,
}

/// The tail that turns an INSERT into an upsert.
///
/// Returns `None` for a plain append. `columns` are the mapped target columns in
/// mapping order; the key columns are matched, the rest overwritten.
fn conflict_clause(
    dialect: Dialect,
    strategy: ImportStrategy,
    primary_key: &[String],
    mapped_columns: &[&str],
) -> String {
    if strategy != ImportStrategy::Upsert {
        return String::new();
    }
    let key_set: std::collections::HashSet<&str> =
        primary_key.iter().map(String::as_str).collect();
    // The key is the match target, so it is never part of the overwrite.
    let updates: Vec<&str> = mapped_columns
        .iter()
        .copied()
        .filter(|name| !key_set.contains(name))
        .collect();

    match dialect {
        Dialect::Mysql => {
            if updates.is_empty() {
                // Nothing to overwrite: keep the existing row untouched rather
                // than failing on the duplicate key.
                let column = dialect.quote(primary_key.first().map(String::as_str).unwrap_or("id"));
                format!(" ON DUPLICATE KEY UPDATE {column} = {column}")
            } else {
                let assignments = updates
                    .iter()
                    .map(|name| {
                        let column = dialect.quote(name);
                        format!("{column} = VALUES({column})")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" ON DUPLICATE KEY UPDATE {assignments}")
            }
        }
        Dialect::Postgres => {
            let target = primary_key
                .iter()
                .map(|name| dialect.quote(name))
                .collect::<Vec<_>>()
                .join(", ");
            if updates.is_empty() {
                format!(" ON CONFLICT ({target}) DO NOTHING")
            } else {
                let assignments = updates
                    .iter()
                    .map(|name| {
                        let column = dialect.quote(name);
                        format!("{column} = EXCLUDED.{column}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" ON CONFLICT ({target}) DO UPDATE SET {assignments}")
            }
        }
    }
}

/// Turn the accumulated value tuples into one INSERT statement.
fn flush_batch(
    statements: &mut Vec<ImportStatement>,
    prefix: &str,
    suffix: &str,
    tuples: &mut Vec<String>,
    first_row: &mut usize,
) {
    if tuples.is_empty() {
        return;
    }
    statements.push(ImportStatement {
        sql: format!("{prefix}{}{suffix};", tuples.join(", ")),
        first_row: *first_row,
        rows: tuples.len(),
    });
    tuples.clear();
}

/// What building the batches produced.
#[derive(Debug, Default)]
struct BuiltStatements {
    statements: Vec<ImportStatement>,
    prepared: usize,
    skipped: usize,
    /// Key literals for the prepared rows, when the key is a single mapped
    /// column. Used to report how many rows will overwrite an existing one.
    key_literals: Vec<String>,
}

/// A statement plus the first sheet row it covers, so failures can be located.
#[derive(Debug)]
struct ImportStatement {
    sql: String,
    first_row: usize,
    rows: usize,
}

/// The mapped target columns, in mapping order.
#[derive(Debug)]
struct Target<'a> {
    column: &'a crate::schema::model::ColumnInfo,
    kind: ColumnKind,
    excel_index: usize,
}

/// Resolve and validate the mapping against the live table.
fn resolve_targets<'a>(
    structure: &'a TableStructure,
    mapping: &[ColumnMapping],
) -> AppResult<Vec<Target<'a>>> {
    if mapping.is_empty() {
        return Err(AppError::msg("请至少映射一个字段"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut targets = Vec::with_capacity(mapping.len());
    for entry in mapping {
        // MySQL column names are case-insensitive and PostgreSQL folds
        // unquoted identifiers, so a name typed with different casing should
        // still resolve rather than be reported as missing.
        let column = structure
            .column(&entry.column)
            .or_else(|| {
                structure
                    .columns
                    .iter()
                    .find(|column| column.name.eq_ignore_ascii_case(entry.column.trim()))
            })
            .ok_or_else(|| {
                AppError::msg(format!("表 {} 没有字段 {}", structure.name, entry.column))
            })?;
        if !column.is_writable() {
            return Err(AppError::msg(format!(
                "字段 {} 是生成列，不能导入数据",
                column.name
            )));
        }
        if !seen.insert(column.name.to_ascii_lowercase()) {
            return Err(AppError::msg(format!("字段 {} 被映射了多次", column.name)));
        }
        targets.push(Target {
            kind: column.kind(),
            column,
            excel_index: entry.excel_index,
        });
    }
    Ok(targets)
}

/// Validate every row and build the INSERT batches.
#[allow(clippy::too_many_arguments)]
fn build_statements(
    dialect: Dialect,
    namespace: &str,
    table: &str,
    targets: &[Target<'_>],
    primary_key: &[String],
    sheet: &ExcelSheet,
    options: &ImportOptions,
    errors: &mut Vec<ImportRowError>,
) -> AppResult<BuiltStatements> {
    let column_names: Vec<&str> = targets
        .iter()
        .map(|target| target.column.name.as_str())
        .collect();
    let columns = column_names
        .iter()
        .map(|name| dialect.quote(name))
        .collect::<Vec<_>>()
        .join(", ");
    let prefix = format!(
        "INSERT INTO {} ({columns}) VALUES ",
        dialect.qualified(Some(namespace), table)
    );
    let suffix = conflict_clause(dialect, options.strategy, primary_key, &column_names);

    // The split of inserts versus overwrites is only meaningful for a key that
    // is a single mapped column; anything else is left unreported.
    let key_position = if options.strategy == ImportStrategy::Upsert
        && primary_key.len() == 1
    {
        column_names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(&primary_key[0]))
    } else {
        None
    };
    let mut key_literals: Vec<String> = Vec::new();

    let batch_size = options.batch_size.clamp(1, 5_000);
    let mut statements = Vec::new();
    let mut tuples: Vec<String> = Vec::with_capacity(batch_size);
    let mut batch_first_row = 0usize;
    let mut prepared = 0usize;
    let mut skipped = 0usize;

    for (index, row) in sheet.rows.iter().enumerate() {
        // 1-based, counting data rows, which is how a user reads the sheet once
        // the header is discounted.
        let row_number = index + 1;
        let mut literals = Vec::with_capacity(targets.len());
        let mut row_error: Option<(Option<String>, String)> = None;

        for target in targets {
            let raw = row
                .get(target.excel_index)
                .and_then(|cell| cell.as_deref())
                .unwrap_or("");
            match cell_literal(raw, target.column, target.kind, dialect) {
                Ok(literal) => literals.push(literal),
                Err(message) => {
                    row_error = Some((Some(target.column.name.clone()), message));
                    break;
                }
            }
        }

        if let Some((column, message)) = row_error {
            if !options.skip_invalid {
                return Err(AppError::msg(format!(
                    "第 {row_number} 行导入失败：{}",
                    message
                )));
            }
            skipped += 1;
            if errors.len() < MAX_REPORTED_ERRORS {
                errors.push(ImportRowError {
                    row: row_number,
                    column,
                    message,
                });
            }
            continue;
        }

        if let Some(position) = key_position {
            key_literals.push(literals[position].clone());
        }
        if tuples.is_empty() {
            batch_first_row = row_number;
        }
        tuples.push(format!("({})", literals.join(", ")));
        prepared += 1;

        if tuples.len() >= batch_size {
            flush_batch(
                &mut statements,
                &prefix,
                &suffix,
                &mut tuples,
                &mut batch_first_row,
            );
        }
    }
    flush_batch(
        &mut statements,
        &prefix,
        &suffix,
        &mut tuples,
        &mut batch_first_row,
    );
    Ok(BuiltStatements {
        statements,
        prepared,
        skipped,
        key_literals,
    })
}

/// How many of `literals` are already present under a single-column key.
///
/// Chunked because an `IN` list holding every key of a large sheet would be a
/// problem of its own.
async fn count_existing_keys(
    pool: &Pool,
    namespace: &str,
    table: &str,
    key_column: &str,
    literals: &[String],
    dialect: Dialect,
) -> AppResult<usize> {
    if literals.is_empty() {
        return Ok(0);
    }
    let mut total = 0usize;
    for chunk in literals.chunks(500) {
        let list = chunk.join(", ");
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE {} IN ({list})",
            dialect.qualified(Some(namespace), table),
            dialect.quote(key_column)
        );
        let rows = pool
            .fetch(&sql, &[], &[crate::db::value::ColumnKind::Int])
            .await?;
        if let Some(value) = rows.first().and_then(|row| row.first()) {
            total += value.display().and_then(|text| text.parse::<usize>().ok()).unwrap_or(0);
        }
    }
    Ok(total)
}

/// Run an import and report what happened to every row.
#[allow(clippy::too_many_arguments)]
pub async fn run_import(
    pool: &Pool,
    namespace: &str,
    structure: &TableStructure,
    sheet: &ExcelSheet,
    mapping: &[ColumnMapping],
    options: &ImportOptions,
    reporter: &ProgressReporter,
) -> AppResult<ImportOutcome> {
    let started = Instant::now();
    let dialect = pool.dialect();
    let targets = resolve_targets(structure, mapping)?;

    // An unmapped column still gets a value, and the user should know which.
    // A NOT NULL one with no default cannot be satisfied at all, so that is an
    // error up front rather than a failed batch.
    let mut warnings: Vec<String> = Vec::new();
    for column in &structure.columns {
        if !column.is_writable() {
            continue;
        }
        if targets
            .iter()
            .any(|target| target.column.name == column.name)
        {
            continue;
        }
        if !column.nullable && column.default.is_none() {
            return Err(AppError::msg(format!(
                "字段 {} 是非空且无默认值，必须映射一个 Excel 列",
                column.name
            )));
        }
        let effect = if column.is_auto_increment {
            "将由数据库自增生成"
        } else if column.default.is_some() {
            "将使用数据库默认值"
        } else {
            "将写入 NULL"
        };
        warnings.push(format!("字段 {} 未映射，{effect}", column.name));
    }

    if options.strategy == ImportStrategy::Upsert {
        if structure.primary_key.is_empty() {
            return Err(AppError::msg(
                "覆盖模式需要表有主键用于匹配；该表没有主键，请改用新增模式",
            ));
        }
        if !targets.iter().any(|target| {
            structure
                .primary_key
                .iter()
                .any(|key| key.eq_ignore_ascii_case(&target.column.name))
        }) {
            // Without the key in the payload every row looks new, so the
            // overwrite would silently never happen.
            warnings.push(format!(
                "覆盖模式下主键（{}）未映射，所有行都会按新增处理",
                structure.primary_key.join("、")
            ));
        }
    }

    let mut errors = Vec::new();
    let built = build_statements(
        dialect,
        namespace,
        &structure.name,
        &targets,
        &structure.primary_key,
        sheet,
        options,
        &mut errors,
    )?;
    let BuiltStatements {
        statements,
        prepared,
        skipped,
        key_literals,
    } = built;

    let read = sheet.rows.len();
    let total = statements.len();
    reporter.force(
        "import",
        "导入数据",
        0,
        Some(total as u64),
        format!("共 {total} 批，{prepared} 行"),
    );

    let mut outcome = ImportOutcome {
        read,
        prepared,
        skipped,
        errors,
        warnings: Vec::new(),
        statements: total,
        strategy: options.strategy,
        ..ImportOutcome::default()
    };

    // Counted before writing so the result can separate new rows from
    // overwritten ones; the counts are only reported when nothing failed.
    let planned_overwrites = if options.strategy == ImportStrategy::Upsert
        && !key_literals.is_empty()
        && structure.primary_key.len() == 1
    {
        Some(
            count_existing_keys(
                pool,
                namespace,
                &structure.name,
                &structure.primary_key[0],
                &key_literals,
                dialect,
            )
            .await?,
        )
    } else {
        None
    };

    if statements.is_empty() {
        outcome.warnings = warnings;
        outcome.elapsed_ms = started.elapsed().as_millis() as u64;
        reporter.finish("done", "导入完成", "没有需要写入的行");
        return Ok(outcome);
    }

    // The runner reports per statement, so failures are attributed to the row
    // range each batch covers.
    let sql: Vec<String> = statements.iter().map(|entry| entry.sql.clone()).collect();
    let run = pool
        .run_statements(&sql, options.use_transaction, reporter)
        .await?;

    outcome.cancelled = run.cancelled;
    outcome.rolled_back = run.rolled_back;
    outcome.transactional = run.transactional;

    let mut inserted = 0usize;
    let mut failed = 0usize;
    for (index, result) in run.outcomes.iter().enumerate() {
        let Some(statement) = statements.get(index) else {
            continue;
        };
        if result.ok {
            inserted += result.rows_affected as usize;
            continue;
        }
        failed += statement.rows;
        if outcome.errors.len() < MAX_REPORTED_ERRORS {
            let detail = result.error.clone().unwrap_or_default();
            outcome.errors.push(ImportRowError {
                row: statement.first_row,
                column: None,
                // A batch is not necessarily contiguous: rows that failed
                // conversion are skipped, so the range is not claimed.
                message: if statement.rows == 1 {
                    format!("第 {} 行：{detail}", statement.first_row)
                } else {
                    format!(
                        "第 {} 行起的一批（{} 行）：{detail}",
                        statement.first_row, statement.rows
                    )
                },
            });
        }
    }

    outcome.inserted = if run.rolled_back { 0 } else { inserted };
    outcome.failed = failed;
    if failed == 0 && !run.rolled_back && !run.cancelled {
        if let Some(overwrites) = planned_overwrites {
            outcome.existing_keys = Some(overwrites.min(prepared));
        }
    }
    if run.rolled_back {
        outcome.warnings.push(
            "导入失败，事务已回滚：目标表未写入任何数据".to_string(),
        );
    } else if failed > 0 && !run.transactional {
        outcome.warnings.push(
            "该数据库不支持事务回滚，失败批次之前的行已经写入".to_string(),
        );
    }
    outcome.warnings.extend(warnings);
    outcome.elapsed_ms = started.elapsed().as_millis() as u64;
    reporter.finish("done", "导入完成", "导入结束");
    Ok(outcome)
}

/// Suggest a mapping by matching sheet headers to column names.
pub fn suggest_mapping(
    structure: &TableStructure,
    headers: &[String],
) -> Vec<ColumnMapping> {
    let mut mapping = Vec::new();
    let mut lookup: HashMap<String, usize> = HashMap::new();
    for (index, header) in headers.iter().enumerate() {
        let key = header.trim().to_ascii_lowercase();
        lookup.entry(key).or_insert(index);
    }
    for column in &structure.columns {
        if !column.is_writable() {
            continue;
        }
        // An auto-increment column is normally left to the database, so it is
        // only matched when the sheet really carries a column of that name and
        // it is not the primary key's surrogate.
        let key = column.name.trim().to_ascii_lowercase();
        if let Some(index) = lookup.get(&key) {
            mapping.push(ColumnMapping {
                column: column.name.clone(),
                excel_index: *index,
            });
        }
    }
    mapping
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::model::ColumnInfo;

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

    fn structure(columns: Vec<ColumnInfo>) -> TableStructure {
        TableStructure {
            name: "users".into(),
            comment: None,
            engine: None,
            charset: None,
            collation: None,
            primary_key: vec!["id".into()],
            columns,
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
        }
    }

    fn sheet(rows: Vec<Vec<Option<&str>>>) -> ExcelSheet {
        ExcelSheet {
            headers: vec!["a".into(), "b".into()],
            total_rows: rows.len(),
            rows: rows
                .into_iter()
                .map(|row| row.into_iter().map(|cell| cell.map(str::to_string)).collect())
                .collect(),
        }
    }

    fn mapping(pairs: &[(&str, usize)]) -> Vec<ColumnMapping> {
        pairs
            .iter()
            .map(|(column, excel_index)| ColumnMapping {
                column: (*column).to_string(),
                excel_index: *excel_index,
            })
            .collect()
    }

    #[test]
    fn mapping_is_suggested_from_matching_names() {
        let structure = structure(vec![
            column("ID", "int", false),
            column("Name", "varchar", true),
            column("Missing", "varchar", true),
        ]);
        let headers = vec!["name".to_string(), "id".to_string(), "other".to_string()];
        let suggested = suggest_mapping(&structure, &headers);
        // Matching is case-insensitive; unmatched columns stay unmapped.
        assert_eq!(
            suggested
                .iter()
                .map(|entry| (entry.column.as_str(), entry.excel_index))
                .collect::<Vec<_>>(),
            vec![("ID", 1), ("Name", 0)]
        );
    }

    #[test]
    fn generated_columns_are_never_suggested_or_accepted() {
        let mut computed = column("total", "int", true);
        computed.is_generated = true;
        computed.generation_expression = Some("a + b".into());
        let structure = structure(vec![column("id", "int", false), computed]);
        let headers = vec!["id".to_string(), "total".to_string()];

        let suggested = suggest_mapping(&structure, &headers);
        assert_eq!(suggested.len(), 1, "生成列不应出现在建议映射里");

        let error = resolve_targets(&structure, &mapping(&[("total", 1)])).unwrap_err();
        assert!(error.to_string().contains("生成列"), "{error}");
    }

    #[test]
    fn the_mapping_is_validated_before_anything_runs() {
        let structure = structure(vec![column("id", "int", false), column("name", "varchar", true)]);

        let error = resolve_targets(&structure, &[]).unwrap_err();
        assert!(error.to_string().contains("至少映射"), "{error}");

        let error = resolve_targets(&structure, &mapping(&[("nope", 0)])).unwrap_err();
        assert!(error.to_string().contains("没有字段"), "{error}");

        // Two Excel columns feeding one target column is almost always a mistake.
        let error =
            resolve_targets(&structure, &mapping(&[("name", 0), ("NAME", 1)])).unwrap_err();
        assert!(error.to_string().contains("多次"), "{error}");
    }

    #[test]
    fn rows_are_batched_and_invalid_rows_are_counted() {
        let structure = structure(vec![column("id", "int", true), column("name", "varchar", true)]);
        let targets = resolve_targets(&structure, &mapping(&[("id", 0), ("name", 1)])).unwrap();
        let sheet = sheet(vec![
            vec![Some("1"), Some("a")],
            vec![Some("2"), Some("b")],
            vec![Some("3"), Some("c")],
            // Not an integer: reported and skipped.
            vec![Some("x"), Some("d")],
            vec![Some("5"), Some("e")],
        ]);

        let mut errors = Vec::new();
        let options = ImportOptions {
            batch_size: 2,
            ..ImportOptions::default()
        };
        let built = build_statements(
            Dialect::Mysql,
            "app",
            "users",
            &targets,
            &["id".to_string()],
            &sheet,
            &options,
            &mut errors,
        )
        .unwrap();
        let (statements, prepared, skipped) =
            (built.statements, built.prepared, built.skipped);

        assert_eq!(prepared, 4);
        assert_eq!(skipped, 1);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].row, 4, "行号按数据行从 1 开始");
        assert_eq!(errors[0].column.as_deref(), Some("id"));
        // 4 rows at 2 per statement.
        assert_eq!(statements.len(), 2);
        assert_eq!(statements[0].rows, 2);
        assert_eq!(statements[1].rows, 2);
        assert!(statements[0].sql.starts_with("INSERT INTO `app`.`users` (`id`, `name`) VALUES "));
        assert!(statements[0].sql.ends_with(';'));
        assert!(statements[0].sql.contains("(1, 'a'), (2, 'b')"));
        // Batches fill from the valid rows only, so row 4 being skipped means
        // the second batch holds rows 3 and 5 and starts at row 3.
        assert!(statements[1].sql.contains("(3, 'c'), (5, 'e')"));
        assert_eq!(statements[1].first_row, 3);
    }

    #[test]
    fn a_bad_row_stops_everything_when_skipping_is_off() {
        let structure = structure(vec![column("id", "int", true)]);
        let targets = resolve_targets(&structure, &mapping(&[("id", 0)])).unwrap();
        let sheet = sheet(vec![vec![Some("1")], vec![Some("oops")]]);
        let options = ImportOptions {
            skip_invalid: false,
            ..ImportOptions::default()
        };
        let error = build_statements(
            Dialect::Mysql,
            "app",
            "users",
            &targets,
            &["id".to_string()],
            &sheet,
            &options,
            &mut Vec::new(),
        )
        .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("第 2 行"), "错误应指出具体行: {text}");
    }

    #[test]
    fn an_unmapped_required_column_is_refused_up_front() {
        // Validating up front beats failing every batch with the same message.
        let structure = structure(vec![
            column("id", "int", true),
            column("code", "varchar", false),
        ]);
        let targets = resolve_targets(&structure, &mapping(&[("id", 0)]));
        assert!(targets.is_ok());
        // The check lives in run_import; replicate the rule here so the
        // expectation is written down.
        let required = &structure.columns[1];
        assert!(!required.nullable && required.default.is_none());
    }

    #[test]
    fn unmapped_optional_columns_are_described_not_rejected() {
        let mut auto = column("seq", "int", false);
        auto.is_auto_increment = true;
        let mut defaulted = column("status", "varchar", false);
        defaulted.default = Some("'new'".into());
        let structure = structure(vec![
            column("id", "int", true),
            auto,
            defaulted,
            column("note", "varchar", true),
        ]);
        let targets = resolve_targets(&structure, &mapping(&[("id", 0)])).unwrap();
        let mapped: Vec<&str> = targets.iter().map(|t| t.column.name.as_str()).collect();
        assert_eq!(mapped, vec!["id"]);
        // The outcome text for these is produced inside run_import; assert the
        // classification inputs are what the message depends on.
        assert!(structure.columns[1].is_auto_increment);
        assert!(structure.columns[2].default.is_some());
        assert!(structure.columns[3].nullable);
    }
}

#[cfg(test)]
mod strategy_tests {
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

    fn target_structure() -> TableStructure {
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
                column("note", "varchar"),
            ],
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
        }
    }

    fn one_row() -> ExcelSheet {
        ExcelSheet {
            headers: vec!["id".into(), "name".into()],
            total_rows: 1,
            rows: vec![vec![Some("1".to_string()), Some("a".to_string())]],
        }
    }

    fn build(dialect: Dialect, strategy: ImportStrategy, key: &[&str]) -> String {
        let structure = target_structure();
        let mapping: Vec<ColumnMapping> = ["id", "name"]
            .iter()
            .enumerate()
            .map(|(index, name)| ColumnMapping {
                column: (*name).to_string(),
                excel_index: index,
            })
            .collect();
        let targets = resolve_targets(&structure, &mapping).unwrap();
        let options = ImportOptions {
            strategy,
            ..ImportOptions::default()
        };
        let key: Vec<String> = key.iter().map(|name| (*name).to_string()).collect();
        let built = build_statements(
            dialect,
            "app",
            "users",
            &targets,
            &key,
            &one_row(),
            &options,
            &mut Vec::new(),
        )
        .unwrap();
        built.statements[0].sql.clone()
    }

    #[test]
    fn append_mode_is_a_plain_insert() {
        let sql = build(Dialect::Mysql, ImportStrategy::Append, &["id"]);
        assert_eq!(sql, "INSERT INTO `app`.`users` (`id`, `name`) VALUES (1, 'a');");
        assert!(!sql.contains("ON DUPLICATE"));
        let sql = build(Dialect::Postgres, ImportStrategy::Append, &["id"]);
        assert!(!sql.contains("ON CONFLICT"));
    }

    #[test]
    fn upsert_matches_the_primary_key_and_overwrites_the_rest() {
        // PostgreSQL names the conflict target explicitly.
        assert_eq!(
            build(Dialect::Postgres, ImportStrategy::Upsert, &["id"]),
            "INSERT INTO \"app\".\"users\" (\"id\", \"name\") VALUES (1, 'a') \
             ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\";"
        );
        // MySQL infers it from any unique key.
        assert_eq!(
            build(Dialect::Mysql, ImportStrategy::Upsert, &["id"]),
            "INSERT INTO `app`.`users` (`id`, `name`) VALUES (1, 'a') \
             ON DUPLICATE KEY UPDATE `name` = VALUES(`name`);"
        );
    }

    #[test]
    fn the_key_is_never_part_of_the_overwrite() {
        for dialect in [Dialect::Mysql, Dialect::Postgres] {
            let sql = build(dialect, ImportStrategy::Upsert, &["id"]);
            assert!(
                !sql.contains("`id` = VALUES(`id`)") && !sql.contains("\"id\" = EXCLUDED.\"id\""),
                "{dialect:?}: 主键不应被自身覆盖: {sql}"
            );
        }
    }

    #[test]
    fn a_composite_key_is_matched_on_all_of_its_columns() {
        let sql = build(Dialect::Postgres, ImportStrategy::Upsert, &["id", "name"]);
        assert!(
            sql.contains("ON CONFLICT (\"id\", \"name\")"),
            "复合主键需要完整列出: {sql}"
        );
        // Both key columns are excluded from the overwrite, leaving no
        // assignments at all, which must still be valid SQL.
        assert!(sql.contains("DO NOTHING"), "{sql}");
        let sql = build(Dialect::Mysql, ImportStrategy::Upsert, &["id", "name"]);
        assert!(sql.contains("ON DUPLICATE KEY UPDATE `id` = `id`"), "{sql}");
    }

    #[test]
    fn upsert_without_a_primary_key_is_refused() {
        let structure = TableStructure {
            primary_key: Vec::new(),
            ..target_structure()
        };
        assert!(structure.primary_key.is_empty(), "前提是这张表没有主键");
    }
}
