use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::State;

use std::cmp::Ordering;

use crate::data::stream::{compare_key_tuples, StreamPlan, TableStream};
use crate::data::{
    build_delete_statements, build_insert_statements, build_update_statements, column_index,
    preview_deletes, preview_inserts, preview_updates, read_rows, ColumnPair, DataDiffResult,
    DataDiffSummary, ReadPlan, RowChange, TableEndpoint, PREVIEW_ROWS, SQL_PREVIEW_LIMIT,
};
use crate::data::plan::DataPlan;
use crate::db::value::{ColumnMeta, Value};
use crate::data::view::{
    build_cell_update, build_grid_query, CellEdit, FilterGroup, RowKeyValue, SortSpec,
};
use crate::data::import::{
    run_import, suggest_mapping, ColumnMapping, ImportOptions, ImportOutcome,
};
use crate::db::{open_pool, Pool, StatementOutcome, StatementRun};
use crate::dialect::Dialect;
use crate::error::{AppError, AppResult};
use crate::excel::{self, ExcelSheetPreview};
use crate::excel_export::{self, ExportColumn, ExportOptions, ExportOutcome};
use crate::queries::{self, SavedQuery};
use crate::jobs::ProgressReporter;
use crate::schema::model::foreign_key_covered_by;
use crate::schema::ddl::{
    generate, normalize_designed_columns, plan_drop_tables, rename_column_ddl,
    create_table_ddl, merge_mysql_index_statements, rename_table_ddl, validate_foreign_keys,
    validate_indexes, DdlStatement, SyncSelection,
};
use crate::schema::diff::{compare_everything, ChangeKind, CompareOptions, SchemaDiff};
use crate::schema::introspect;
use crate::schema::model::{
    ColumnInfo, ForeignKeyInfo, IndexInfo, NamespaceInfo, TableStructure, TableSummary,
};
use crate::state::{AppState, SchemaCompare};
use crate::store::ConnectionConfig;

const DEFAULT_BATCH_SIZE: usize = 200;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

async fn pool_for(state: &AppState, connection_id: &str) -> AppResult<Arc<Pool>> {
    if let Some(pool) = state.registry.get(connection_id) {
        return Ok(pool);
    }
    let config = state
        .store
        .get(connection_id)
        .ok_or_else(|| AppError::msg("连接不存在，请重新选择"))?;
    let password = state.store.resolve_password(&config);
    let pool = Arc::new(open_pool(&config, password.as_deref()).await?);
    state.registry.put(connection_id.to_string(), pool.clone());
    Ok(pool)
}

// ---------------------------------------------------------------------------
// Connections
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_connections(state: State<'_, AppState>) -> AppResult<Vec<ConnectionConfig>> {
    Ok(state.store.list())
}

#[tauri::command]
pub fn save_connection(
    state: State<'_, AppState>,
    config: ConnectionConfig,
) -> AppResult<ConnectionConfig> {
    let saved = state.store.upsert(config)?;
    state.registry.drop_pool(&saved.id);
    Ok(saved)
}

#[tauri::command]
pub async fn delete_connection(state: State<'_, AppState>, connection_id: String) -> AppResult<()> {
    if let Some(pool) = state.registry.get(&connection_id) {
        pool.close().await;
    }
    state.registry.drop_pool(&connection_id);
    state.store.remove(&connection_id)
}

#[tauri::command]
pub fn reorder_connections(state: State<'_, AppState>, ids: Vec<String>) -> AppResult<()> {
    state.store.reorder(&ids)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestConnectionResult {
    pub ok: bool,
    pub server_version: Option<String>,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn test_connection(
    state: State<'_, AppState>,
    config: ConnectionConfig,
) -> AppResult<TestConnectionResult> {
    let explicit = config
        .password
        .clone()
        .filter(|p| !p.is_empty())
        .or_else(|| state.store.resolve_password(&config));
    let started = Instant::now();
    match open_pool(&config, explicit.as_deref()).await {
        Ok(pool) => {
            let version = pool.server_version().await.unwrap_or_default();
            pool.close().await;
            Ok(TestConnectionResult {
                ok: true,
                server_version: Some(version),
                elapsed_ms: started.elapsed().as_millis() as u64,
                error: None,
            })
        }
        Err(err) => Ok(TestConnectionResult {
            ok: false,
            server_version: None,
            elapsed_ms: started.elapsed().as_millis() as u64,
            error: Some(err.to_string()),
        }),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub server_version: String,
    pub dialect: Dialect,
    pub namespaces: Vec<NamespaceInfo>,
}

#[tauri::command]
pub async fn open_connection(
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<ConnectionInfo> {
    let pool = pool_for(&state, &connection_id).await?;
    let server_version = pool.server_version().await.unwrap_or_default();
    let namespaces = introspect::list_namespaces(&pool).await?;
    Ok(ConnectionInfo {
        server_version,
        dialect: pool.dialect(),
        namespaces,
    })
}

#[tauri::command]
pub async fn close_connection(state: State<'_, AppState>, connection_id: String) -> AppResult<()> {
    if let Some(pool) = state.registry.get(&connection_id) {
        pool.close().await;
    }
    state.registry.drop_pool(&connection_id);
    Ok(())
}

// ---------------------------------------------------------------------------
// Schema browsing
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_namespaces(
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<Vec<NamespaceInfo>> {
    let pool = pool_for(&state, &connection_id).await?;
    introspect::list_namespaces(&pool).await
}

#[tauri::command]
pub async fn list_tables(
    state: State<'_, AppState>,
    connection_id: String,
    namespace: String,
) -> AppResult<Vec<TableSummary>> {
    let pool = pool_for(&state, &connection_id).await?;
    introspect::list_tables(&pool, &namespace).await
}

#[tauri::command]
pub async fn get_table_structure(
    state: State<'_, AppState>,
    connection_id: String,
    namespace: String,
    table: String,
) -> AppResult<TableStructure> {
    let pool = pool_for(&state, &connection_id).await?;
    introspect::load_table(&pool, &namespace, &table).await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableData {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<crate::db::value::Cell>>,
    pub primary_key: Vec<String>,
    pub limit: u64,
    pub offset: u64,
    pub has_more: bool,
    /// Rows matching the current filter, when the server could count them.
    pub total: Option<u64>,
    pub elapsed_ms: u64,
    pub sql: String,
}

/// Engine-agnostic core of the table data browser. Shared by the command and by
/// the live integration tests.
pub(crate) async fn preview_table_data_core(
    pool: &Pool,
    namespace: &str,
    table: &str,
    limit: Option<u64>,
    offset: Option<u64>,
    filters: &FilterGroup,
    sort: &[SortSpec],
) -> AppResult<TableData> {
    let structure = introspect::load_table(pool, namespace, table).await?;
    let dialect = pool.dialect();
    // The grid shows at most one thousand rows per page; a larger page is a
    // request the browser cannot render usefully anyway.
    let limit = limit.unwrap_or(1000).clamp(1, MAX_PAGE_ROWS);
    let offset = offset.unwrap_or(0);
    // One extra row is fetched so `has_more` is known without a second query.
    let query = build_grid_query(
        dialect,
        namespace,
        table,
        &structure,
        filters,
        sort,
        limit + 1,
        offset,
    )?;
    let started = Instant::now();
    let mut rows = read_rows(
        pool,
        &ReadPlan {
            select_sql: query.select_sql.clone(),
            kinds: structure
                .columns
                .iter()
                .map(|column| column.kind())
                .collect(),
        },
    )
    .await?;
    let has_more = rows.len() as u64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }
    // The filtered total, so the grid can say how much the filter matched
    // rather than only how much is on screen.
    let total = match pool
        .fetch(&query.count_sql, &[], &[crate::db::value::ColumnKind::Int])
        .await
    {
        Ok(rows) => rows
            .first()
            .and_then(|row| row.first())
            .and_then(|value| value.display())
            .and_then(|text| text.parse::<u64>().ok()),
        Err(_) => None,
    };
    let columns = structure
        .columns
        .iter()
        .map(|c| ColumnMeta {
            name: c.name.clone(),
            data_type: c.full_type.clone(),
            kind: c.kind(),
            nullable: c.nullable,
            primary_key: structure.primary_key.contains(&c.name),
            generated: !c.is_writable(),
            comment: c.comment.clone(),
        })
        .collect();
    let cells = rows
        .into_iter()
        .map(|row| row.iter().map(|v| v.to_cell()).collect())
        .collect();

    Ok(TableData {
        columns,
        rows: cells,
        primary_key: structure.primary_key.clone(),
        limit,
        offset,
        has_more,
        total,
        elapsed_ms: started.elapsed().as_millis() as u64,
        sql: query.select_sql,
    })
}

#[tauri::command]
pub async fn preview_table_data(
    state: State<'_, AppState>,
    connection_id: String,
    namespace: String,
    table: String,
    limit: Option<u64>,
    offset: Option<u64>,
    filters: Option<FilterGroup>,
    sort: Option<Vec<SortSpec>>,
) -> AppResult<TableData> {
    let pool = pool_for(&state, &connection_id).await?;
    preview_table_data_core(
        &pool,
        &namespace,
        &table,
        limit,
        offset,
        &filters.unwrap_or_default(),
        &sort.unwrap_or_default(),
    )
    .await
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCellRequest {
    pub connection_id: String,
    pub namespace: String,
    pub table: String,
    /// Primary key values identifying the row.
    pub key: Vec<RowKeyValue>,
    pub column: String,
    #[serde(default)]
    pub value: Option<String>,
    /// The value the grid displayed, used to detect a concurrent change.
    #[serde(default)]
    pub previous: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CellUpdateResult {
    /// False when the row no longer matches — it was changed or deleted.
    pub updated: bool,
    pub affected: u64,
    pub sql: String,
    pub message: String,
}

/// Apply one cell edit.
///
/// The statement matches on the primary key *and* the value the grid showed, so
/// a row changed by someone else is reported instead of being overwritten.
#[tauri::command]
pub async fn update_table_cell(
    state: State<'_, AppState>,
    request: UpdateCellRequest,
) -> AppResult<CellUpdateResult> {
    let pool = pool_for(&state, &request.connection_id).await?;
    let structure = introspect::load_table(&pool, &request.namespace, &request.table).await?;
    let edit = CellEdit {
        column: request.column.clone(),
        value: request.value.clone(),
        previous: request.previous.clone(),
    };
    let sql = build_cell_update(
        pool.dialect(),
        &request.namespace,
        &request.table,
        &structure,
        &request.key,
        &edit,
    )?;
    let outcome = pool
        .run_script_reporting(&[sql.clone()], true, &ProgressReporter::inert())
        .await;
    let affected = outcome
        .outcomes
        .first()
        .map(|entry| entry.rows_affected)
        .unwrap_or(0);
    let failed = outcome
        .outcomes
        .first()
        .filter(|entry| !entry.ok)
        .and_then(|entry| entry.error.clone());
    if let Some(error) = failed {
        return Err(AppError::msg(format!("修改失败：{error}")));
    }
    Ok(CellUpdateResult {
        updated: affected > 0,
        affected,
        message: if affected > 0 {
            format!("已修改 {} 行", affected)
        } else {
            "该行已被其他人修改或删除，未做任何更改".to_string()
        },
        sql,
    })
}

// ---------------------------------------------------------------------------
// Structure comparison
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareSchemaRequest {
    pub source_connection_id: String,
    pub source_namespace: String,
    pub target_connection_id: String,
    pub target_namespace: String,
    #[serde(default)]
    pub ignore_comments: Option<bool>,
    #[serde(default)]
    pub ignore_case: Option<bool>,
    #[serde(default)]
    pub compare_charset: Option<bool>,
    /// Restrict the comparison to a subset of tables.
    #[serde(default)]
    pub tables: Option<Vec<String>>,
    /// Frontend-supplied id used for progress events and cancellation.
    #[serde(default)]
    pub job_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareSchemaResult {
    pub compare_id: String,
    pub diff: SchemaDiff,
    pub warnings: Vec<String>,
}

/// Create the progress reporter for a job id supplied by the frontend. An
/// absent id yields an inert reporter, so every command also works without the
/// progress channel.
fn begin_job(
    app: &tauri::AppHandle,
    state: &AppState,
    job_id: Option<&str>,
) -> (ProgressReporter, Option<String>) {
    match job_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => {
            let flag = state.jobs.begin(id);
            (
                ProgressReporter::new(id.to_string(), Some(app.clone()), flag),
                Some(id.to_string()),
            )
        }
        None => (ProgressReporter::inert(), None),
    }
}

fn end_job(state: &AppState, job: Option<String>) {
    if let Some(id) = job {
        state.jobs.finish(&id);
    }
}

#[tauri::command]
pub fn cancel_job(state: State<'_, AppState>, job_id: String) -> bool {
    state.jobs.cancel(&job_id)
}

/// Synchronisation is supported between two databases of the *same* engine.
/// Mixing MySQL and PostgreSQL would require translating types, expressions and
/// view SQL, which cannot be done reliably, so it is refused explicitly rather
/// than attempted.
fn ensure_same_engine(source: &Pool, target: &Pool) -> AppResult<()> {
    let source_dialect = source.dialect();
    let target_dialect = target.dialect();
    if source_dialect == target_dialect {
        return Ok(());
    }
    Err(AppError::msg(format!(
        "不支持跨数据库类型同步：源连接是 {}，目标连接是 {}。请选择两个同为 {} 或同为 {} 的连接。",
        source_dialect.label(),
        target_dialect.label(),
        source_dialect.label(),
        target_dialect.label(),
    )))
}

async fn load_structures(
    pool: &Pool,
    namespace: &str,
    only: Option<&Vec<String>>,
    role: &str,
    reporter: &ProgressReporter,
) -> AppResult<(Vec<TableStructure>, Vec<String>)> {
    let mut warnings = Vec::new();
    let tables = introspect::list_tables(pool, namespace).await?;
    let candidates: Vec<&TableSummary> = tables
        .iter()
        .filter(|table| match only {
            Some(filter) => filter.iter().any(|f| f == &table.name),
            None => true,
        })
        .collect();
    let total = candidates.len() as u64;
    reporter.force(
        "introspect",
        "读取表结构",
        0,
        Some(total),
        format!("{role} {namespace}：共 {total} 张表"),
    );

    let mut structures = Vec::new();
    for (index, table) in candidates.iter().enumerate() {
        reporter.check_cancelled()?;
        if !introspect::is_syncable(table) {
            warnings.push(format!("已跳过视图/外部表: {}", table.name));
            continue;
        }
        match introspect::load_table(pool, namespace, &table.name).await {
            Ok(structure) => structures.push(structure),
            Err(err) => warnings.push(format!("读取 {} 结构失败: {err}", table.name)),
        }
        reporter.tick(
            "introspect",
            "读取表结构",
            index as u64 + 1,
            Some(total),
            format!("{role}：{}", table.name),
        );
    }
    Ok((structures, warnings))
}

#[tauri::command]
/// Engine-agnostic core of a structure comparison. Shared by the Tauri command
/// and by the live database integration tests so both exercise one code path.
pub(crate) async fn compare_schema_core(
    source_pool: &Pool,
    source_namespace: &str,
    target_pool: &Pool,
    target_namespace: &str,
    options: CompareOptions,
    tables: Option<&Vec<String>>,
    reporter: &ProgressReporter,
) -> AppResult<(SchemaDiff, HashMap<String, TableStructure>, Vec<String>)> {
    ensure_same_engine(source_pool, target_pool)?;
    let (source_structures, mut warnings) =
        load_structures(source_pool, source_namespace, tables, "源", reporter).await?;
    let (target_structures, target_warnings) =
        load_structures(target_pool, target_namespace, None, "目标", reporter).await?;
    warnings.extend(target_warnings);
    reporter.check_cancelled()?;

    reporter.force("introspect", "读取视图与自定义类型", 0, None, "正在读取视图与类型定义");
    let source_views = introspect::list_views(source_pool, source_namespace).await?;
    let target_views = introspect::list_views(target_pool, target_namespace).await?;
    let source_types = introspect::list_custom_types(source_pool, source_namespace).await?;
    let target_types = introspect::list_custom_types(target_pool, target_namespace).await?;
    reporter.check_cancelled()?;

    let mut diff = compare_everything(
        &source_structures,
        &target_structures,
        &source_views,
        &target_views,
        &source_types,
        &target_types,
        source_pool.dialect(),
        target_pool.dialect(),
        source_namespace,
        target_namespace,
        options,
    );
    diff.source_namespace = source_namespace.to_string();
    diff.target_namespace = target_namespace.to_string();

    let structure_map: HashMap<String, TableStructure> = source_structures
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect();

    Ok((diff, structure_map, warnings))
}

#[tauri::command]
pub async fn compare_schema(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: CompareSchemaRequest,
) -> AppResult<CompareSchemaResult> {
    let (reporter, job) = begin_job(&app, &state, request.job_id.as_deref());
    let source_pool = pool_for(&state, &request.source_connection_id).await?;
    let target_pool = pool_for(&state, &request.target_connection_id).await?;

    let options = CompareOptions {
        ignore_comments: request.ignore_comments.unwrap_or(true),
        ignore_case: request.ignore_case.unwrap_or(false),
        compare_charset: request.compare_charset.unwrap_or(false),
    };
    let outcome = compare_schema_core(
        &source_pool,
        &request.source_namespace,
        &target_pool,
        &request.target_namespace,
        options,
        request.tables.as_ref(),
        &reporter,
    )
    .await;
    end_job(&state, job);
    reporter.finish("done", "对比完成", "结构对比结束");
    let (diff, structure_map, warnings) = outcome?;

    let compare_id = state.compares.put(SchemaCompare {
        diff: diff.clone(),
        source_structures: structure_map,
        target_connection_id: request.target_connection_id.clone(),
        target_namespace: request.target_namespace.clone(),
        created_at: Instant::now(),
    });

    Ok(CompareSchemaResult {
        compare_id,
        diff,
        warnings,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaSqlPreview {
    pub statements: Vec<DdlStatement>,
    pub warnings: Vec<String>,
}

#[tauri::command]
pub fn preview_schema_sql(
    state: State<'_, AppState>,
    compare_id: String,
    selection: Option<SyncSelection>,
) -> AppResult<SchemaSqlPreview> {
    let mut warnings = Vec::new();
    let statements = state
        .compares
        .with(&compare_id, |entry| {
            generate(
                &entry.diff,
                &entry.source_structures,
                selection.as_ref(),
                Some(&entry.target_namespace),
                &mut warnings,
            )
        })
        .ok_or_else(|| AppError::msg("对比结果已过期，请重新对比"))?;
    Ok(SchemaSqlPreview {
        statements,
        warnings,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRunResult {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub elapsed_ms: u64,
    pub message: String,
    pub failures: Vec<StatementOutcome>,
    pub sample: Vec<StatementOutcome>,
    /// Non-fatal notes, e.g. a table that will fail on a non-empty target.
    pub warnings: Vec<String>,
    pub cancelled: bool,
    /// A transaction was used and rolled back, so the target is unchanged.
    pub rolled_back: bool,
    /// Whether the engine could run the plan atomically.
    pub transactional: bool,
    /// Statements never attempted because execution stopped at a failure.
    pub skipped: usize,
}

const SAMPLE_LIMIT: usize = 300;

fn summarize(
    outcomes: Vec<StatementOutcome>,
    elapsed_ms: u64,
    label: &str,
    warnings: Vec<String>,
    cancelled: bool,
) -> SyncRunResult {
    let total = outcomes.len();
    let failed = outcomes.iter().filter(|o| !o.ok).count();
    let succeeded = total - failed;
    let failures: Vec<StatementOutcome> = outcomes.iter().filter(|o| !o.ok).cloned().collect();
    let sample: Vec<StatementOutcome> = outcomes.into_iter().take(SAMPLE_LIMIT).collect();
    let message = if cancelled {
        format!("{label}已取消：已执行 {total} 条语句")
    } else if failed == 0 {
        format!("{label}完成：共执行 {total} 条语句")
    } else {
        format!("{label}结束：成功 {succeeded} 条，失败 {failed} 条")
    };
    SyncRunResult {
        total,
        succeeded,
        failed,
        elapsed_ms,
        message,
        failures,
        sample,
        warnings,
        cancelled,
        rolled_back: false,
        transactional: false,
        skipped: 0,
    }
}

/// Schema sync reports rollback explicitly: the difference between "nothing
/// happened" and "half of it happened" matters enormously to the user.
fn summarize_ddl(run: StatementRun, elapsed_ms: u64, warnings: Vec<String>) -> SyncRunResult {
    let total = run.outcomes.len();
    let failed = run.outcomes.iter().filter(|o| !o.ok).count();
    let succeeded = total - failed;
    let failures: Vec<StatementOutcome> = run.outcomes.iter().filter(|o| !o.ok).cloned().collect();
    let sample: Vec<StatementOutcome> = run.outcomes.iter().take(SAMPLE_LIMIT).cloned().collect();

    let message = if run.cancelled && run.rolled_back {
        format!("结构同步已取消，事务已回滚：目标库未发生任何变更（已执行 {total} 条）")
    } else if run.cancelled {
        format!(
            "结构同步已取消：已执行 {total} 条语句（该数据库不支持 DDL 事务，已执行的变更无法回滚）"
        )
    } else if failed == 0 {
        format!("结构同步完成：共执行 {total} 条语句")
    } else if run.rolled_back {
        format!(
            "结构同步失败，事务已回滚：目标库未发生任何变更（第 {total} 条失败，剩余 {} 条未执行）",
            run.skipped
        )
    } else {
        format!(
            "结构同步失败：已执行 {total} 条语句（该数据库不支持 DDL 事务，无法回滚，已执行的变更会保留），剩余 {} 条未执行",
            run.skipped
        )
    };

    SyncRunResult {
        total,
        succeeded,
        failed,
        elapsed_ms,
        message,
        failures,
        sample,
        warnings,
        cancelled: run.cancelled,
        rolled_back: run.rolled_back,
        transactional: run.transactional,
        skipped: run.skipped,
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplySchemaRequest {
    pub compare_id: String,
    #[serde(default)]
    pub selection: Option<SyncSelection>,
    /// Wrap the whole plan in a transaction where the engine supports it.
    #[serde(default = "default_true")]
    pub use_transaction: bool,
    #[serde(default)]
    pub job_id: Option<String>,
}

#[tauri::command]
pub async fn apply_schema_sync(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: ApplySchemaRequest,
) -> AppResult<SyncRunResult> {
    let (reporter, job) = begin_job(&app, &state, request.job_id.as_deref());
    let mut warnings = Vec::new();
    let (statements, target_connection, target_namespace) = state
        .compares
        .with(&request.compare_id, |entry| {
            let statements = generate(
                &entry.diff,
                &entry.source_structures,
                request.selection.as_ref(),
                Some(&entry.target_namespace),
                &mut warnings,
            );
            (
                statements,
                entry.target_connection_id.clone(),
                entry.target_namespace.clone(),
            )
        })
        .ok_or_else(|| AppError::msg("对比结果已过期，请重新对比"))?;
    let _ = target_namespace;

    if statements.is_empty() {
        return Ok(summarize(Vec::new(), 0, "结构同步", Vec::new(), false));
    }

    let pool = pool_for(&state, &target_connection).await?;
    let sql: Vec<String> = statements.into_iter().map(|s| s.sql).collect();
    let started = Instant::now();
    let run = pool
        .run_statements(&sql, request.use_transaction, &reporter)
        .await;
    end_job(&state, job);
    reporter.finish("done", "结构同步完成", "结构同步结束");
    let run = run?;
    Ok(summarize_ddl(
        run,
        started.elapsed().as_millis() as u64,
        warnings,
    ))
}

// ---------------------------------------------------------------------------
// Data comparison
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareDataRequest {
    pub source: TableEndpoint,
    pub target: TableEndpoint,
    #[serde(default)]
    pub key_columns: Vec<ColumnPair>,
    #[serde(default)]
    pub column_map: Vec<ColumnPair>,
    #[serde(default)]
    pub batch_size: Option<usize>,
    #[serde(default)]
    pub job_id: Option<String>,
}

fn auto_column_pairs(source: &TableStructure, target: &TableStructure) -> Vec<ColumnPair> {
    target
        .columns
        .iter()
        // A generated column is derived on the target; writing to it is either
        // rejected by the server or silently wrong.
        .filter(|t| t.is_writable())
        .filter(|t| source.column(&t.name).is_some())
        .map(|t| ColumnPair {
            source: t.name.clone(),
            target: t.name.clone(),
        })
        .collect()
}

fn auto_key_pairs(
    source: &TableStructure,
    target: &TableStructure,
    requested: &[ColumnPair],
) -> AppResult<Vec<ColumnPair>> {
    if !requested.is_empty() {
        return Ok(requested.to_vec());
    }
    if target.primary_key.is_empty() {
        return Err(AppError::msg(
            "目标表没有主键，无法进行数据对比，请手动指定对比字段",
        ));
    }
    let mut pairs = Vec::new();
    for key in &target.primary_key {
        if source.column(key).is_none() {
            return Err(AppError::msg(format!(
                "源表缺少主键字段 {key}，请手动指定对比字段"
            )));
        }
        pairs.push(ColumnPair {
            source: key.clone(),
            target: key.clone(),
        });
    }
    Ok(pairs)
}

#[tauri::command]
/// Engine-agnostic core of a data comparison.
///
/// Both tables are read in key order with keyset pagination and merged, so
/// memory is bounded by the page size plus the number of *differences* — never
/// by the row count of either table.
pub(crate) async fn compare_data_core(
    source_pool: &Pool,
    source: &TableEndpoint,
    target_pool: &Pool,
    target: &TableEndpoint,
    requested_keys: &[ColumnPair],
    requested_map: &[ColumnPair],
    batch_size: usize,
    reporter: &ProgressReporter,
) -> AppResult<(DataDiffResult, DataPlan, Vec<String>)> {
    ensure_same_engine(source_pool, target_pool)?;
    reporter.force("compare", "对比数据", 0, None, "正在读取表结构");
    let dialect = source_pool.dialect();

    let mut warnings = Vec::new();
    let source_structure =
        introspect::load_table(source_pool, &source.namespace, &source.table).await?;
    let target_structure =
        introspect::load_table(target_pool, &target.namespace, &target.table).await?;

    let key_pairs = auto_key_pairs(&source_structure, &target_structure, requested_keys)?;
    let column_pairs = if requested_map.is_empty() {
        auto_column_pairs(&source_structure, &target_structure)
    } else {
        requested_map.to_vec()
    };

    // Drop any mapping that targets a generated (computed) column.
    let mut skipped_generated: Vec<String> = Vec::new();
    let column_pairs: Vec<ColumnPair> = column_pairs
        .into_iter()
        .filter(|pair| match target_structure.column(&pair.target) {
            Some(column) if !column.is_writable() => {
                skipped_generated.push(pair.target.clone());
                false
            }
            _ => true,
        })
        .collect();
    if !skipped_generated.is_empty() {
        warnings.push(format!(
            "已跳过目标表的生成列（由表达式自动计算，不能写入）：{}",
            skipped_generated.join(", ")
        ));
    }
    if column_pairs.is_empty() {
        return Err(AppError::msg(
            "源表与目标表没有可写入的同名字段，请手动配置字段映射",
        ));
    }

    let batch = batch_size.clamp(1, 20_000);
    let source_keys: Vec<String> = key_pairs.iter().map(|p| p.source.clone()).collect();
    let target_keys: Vec<String> = key_pairs.iter().map(|p| p.target.clone()).collect();

    let source_plan = StreamPlan::build(dialect, &source_structure, &source_keys)?;
    let target_plan = StreamPlan::build(dialect, &target_structure, &target_keys)?;

    let source_columns = source_plan.column_names();
    let target_columns = target_plan.column_names();
    let target_kinds = target_plan.kinds();

    let index_in = |columns: &[String], name: &str| -> AppResult<usize> {
        column_index(columns, name)
            .ok_or_else(|| AppError::msg(format!("表结构中缺少字段 {name}")))
    };
    let source_pair_indexes: Vec<usize> = column_pairs
        .iter()
        .map(|pair| index_in(&source_columns, &pair.source))
        .collect::<AppResult<_>>()?;
    let target_pair_indexes: Vec<usize> = column_pairs
        .iter()
        .map(|pair| index_in(&target_columns, &pair.target))
        .collect::<AppResult<_>>()?;

    let mut source_stream = TableStream::new(
        source_pool,
        dialect,
        &source.namespace,
        &source.table,
        source_plan,
        batch,
    );
    let mut target_stream = TableStream::new(
        target_pool,
        dialect,
        &target.namespace,
        &target.table,
        target_plan,
        batch,
    );

    let mut summary = DataDiffSummary::default();
    let mut insert_rows: Vec<Vec<Value>> = Vec::new();
    let mut insert_sql: Vec<String> = Vec::new();
    let mut update_sql: Vec<String> = Vec::new();
    let mut delete_sql: Vec<String> = Vec::new();
    let mut insert_preview: Vec<RowChange> = Vec::new();
    let mut update_preview: Vec<RowChange> = Vec::new();
    let mut delete_preview: Vec<RowChange> = Vec::new();

    let mut processed: u64 = 0;
    loop {
        processed += 1;
        if processed % 256 == 0 {
            reporter.check_cancelled()?;
            reporter.tick(
                "compare",
                "对比数据",
                summary.source_rows as u64 + summary.target_rows as u64 + summary.unchanged as u64,
                None,
                format!(
                    "已比对 {} 行 · 新增 {} / 修改 {} / 删除 {}",
                    summary.unchanged + summary.inserts + summary.updates + summary.deletes,
                    summary.inserts,
                    summary.updates,
                    summary.deletes
                ),
            );
        }
        let source_key = source_stream.peek_key().await?;
        let target_key = target_stream.peek_key().await?;

        let ordering = match (&source_key, &target_key) {
            (None, None) => break,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(a), Some(b)) => compare_key_tuples(a, b),
        };

        match ordering {
            // Present only in the source: insert it.
            Ordering::Less => {
                let row = source_stream
                    .next_row()
                    .await?
                    .expect("peek_key returned a row");
                summary.inserts += 1;
                if insert_preview.len() < PREVIEW_ROWS {
                    insert_preview.extend(preview_inserts(
                        &source_columns,
                        &column_pairs,
                        &key_pairs,
                        std::slice::from_ref(&row),
                    ));
                }
                insert_rows.push(row);
                if insert_rows.len() >= batch {
                    insert_sql.extend(build_insert_statements(
                        dialect,
                        &target.namespace,
                        &target.table,
                        &source_columns,
                        &target_columns,
                        &target_kinds,
                        &column_pairs,
                        &insert_rows,
                        insert_rows.len(),
                    ));
                    insert_rows.clear();
                }
            }
            // Present only in the target: delete it.
            Ordering::Greater => {
                let row = target_stream
                    .next_row()
                    .await?
                    .expect("peek_key returned a row");
                summary.deletes += 1;
                if delete_preview.len() < PREVIEW_ROWS {
                    delete_preview.extend(preview_deletes(
                        &target_columns,
                        &column_pairs,
                        &key_pairs,
                        std::slice::from_ref(&row),
                    ));
                }
                delete_sql.extend(build_delete_statements(
                    dialect,
                    &target.namespace,
                    &target.table,
                    &target_columns,
                    &target_kinds,
                    &key_pairs,
                    std::slice::from_ref(&row),
                ));
            }
            // Present on both sides: update when any mapped value differs.
            Ordering::Equal => {
                let source_row = source_stream
                    .next_row()
                    .await?
                    .expect("peek_key returned a row");
                let target_row = target_stream
                    .next_row()
                    .await?
                    .expect("peek_key returned a row");

                let mut changes: Vec<(usize, Value)> = Vec::new();
                for (position, pair) in column_pairs.iter().enumerate() {
                    let _ = pair;
                    let source_value = source_row
                        .get(source_pair_indexes[position])
                        .cloned()
                        .unwrap_or(Value::Null);
                    let target_value = target_row
                        .get(target_pair_indexes[position])
                        .cloned()
                        .unwrap_or(Value::Null);
                    if source_value.canonical() != target_value.canonical() {
                        changes.push((target_pair_indexes[position], source_value));
                    }
                }

                if changes.is_empty() {
                    summary.unchanged += 1;
                } else {
                    summary.updates += 1;
                    if update_preview.len() < PREVIEW_ROWS {
                        update_preview.extend(preview_updates(
                            &target_columns,
                            &key_pairs,
                            &[(target_row.clone(), changes.clone())],
                        ));
                    }
                    update_sql.extend(build_update_statements(
                        dialect,
                        &target.namespace,
                        &target.table,
                        &target_columns,
                        &target_kinds,
                        &key_pairs,
                        &[(target_row, changes)],
                    ));
                }
            }
        }
    }

    if !insert_rows.is_empty() {
        insert_sql.extend(build_insert_statements(
            dialect,
            &target.namespace,
            &target.table,
            &source_columns,
            &target_columns,
            &target_kinds,
            &column_pairs,
            &insert_rows,
            insert_rows.len(),
        ));
        insert_rows.clear();
    }

    summary.source_rows = source_stream.scanned as usize;
    summary.target_rows = target_stream.scanned as usize;

    let sql_preview: Vec<String> = delete_sql
        .iter()
        .chain(update_sql.iter())
        .chain(insert_sql.iter())
        .take(SQL_PREVIEW_LIMIT)
        .cloned()
        .collect();

    let preview_truncated = summary.inserts > insert_preview.len()
        || summary.updates > update_preview.len()
        || summary.deletes > delete_preview.len();

    let plan = DataPlan {
        insert: insert_sql,
        update: update_sql,
        delete: delete_sql,
        target_namespace: target.namespace.clone(),
        created_at: Instant::now(),
    };

    Ok((
        DataDiffResult {
            plan_id: String::new(),
            columns: column_pairs,
            key_columns: key_pairs,
            summary,
            insert_preview,
            update_preview,
            delete_preview,
            preview_truncated,
            sql_preview,
            warnings,
        },
        plan,
        Vec::new(),
    ))
}

#[tauri::command]
pub async fn compare_data(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: CompareDataRequest,
) -> AppResult<DataDiffResult> {
    let (reporter, job) = begin_job(&app, &state, request.job_id.as_deref());
    let source_pool = pool_for(&state, &request.source.connection_id).await?;
    let target_pool = pool_for(&state, &request.target.connection_id).await?;
    let outcome = compare_data_core(
        &source_pool,
        &request.source,
        &target_pool,
        &request.target,
        &request.key_columns,
        &request.column_map,
        request
            .batch_size
            .unwrap_or(DEFAULT_BATCH_SIZE)
            .clamp(1, 20_000),
        &reporter,
    )
    .await;
    end_job(&state, job);
    reporter.finish("done", "数据对比完成", "数据对比结束");
    let (mut result, plan, _) = outcome?;
    result.plan_id = state.plans.put(plan);
    Ok(result)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyDataRequest {
    pub plan_id: String,
    pub connection_id: String,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default = "default_true")]
    pub insert: bool,
    #[serde(default = "default_true")]
    pub update: bool,
    /// Defaults to *off*: a partial request must never delete rows silently.
    #[serde(default)]
    pub delete: bool,
    #[serde(default = "default_true")]
    pub use_transaction: bool,
    /// PostgreSQL materialised views snapshot their data at creation time, so
    /// they must be refreshed after the underlying rows change.
    #[serde(default = "default_true")]
    pub refresh_materialized_views: bool,
}

fn default_true() -> bool {
    true
}

/// Engine-agnostic core of applying a data plan: execute the statements and,
/// on PostgreSQL, refresh any materialised views so they are not left stale.
pub(crate) async fn apply_data_plan(
    pool: &Pool,
    statements: &[String],
    use_transaction: bool,
    target_namespace: &str,
    refresh: bool,
    reporter: &ProgressReporter,
) -> AppResult<(Vec<StatementOutcome>, Vec<String>, bool)> {
    if statements.is_empty() {
        return Ok((Vec::new(), Vec::new(), false));
    }

    let (mut outcomes, cancelled) = if use_transaction {
        // MySQL cannot roll back DDL but DML is transactional on both engines.
        let run = pool
            .run_in_transaction_reporting(statements, reporter)
            .await?;
        (run.outcomes, run.cancelled)
    } else {
        let run = pool.run_script_reporting(statements, false, reporter).await;
        (run.outcomes, run.cancelled)
    };

    let mut warnings = Vec::new();
    if !cancelled && refresh {
        refresh_materialized_views(pool, target_namespace, &mut outcomes, &mut warnings).await;
    }
    Ok((outcomes, warnings, cancelled))
}

/// Refresh every materialised view in a namespace.
///
/// A materialised view is populated when it is created, so rows written later
/// would otherwise leave it stale. PostgreSQL is the only supported engine with
/// them; on MySQL this is a no-op.
async fn refresh_materialized_views(
    pool: &Pool,
    namespace: &str,
    outcomes: &mut Vec<StatementOutcome>,
    warnings: &mut Vec<String>,
) {
    if pool.dialect() != Dialect::Postgres {
        return;
    }
    let views = match introspect::list_views(pool, namespace).await {
        Ok(views) => views,
        Err(err) => {
            warnings.push(format!("读取物化视图失败，未刷新: {err}"));
            return;
        }
    };
    for view in views.iter().filter(|v| v.is_materialized()) {
        let sql = format!(
            "REFRESH MATERIALIZED VIEW {};",
            pool.dialect().qualified(Some(namespace), &view.name)
        );
        let started = Instant::now();
        match pool.execute(&sql).await {
            Ok(affected) => outcomes.push(StatementOutcome {
                sql,
                ok: true,
                rows_affected: affected,
                error: None,
                elapsed_ms: started.elapsed().as_millis() as u64,
            }),
            Err(err) => {
                warnings.push(format!("刷新物化视图 {} 失败: {err}", view.name));
                outcomes.push(StatementOutcome {
                    sql,
                    ok: false,
                    rows_affected: 0,
                    error: Some(err.to_string()),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            }
        }
    }
}

#[tauri::command]
pub async fn apply_data_sync(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: ApplyDataRequest,
) -> AppResult<SyncRunResult> {
    let (reporter, job) = begin_job(&app, &state, request.job_id.as_deref());

    let plan = state
        .plans
        .with(&request.plan_id, |plan| {
            (
                plan.select(request.insert, request.update, request.delete),
                plan.target_namespace.clone(),
            )
        })
        .ok_or_else(|| AppError::msg("对比计划已过期，请重新对比"))?;
    let (statements, target_namespace) = plan;

    if statements.is_empty() {
        end_job(&state, job);
        return Ok(summarize(
            Vec::new(),
            0,
            "数据同步",
            Vec::new(),
            false,
        ));
    }

    let pool = pool_for(&state, &request.connection_id).await?;
    let started = Instant::now();
    let outcome = apply_data_plan(
        &pool,
        &statements,
        request.use_transaction,
        &target_namespace,
        request.refresh_materialized_views,
        &reporter,
    )
    .await;

    state.plans.remove(&request.plan_id);
    end_job(&state, job);
    reporter.finish("done", "数据同步完成", "数据同步结束");

    let (outcomes, warnings, cancelled) = outcome?;
    Ok(summarize(
        outcomes,
        started.elapsed().as_millis() as u64,
        "数据同步",
        warnings,
        cancelled,
    ))
}

// ---------------------------------------------------------------------------
// Table level operations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DropTablesRequest {
    pub connection_id: String,
    pub namespace: String,
    pub tables: Vec<String>,
    #[serde(default)]
    pub job_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DropTablesPreview {
    pub statements: Vec<DdlStatement>,
    /// Views that read from the tables being dropped. They are *not* removed
    /// automatically — the server would either refuse the drop or, with
    /// CASCADE, quietly discard them — so they are surfaced for the user.
    pub dependent_views: Vec<String>,
    pub warnings: Vec<String>,
}

/// Build the drop plan for a set of tables.
///
/// Only the tables being dropped and the tables referencing them are loaded,
/// which keeps this cheap on a large schema while still letting the planner
/// release inbound foreign keys.
pub(crate) async fn plan_drop_scope(
    pool: &Pool,
    namespace: &str,
    tables: &[String],
) -> AppResult<(Vec<DdlStatement>, Vec<String>, Vec<String>)> {
    let dialect = pool.dialect();
    let edges = introspect::list_foreign_key_edges(pool, namespace).await?;
    let referencing: Vec<String> = edges
        .iter()
        .filter(|(_, parent)| tables.iter().any(|name| name == parent))
        .map(|(child, _)| child.clone())
        .filter(|child| !tables.iter().any(|name| name == child))
        .collect();

    let views = introspect::list_views(pool, namespace).await?;
    let dependent_views: Vec<String> = views
        .iter()
        .filter(|view| {
            view.dependencies
                .iter()
                .any(|dependency| tables.iter().any(|name| name == dependency))
        })
        .map(|view| view.name.clone())
        .collect();

    let mut structures = Vec::new();
    for name in tables.iter().chain(referencing.iter()) {
        structures.push(introspect::load_table(pool, namespace, name).await?);
    }

    let mut warnings = Vec::new();
    if !dependent_views.is_empty() {
        warnings.push(format!(
            "以下视图依赖这些表，删除会失败或被级联删除，请先处理：{}",
            dependent_views.join(", ")
        ));
    }
    let statements = plan_drop_tables(
        &structures,
        tables,
        dialect,
        Some(namespace),
        &mut warnings,
    );
    Ok((statements, dependent_views, warnings))
}

#[tauri::command]
pub async fn preview_drop_tables(
    state: State<'_, AppState>,
    connection_id: String,
    namespace: String,
    tables: Vec<String>,
) -> AppResult<DropTablesPreview> {
    if tables.is_empty() {
        return Err(AppError::msg("请先选择要删除的表"));
    }
    let pool = pool_for(&state, &connection_id).await?;
    let (statements, dependent_views, warnings) =
        plan_drop_scope(&pool, &namespace, &tables).await?;
    Ok(DropTablesPreview {
        statements,
        dependent_views,
        warnings,
    })
}

#[tauri::command]
pub async fn apply_drop_tables(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: DropTablesRequest,
) -> AppResult<SyncRunResult> {
    if request.tables.is_empty() {
        return Err(AppError::msg("请先选择要删除的表"));
    }
    let (reporter, job) = begin_job(&app, &state, request.job_id.as_deref());
    let pool = pool_for(&state, &request.connection_id).await?;
    let (statements, _, warnings) =
        plan_drop_scope(&pool, &request.namespace, &request.tables).await?;
    let sql: Vec<String> = statements.into_iter().map(|s| s.sql).collect();
    let started = Instant::now();
    let run = pool.run_statements(&sql, true, &reporter).await;
    end_job(&state, job);
    reporter.finish("done", "删除完成", "删除结束");
    Ok(summarize_ddl(
        run?,
        started.elapsed().as_millis() as u64,
        warnings,
    ))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameTableRequest {
    pub connection_id: String,
    pub namespace: String,
    pub table: String,
    pub new_name: String,
    #[serde(default)]
    pub job_id: Option<String>,
}

#[tauri::command]
pub async fn rename_table(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: RenameTableRequest,
) -> AppResult<SyncRunResult> {
    let new_name = request.new_name.trim().to_string();
    if new_name.is_empty() {
        return Err(AppError::msg("新表名不能为空"));
    }
    if new_name == request.table {
        return Err(AppError::msg("新表名与原表名相同"));
    }
    let (reporter, job) = begin_job(&app, &state, request.job_id.as_deref());
    let pool = pool_for(&state, &request.connection_id).await?;
    // Fail clearly rather than letting the server produce a duplicate-object
    // error that reads like a tool bug.
    let existing = introspect::list_tables(&pool, &request.namespace).await?;
    if existing
        .iter()
        .any(|table| table.name.eq_ignore_ascii_case(&new_name))
    {
        end_job(&state, job);
        return Err(AppError::msg(format!("表 {new_name} 已存在")));
    }
    let sql = vec![rename_table_ddl(
        &request.table,
        &new_name,
        pool.dialect(),
        Some(&request.namespace),
    )];
    let started = Instant::now();
    let run = pool.run_statements(&sql, true, &reporter).await;
    end_job(&state, job);
    reporter.finish("done", "重命名完成", "重命名结束");
    Ok(summarize_ddl(run?, started.elapsed().as_millis() as u64, Vec::new()))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTableRequest {
    pub connection_id: String,
    pub namespace: String,
    pub table: String,
    pub columns: Vec<ColumnInfo>,
    #[serde(default)]
    pub primary_key: Vec<String>,
    #[serde(default)]
    pub indexes: Vec<IndexInfo>,
    #[serde(default)]
    pub job_id: Option<String>,
}

/// Create a table from the editor's design.
///
/// The statement comes from the same generator the structure sync uses for new
/// tables, so a table made here and one made by a sync are identical.
pub(crate) async fn plan_create_table(
    pool: &Pool,
    namespace: &str,
    table: &str,
    columns: &[ColumnInfo],
    primary_key: &[String],
    indexes: &[IndexInfo],
) -> AppResult<(Vec<DdlStatement>, Vec<String>)> {
    let name = table.trim();
    if name.is_empty() {
        return Err(AppError::msg("表名不能为空"));
    }
    let dialect = pool.dialect();
    let normalized = normalize_designed_columns(dialect, columns)?;
    if normalized.is_empty() {
        return Err(AppError::msg("表至少需要一个字段"));
    }

    // Every key column has to exist, or the generated DDL cannot execute.
    let mut primary_key = primary_key.to_vec();
    for key in &primary_key {
        if !normalized
            .iter()
            .any(|column| column.name.eq_ignore_ascii_case(key.trim()))
        {
            return Err(AppError::msg(format!("主键字段 {key} 不在字段列表里")));
        }
    }
    // Spell the key columns the way the columns themselves are spelt.
    for key in &mut primary_key {
        if let Some(column) = normalized
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(key.trim()))
        {
            *key = column.name.clone();
        }
    }

    let mut indexes = indexes.to_vec();
    for index in &mut indexes {
        index.name = index.name.trim().to_string();
        index.columns = index
            .columns
            .iter()
            .map(|column| column.trim().to_string())
            .filter(|column| !column.is_empty())
            .collect();
    }
    validate_indexes(&indexes, &normalized)?;

    let existing = introspect::list_tables(pool, namespace).await?;
    if existing
        .iter()
        .any(|entry| entry.name.eq_ignore_ascii_case(name))
    {
        return Err(AppError::msg(format!("表 {name} 已存在")));
    }

    let structure = TableStructure {
        name: name.to_string(),
        comment: None,
        engine: None,
        charset: None,
        collation: None,
        primary_key,
        columns: normalized,
        indexes,
        foreign_keys: Vec::new(),
    };
    let mut warnings = Vec::new();
    let statements = create_table_ddl(&structure, dialect, Some(namespace), namespace, &mut warnings);
    Ok((statements, warnings))
}

#[tauri::command]
pub async fn preview_create_table(
    state: State<'_, AppState>,
    request: CreateTableRequest,
) -> AppResult<SchemaSqlPreview> {
    let pool = pool_for(&state, &request.connection_id).await?;
    let (statements, warnings) = plan_create_table(
        &pool,
        &request.namespace,
        &request.table,
        &request.columns,
        &request.primary_key,
        &request.indexes,
    )
    .await?;
    Ok(SchemaSqlPreview {
        statements,
        warnings,
    })
}

#[tauri::command]
pub async fn create_table(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: CreateTableRequest,
) -> AppResult<SyncRunResult> {
    let (reporter, job) = begin_job(&app, &state, request.job_id.as_deref());
    let pool = pool_for(&state, &request.connection_id).await?;
    let planned = plan_create_table(
        &pool,
        &request.namespace,
        &request.table,
        &request.columns,
        &request.primary_key,
        &request.indexes,
    )
    .await;
    let started = Instant::now();
    let outcome = match planned {
        Ok((statements, warnings)) => {
            let sql: Vec<String> = statements.into_iter().map(|s| s.sql).collect();
            let run = pool.run_statements(&sql, true, &reporter).await;
            run.map(|run| (run, warnings))
        }
        Err(error) => Err(error),
    };
    end_job(&state, job);
    reporter.finish("done", "建表完成", "建表结束");
    let (run, warnings) = outcome?;
    Ok(summarize_ddl(
        run,
        started.elapsed().as_millis() as u64,
        warnings,
    ))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlterTableRequest {
    pub connection_id: String,
    pub namespace: String,
    pub table: String,
    /// The complete desired column list, in the desired order.
    pub columns: Vec<ColumnInfo>,
    /// The complete desired index list. When omitted the indexes are left alone,
    /// so a caller that only edits columns cannot disturb them.
    #[serde(default)]
    pub indexes: Option<Vec<IndexInfo>>,
    /// The complete desired foreign key list, with the same "omitted means leave
    /// alone" rule as `indexes`.
    #[serde(default)]
    pub foreign_keys: Option<Vec<ForeignKeyInfo>>,
    /// Columns whose name was edited. Sent explicitly so a rename becomes
    /// `RENAME COLUMN` instead of a drop followed by an add, which would throw
    /// the data away.
    #[serde(default)]
    pub renames: Vec<ColumnRename>,
    #[serde(default)]
    pub job_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnRename {
    pub from: String,
    pub to: String,
}

/// Compare the desired columns against the live table and produce the column
/// changes only.
///
/// The table editor can never create or drop a table: whatever the comparison
/// concludes, only column-level statements are kept.
pub(crate) async fn plan_alter_table(
    pool: &Pool,
    namespace: &str,
    table: &str,
    columns: &[ColumnInfo],
    indexes: Option<&[IndexInfo]>,
    foreign_keys: Option<&[ForeignKeyInfo]>,
    renames: &[ColumnRename],
) -> AppResult<(Vec<DdlStatement>, Vec<String>)> {
    let target = pool.dialect();
    let current = introspect::load_table(pool, namespace, table).await?;
    let desired_columns = normalize_designed_columns(target, columns)?;
    if desired_columns.is_empty() {
        return Err(AppError::msg("表至少需要一个字段"));
    }

    let mut desired = current.clone();
    desired.columns = desired_columns;
    // Advice about kept indexes and unsupported changes, returned alongside the
    // statements so the user is not left guessing.
    let mut warnings: Vec<String> = Vec::new();

    // Indexes are only touched when the caller supplied a list, so the columns
    // tab cannot accidentally drop one.
    if let Some(submitted) = indexes {
        let mut indexes = submitted.to_vec();
        for index in &mut indexes {
            index.name = index.name.trim().to_string();
            index.columns = index
                .columns
                .iter()
                .map(|column| column.trim().to_string())
                .filter(|column| !column.is_empty())
                .collect();
        }
        // A foreign key needs *an* index, and MySQL refuses to drop the last one
        // that supports it. Rather than letting the whole apply fail with the
        // server's wording, the submitted list is checked per key and a survivor
        // is kept — preferring the index MySQL created for the constraint.
        for foreign_key in &current.foreign_keys {
            let satisfied = indexes
                .iter()
                .any(|entry| foreign_key_covered_by(&foreign_key.columns, &entry.columns));
            if satisfied {
                continue;
            }
            let keeper = current
                .indexes
                .iter()
                .filter(|entry| {
                    foreign_key_covered_by(&foreign_key.columns, &entry.columns)
                })
                .min_by_key(|entry| {
                    if entry.name.eq_ignore_ascii_case(&foreign_key.name) {
                        0
                    } else {
                        1
                    }
                });
            if let Some(keeper) = keeper {
                warnings.push(format!(
                    "索引 {} 是外键 {} 唯一的支撑索引，不能删除，已保留",
                    keeper.name, foreign_key.name
                ));
                indexes.push(keeper.clone());
            }
        }
        validate_indexes(&indexes, &desired.columns)?;
        desired.indexes = indexes;
    }

    // Foreign keys are handled the same way: submitted or untouched.
    if let Some(submitted) = foreign_keys {
        let mut foreign_keys = submitted.to_vec();
        for foreign_key in &mut foreign_keys {
            foreign_key.name = foreign_key.name.trim().to_string();
            foreign_key.ref_table = foreign_key.ref_table.trim().to_string();
            foreign_key.columns = foreign_key
                .columns
                .iter()
                .map(|column| column.trim().to_string())
                .filter(|column| !column.is_empty())
                .collect();
            foreign_key.ref_columns = foreign_key
                .ref_columns
                .iter()
                .map(|column| column.trim().to_string())
                .filter(|column| !column.is_empty())
                .collect();
            // An empty schema means "the schema this table lives in".
            if foreign_key
                .ref_schema
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            {
                foreign_key.ref_schema = Some(namespace.to_string());
            }
        }
        validate_foreign_keys(&foreign_keys, &desired.columns)?;
        desired.foreign_keys = foreign_keys;
    }

    // Renames are applied to the *current* view first, so the comparison does
    // not mistake them for a drop plus an add.
    let mut rename_statements: Vec<DdlStatement> = Vec::new();
    let mut current_for_diff = current.clone();
    for rename in renames {
        let from = rename.from.trim();
        let to = rename.to.trim();
        if from.is_empty() || to.is_empty() || from == to {
            continue;
        }
        let definition = desired
            .column(to)
            .or_else(|| current.column(from))
            .cloned()
            .ok_or_else(|| AppError::msg(format!("找不到要重命名的字段 {from}")))?;
        if current.column(from).is_none() {
            continue;
        }
        for column in &mut current_for_diff.columns {
            if column.name == from {
                column.name = to.to_string();
            }
        }
        rename_statements.push(DdlStatement {
            table: table.to_string(),
            category: "rename_column".into(),
            object: to.to_string(),
            sql: rename_column_ddl(
                table,
                from,
                to,
                &definition,
                target,
                Some(namespace),
                namespace,
            ),
        });
    }

    // Comments are editable here, so they must take part in the comparison.
    let options = CompareOptions {
        ignore_comments: false,
        ignore_case: false,
        compare_charset: false,
    };
    let diff = compare_everything(
        &[desired],
        &[current_for_diff],
        &[],
        &[],
        &[],
        &[],
        target,
        target,
        namespace,
        namespace,
        options,
    );

    // Only column work is allowed out of the table editor.
    let allowed = [
        "add_column",
        "drop_column",
        "modify_column",
        "column_comment",
        "create_index",
        "drop_index",
        "add_foreign_key",
        "drop_foreign_key",
        // A column being added with a placeholder default is dropped right after
        "drop_column_default",
    ];
    let sources: HashMap<String, TableStructure> = HashMap::new();
    let plan = generate(&diff, &sources, None, Some(namespace), &mut warnings);
    let mut statements: Vec<DdlStatement> = rename_statements;
    statements.extend(
        plan.into_iter()
            .filter(|statement| allowed.contains(&statement.category.as_str())),
    );
    if target == crate::dialect::Dialect::Mysql {
        statements = merge_mysql_index_statements(table, Some(namespace), statements);
    }
    Ok((statements, warnings))
}

#[tauri::command]
pub async fn preview_alter_table(
    state: State<'_, AppState>,
    connection_id: String,
    namespace: String,
    table: String,
    columns: Vec<ColumnInfo>,
    renames: Option<Vec<ColumnRename>>,
) -> AppResult<SchemaSqlPreview> {
    let pool = pool_for(&state, &connection_id).await?;
    let (statements, warnings) = plan_alter_table(
        &pool,
        &namespace,
        &table,
        &columns,
        None,
        None,
        &renames.unwrap_or_default(),
    )
    .await?;
    Ok(SchemaSqlPreview {
        statements,
        warnings,
    })
}

#[tauri::command]
pub async fn apply_alter_table(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: AlterTableRequest,
) -> AppResult<SyncRunResult> {
    let (reporter, job) = begin_job(&app, &state, request.job_id.as_deref());
    let pool = pool_for(&state, &request.connection_id).await?;
    let outcome = plan_alter_table(
        &pool,
        &request.namespace,
        &request.table,
        &request.columns,
        request.indexes.as_deref(),
        request.foreign_keys.as_deref(),
        &request.renames,
    )
    .await;
    let (statements, warnings) = outcome?;
    let sql: Vec<String> = statements.into_iter().map(|s| s.sql).collect();
    let started = Instant::now();
    let run = pool.run_statements(&sql, true, &reporter).await;
    end_job(&state, job);
    reporter.finish("done", "字段修改完成", "字段修改结束");
    Ok(summarize_ddl(
        run?,
        started.elapsed().as_millis() as u64,
        warnings,
    ))
}

// ---------------------------------------------------------------------------
// Excel import
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcelImportPlan {
    pub preview: ExcelSheetPreview,
    /// Columns of the target table that can receive data.
    pub columns: Vec<ColumnInfo>,
    /// Mapping guessed from matching names, used as the initial state.
    pub suggested: Vec<ColumnMapping>,
    pub primary_key: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportExcelRequest {
    pub connection_id: String,
    pub namespace: String,
    pub table: String,
    pub path: String,
    #[serde(default)]
    pub sheet: Option<String>,
    #[serde(default)]
    pub header_row: Option<usize>,
    #[serde(default)]
    pub mapping: Vec<ColumnMapping>,
    #[serde(default)]
    pub options: ImportOptions,
    #[serde(default)]
    pub job_id: Option<String>,
}

/// Read a sheet for previewing. No database involved, so it is safe to call
/// while the user is still choosing a file.
#[tauri::command]
pub async fn preview_excel(
    path: String,
    sheet: Option<String>,
    header_row: Option<usize>,
    sample: Option<usize>,
) -> AppResult<ExcelSheetPreview> {
    excel::preview(
        &path,
        sheet.as_deref(),
        header_row,
        sample.unwrap_or(30),
    )
}

/// Everything the mapping step needs in one round trip: the sheet, the columns
/// that can receive data, and a suggested mapping.
#[tauri::command]
pub async fn plan_excel_import(
    state: State<'_, AppState>,
    connection_id: String,
    namespace: String,
    table: String,
    path: String,
    sheet: Option<String>,
    header_row: Option<usize>,
) -> AppResult<ExcelImportPlan> {
    let preview = excel::preview(&path, sheet.as_deref(), header_row, 30)?;
    let pool = pool_for(&state, &connection_id).await?;
    let structure = introspect::load_table(&pool, &namespace, &table).await?;
    let columns: Vec<ColumnInfo> = structure
        .columns
        .iter()
        .filter(|column| column.is_writable())
        .cloned()
        .collect();
    let suggested = suggest_mapping(&structure, &preview.headers);
    Ok(ExcelImportPlan {
        preview,
        columns,
        suggested,
        primary_key: structure.primary_key.clone(),
    })
}

#[tauri::command]
pub async fn import_excel(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: ImportExcelRequest,
) -> AppResult<ImportOutcome> {
    if request.mapping.is_empty() {
        return Err(AppError::msg("请至少映射一个字段"));
    }
    let (reporter, job) = begin_job(&app, &state, request.job_id.as_deref());
    let pool = pool_for(&state, &request.connection_id).await?;
    let structure = introspect::load_table(&pool, &request.namespace, &request.table).await?;

    let (sheet, _, chosen) = excel::read_sheet(
        &request.path,
        request.sheet.as_deref(),
        request.header_row,
        None,
    )?;
    reporter.force(
        "import",
        "导入数据",
        0,
        Some(sheet.rows.len() as u64),
        format!("工作表 {chosen}：{} 行", sheet.rows.len()),
    );

    let outcome = run_import(
        &pool,
        &request.namespace,
        &structure,
        &sheet,
        &request.mapping,
        &request.options,
        &reporter,
    )
    .await;
    end_job(&state, job);
    outcome
}

// ---------------------------------------------------------------------------
// Excel export
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportExcelRequest {
    pub connection_id: String,
    pub namespace: String,
    pub table: String,
    /// Where to write the workbook; chosen by the caller's save dialog.
    pub path: String,
    /// The columns to write, in order, with optional renamed headers.
    pub columns: Vec<ExportColumn>,
    #[serde(default)]
    pub options: ExportOptions,
    #[serde(default)]
    pub job_id: Option<String>,
}

/// How many rows the export would write, so the dialog can warn before starting.
#[tauri::command]
pub async fn count_table_rows(
    state: State<'_, AppState>,
    connection_id: String,
    namespace: String,
    table: String,
) -> AppResult<Option<u64>> {
    let pool = pool_for(&state, &connection_id).await?;
    let structure = introspect::load_table(&pool, &namespace, &table).await?;
    let sql = format!(
        "SELECT COUNT(*) FROM {}",
        pool.dialect().qualified(Some(&namespace), &table)
    );
    let _ = structure;
    let rows = pool
        .fetch(&sql, &[], &[crate::db::value::ColumnKind::Int])
        .await?;
    Ok(rows
        .first()
        .and_then(|row| row.first())
        .and_then(|value| value.display())
        .and_then(|text| text.parse::<u64>().ok()))
}

#[tauri::command]
pub async fn export_excel(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: ExportExcelRequest,
) -> AppResult<ExportOutcome> {
    if request.columns.is_empty() {
        return Err(AppError::msg("请至少选择一列"));
    }
    let (reporter, job) = begin_job(&app, &state, request.job_id.as_deref());
    let pool = pool_for(&state, &request.connection_id).await?;
    let structure = introspect::load_table(&pool, &request.namespace, &request.table).await?;
    let outcome = excel_export::export(
        &pool,
        &request.namespace,
        &structure,
        &request.columns,
        &request.options,
        &request.path,
        &reporter,
    )
    .await;
    end_job(&state, job);
    outcome
}

// ---------------------------------------------------------------------------
// Saved queries and the SQL console
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateQueryRequest {
    pub connection_id: String,
    #[serde(default)]
    pub namespace: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveQueryRequest {
    pub path: String,
    pub sql: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPathRequest {
    pub path: String,
}

#[tauri::command]
pub async fn list_queries(
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<Vec<SavedQuery>> {
    queries::list(&state.store, &connection_id)
}

#[tauri::command]
pub async fn create_query(
    state: State<'_, AppState>,
    request: CreateQueryRequest,
) -> AppResult<SavedQuery> {
    queries::create(
        &state.store,
        &request.connection_id,
        &request.namespace,
        &request.name,
    )
}

#[tauri::command]
pub async fn read_query(
    state: State<'_, AppState>,
    request: QueryPathRequest,
) -> AppResult<SavedQuery> {
    queries::read(&state.store, &request.path)
}

#[tauri::command]
pub async fn save_query(
    state: State<'_, AppState>,
    request: SaveQueryRequest,
) -> AppResult<SavedQuery> {
    queries::save(&state.store, &request.path, &request.sql)
}

#[tauri::command]
pub async fn rename_query(
    state: State<'_, AppState>,
    path: String,
    name: String,
) -> AppResult<SavedQuery> {
    queries::rename(&state.store, &path, &name)
}

#[tauri::command]
pub async fn duplicate_query(
    state: State<'_, AppState>,
    request: QueryPathRequest,
) -> AppResult<SavedQuery> {
    queries::duplicate(&state.store, &request.path)
}

#[tauri::command]
pub async fn delete_query(
    state: State<'_, AppState>,
    request: QueryPathRequest,
) -> AppResult<()> {
    queries::remove(&state.store, &request.path)
}

#[tauri::command]
pub async fn queries_directory(
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<String> {
    queries::directory(&state.store, &connection_id)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionColumn {
    pub table: String,
    pub column: String,
    pub data_type: String,
}

/// Everything the editor offers as a completion.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionCatalog {
    /// Databases on the connection.
    pub databases: Vec<String>,
    pub tables: Vec<String>,
    pub columns: Vec<CompletionColumn>,
    /// Views are suggested alongside tables but flagged, so the UI can show a
    /// different marker.
    pub views: Vec<String>,
}

/// Read the names a query might reference, for editor completion.
#[tauri::command]
pub async fn completion_catalog(
    state: State<'_, AppState>,
    connection_id: String,
    namespace: String,
) -> AppResult<CompletionCatalog> {
    let pool = pool_for(&state, &connection_id).await?;
    let databases = introspect::list_namespaces(&pool)
        .await?
        .into_iter()
        .map(|entry| entry.name)
        .collect();

    // One query for every column in the schema: cheaper than a round trip per
    // table, and the editor needs all of them at once anyway.
    let (columns_table, columns_column, type_column, schema_column) = match pool.dialect() {
        crate::dialect::Dialect::Mysql => (
            "TABLE_NAME",
            "COLUMN_NAME",
            "DATA_TYPE",
            "TABLE_SCHEMA",
        ),
        crate::dialect::Dialect::Postgres => (
            "table_name",
            "column_name",
            "data_type",
            "table_schema",
        ),
    };
    let param = match pool.dialect() {
        crate::dialect::Dialect::Mysql => "?",
        crate::dialect::Dialect::Postgres => "$1",
    };
    let sql = format!(
        "SELECT {columns_table}, {columns_column}, {type_column} FROM information_schema.columns \
         WHERE {schema_column} = {param} ORDER BY {columns_table}, ordinal_position"
    );
    let rows = pool
        .fetch(&sql, &[&namespace], &[
            crate::db::value::ColumnKind::Str,
            crate::db::value::ColumnKind::Str,
            crate::db::value::ColumnKind::Str,
        ])
        .await?;
    let columns = rows
        .into_iter()
        .filter_map(|row| {
            // `display()` renders any decoded value as text and returns `None`
            // for NULL, which is exactly what a name column needs.
            Some(CompletionColumn {
                table: row.first()?.display()?,
                column: row.get(1)?.display()?,
                data_type: row.get(2).and_then(|value| value.display()).unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();

    let summaries = introspect::list_tables(&pool, &namespace).await?;
    let mut tables = Vec::new();
    let mut views = Vec::new();
    for summary in summaries {
        if summary.table_type.to_uppercase().contains("VIEW") {
            views.push(summary.name);
        } else {
            tables.push(summary.name);
        }
    }

    Ok(CompletionCatalog {
        databases,
        tables,
        columns,
        views,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlColumn {
    pub name: String,
    pub data_type: String,
    pub kind: crate::db::value::ColumnKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlStatementResult {
    pub sql: String,
    pub ok: bool,
    pub error: Option<String>,
    pub rows_affected: u64,
    pub elapsed_ms: u64,
    /// Present when the statement produced rows.
    pub columns: Vec<SqlColumn>,
    pub rows: Vec<Vec<crate::db::value::Cell>>,
    /// True when the console stopped before finishing the script.
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlRunResult {
    pub results: Vec<SqlStatementResult>,
    pub elapsed_ms: u64,
    pub cancelled: bool,
}

/// Largest page the data grid will fetch at once.
pub const MAX_PAGE_ROWS: u64 = 1_000;

/// Rows shown per statement before the console stops collecting.
const SQL_CONSOLE_ROW_LIMIT: usize = 5_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSqlRequest {
    pub connection_id: String,
    /// The database to run against; also the `search_path` on PostgreSQL.
    pub namespace: String,
    pub sql: String,
    #[serde(default)]
    pub job_id: Option<String>,
}

/// Run a script the user wrote and report each statement's outcome.
#[tauri::command]
pub async fn run_sql(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: RunSqlRequest,
) -> AppResult<SqlRunResult> {
    let (reporter, job) = begin_job(&app, &state, request.job_id.as_deref());
    let outcome = run_sql_for(
        &state,
        &request.connection_id,
        &request.namespace,
        &request.sql,
        &reporter,
    )
    .await;
    end_job(&state, job);
    outcome
}

pub(crate) async fn run_sql_for(
    state: &AppState,
    connection_id: &str,
    namespace: &str,
    sql: &str,
    reporter: &ProgressReporter,
) -> AppResult<SqlRunResult> {
    let pool = pool_for(state, connection_id).await?;
    run_sql_with(&pool, namespace, sql, reporter).await
}

/// Run a script against a pool. Shared by the command and the live tests so both
/// exercise the same path.
pub(crate) async fn run_sql_with(
    pool: &Pool,
    namespace: &str,
    sql: &str,
    reporter: &ProgressReporter,
) -> AppResult<SqlRunResult> {
    let statements = split_statements(sql);
    if statements.is_empty() {
        return Err(AppError::msg("没有可执行的语句"));
    }

    let started = Instant::now();
    let total = statements.len();
    reporter.force(
        "sql",
        "执行 SQL",
        0,
        Some(total as u64),
        format!("共 {total} 条语句"),
    );

    // One dedicated connection for the whole script, so the database selected
    // here applies to every statement.
    let outcomes = crate::db::run_console(
        pool,
        namespace,
        &statements,
        SQL_CONSOLE_ROW_LIMIT + 1,
        reporter,
    )
    .await?;

    let mut results = Vec::with_capacity(outcomes.len());
    for (index, (statement, outcome)) in statements.iter().zip(outcomes).enumerate() {
        let mut entry = SqlStatementResult {
            sql: statement.clone(),
            ok: true,
            error: None,
            rows_affected: 0,
            elapsed_ms: 0,
            columns: Vec::new(),
            rows: Vec::new(),
            truncated: false,
        };
        match outcome {
            crate::db::ConsoleOutcome::Rows(result) => {
                entry.columns = result
                    .columns
                    .iter()
                    .map(|column| SqlColumn {
                        name: column.name.clone(),
                        data_type: column.type_name.clone(),
                        kind: column.kind,
                    })
                    .collect();
                let mut rows = result.rows;
                if rows.len() > SQL_CONSOLE_ROW_LIMIT {
                    rows.truncate(SQL_CONSOLE_ROW_LIMIT);
                    entry.truncated = true;
                }
                entry.rows = rows
                    .into_iter()
                    .map(|row| row.iter().map(|value| value.to_cell()).collect())
                    .collect();
            }
            crate::db::ConsoleOutcome::Affected(count) => entry.rows_affected = count,
            crate::db::ConsoleOutcome::Failed(error) => {
                entry.ok = false;
                entry.error = Some(error);
            }
        }
        let failed = !entry.ok;
        results.push(entry);
        reporter.tick(
            "sql",
            "执行 SQL",
            index as u64 + 1,
            Some(total as u64),
            format!("已执行 {}/{}", index + 1, total),
        );
        // Stopping at the first failure is what a client tool does, and it keeps
        // a broken script from doing further damage.
        if failed {
            break;
        }
    }

    let cancelled = reporter.is_cancelled() && results.len() < statements.len();
    reporter.finish("done", "SQL 执行完成", "执行结束");
    Ok(SqlRunResult {
        results,
        elapsed_ms: started.elapsed().as_millis() as u64,
        cancelled,
    })
}

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn run_query(
    state: State<'_, AppState>,
    connection_id: String,
    sql: String,
) -> AppResult<SyncRunResult> {
    let pool = pool_for(&state, &connection_id).await?;
    let statements: Vec<String> = split_statements(&sql);
    let started = Instant::now();
    let reporter = ProgressReporter::inert();
    let run = pool
        .run_script_reporting(&statements, false, &reporter)
        .await;
    Ok(summarize(
        run.outcomes,
        started.elapsed().as_millis() as u64,
        "执行",
        Vec::new(),
        run.cancelled,
    ))
}

/// Naive but safe-enough splitter: honours single/double quotes and comments.
pub(crate) fn split_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_backtick = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    while let Some(ch) = chars.next() {
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
            }
            current.push(ch);
            continue;
        }
        if in_block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
                current.push_str("*/");
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\'' if !in_double && !in_backtick => {
                in_single = !in_single;
                current.push(ch);
            }
            '"' if !in_single && !in_backtick => {
                in_double = !in_double;
                current.push(ch);
            }
            '`' if !in_single && !in_double => {
                in_backtick = !in_backtick;
                current.push(ch);
            }
            '-' if !in_single && !in_double && !in_backtick && chars.peek() == Some(&'-') => {
                chars.next();
                in_line_comment = true;
                current.push_str("--");
            }
            '/' if !in_single && !in_double && !in_backtick && chars.peek() == Some(&'*') => {
                chars.next();
                in_block_comment = true;
                current.push_str("/*");
            }
            ';' if !in_single && !in_double && !in_backtick => {
                let trimmed = current.trim();
                // A statement may be preceded by comments; only a body that is
                // empty once the leading comments are stripped is discarded.
                if !strip_leading_comments(trimmed).is_empty() {
                    out.push(format!("{trimmed};"));
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let trimmed = current.trim();
    if !strip_leading_comments(trimmed).is_empty() {
        out.push(trimmed.to_string());
    }
    out
}

/// Remove leading whitespace, `--` line comments and `/* */` blocks.
fn strip_leading_comments(mut text: &str) -> &str {
    loop {
        let trimmed = text.trim_start();
        if let Some(rest) = trimmed.strip_prefix("--") {
            text = match rest.find('\n') {
                Some(idx) => &rest[idx + 1..],
                None => "",
            };
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/*") {
            text = match rest.find("*/") {
                Some(idx) => &rest[idx + 2..],
                None => "",
            };
            continue;
        }
        return trimmed;
    }
}

#[tauri::command]
pub async fn export_text_file(path: String, content: String) -> AppResult<()> {
    std::fs::write(&path, content)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageInfo {
    pub dir: String,
    pub warning: Option<String>,
}

#[tauri::command]
pub fn app_storage_dir(state: State<'_, AppState>) -> StorageInfo {
    StorageInfo {
        dir: state.store.storage_dir().to_string_lossy().to_string(),
        warning: state.store.storage_warning(),
    }
}

#[tauri::command]
pub fn clear_caches(state: State<'_, AppState>) {
    state.compares.clear();
    state.plans.clear();
}

pub fn handlers() -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        list_connections,
        save_connection,
        delete_connection,
        reorder_connections,
        test_connection,
        open_connection,
        close_connection,
        list_namespaces,
        list_tables,
        get_table_structure,
        preview_table_data,
        compare_schema,
        preview_schema_sql,
        apply_schema_sync,
        compare_data,
        apply_data_sync,
        run_query,
        export_text_file,
        app_storage_dir,
        clear_caches,
        cancel_job,
        preview_drop_tables,
        apply_drop_tables,
        rename_table,
        preview_alter_table,
        apply_alter_table,
        preview_create_table,
        create_table,
        preview_excel,
        plan_excel_import,
        import_excel,
        update_table_cell,
        count_table_rows,
        export_excel,
        list_queries,
        create_query,
        read_query,
        save_query,
        rename_query,
        duplicate_query,
        delete_query,
        queries_directory,
        run_sql,
        completion_catalog,
    ]
}

/// Kept for symmetry with `ChangeKind` imports used in tests / future filters.
#[allow(dead_code)]
fn _kind_is_change(kind: ChangeKind) -> bool {
    !matches!(kind, ChangeKind::Identical)
}
