use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::State;

use std::cmp::Ordering;

use crate::data::stream::{compare_key_tuples, StreamPlan, TableStream};
use crate::data::{
    build_delete_statements, build_insert_statements, build_read_plan, build_update_statements,
    column_index, preview_deletes, preview_inserts, preview_updates, read_rows, ColumnPair,
    DataDiffResult, DataDiffSummary, RowChange, TableEndpoint, PREVIEW_ROWS, SQL_PREVIEW_LIMIT,
};
use crate::data::plan::DataPlan;
use crate::db::value::{ColumnMeta, Value};
use crate::db::{open_pool, Pool, StatementOutcome};
use crate::dialect::Dialect;
use crate::error::{AppError, AppResult};
use crate::jobs::ProgressReporter;
use crate::schema::ddl::{generate, DdlStatement, SyncSelection};
use crate::schema::diff::{compare_everything, ChangeKind, CompareOptions, SchemaDiff};
use crate::schema::introspect;
use crate::schema::model::{NamespaceInfo, TableStructure, TableSummary};
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
) -> AppResult<TableData> {
    let structure = introspect::load_table(pool, namespace, table).await?;
    let limit = limit.unwrap_or(200).clamp(1, 5000);
    let offset = offset.unwrap_or(0);
    let plan = build_read_plan(
        pool.dialect(),
        namespace,
        table,
        &structure,
        &structure.primary_key,
        Some(limit + 1),
        Some(offset),
    );
    let started = Instant::now();
    // One extra row is fetched so `has_more` is known without a second query.
    let mut rows = read_rows(pool, &plan).await?;
    let has_more = rows.len() as u64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }
    let columns = structure
        .columns
        .iter()
        .map(|c| ColumnMeta {
            name: c.name.clone(),
            data_type: c.full_type.clone(),
            kind: c.kind(),
            nullable: c.nullable,
            primary_key: structure.primary_key.contains(&c.name),
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
        elapsed_ms: started.elapsed().as_millis() as u64,
        sql: plan.select_sql,
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
) -> AppResult<TableData> {
    let pool = pool_for(&state, &connection_id).await?;
    preview_table_data_core(&pool, &namespace, &table, limit, offset).await
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
    /// Non-fatal notes, e.g. a generated column that had to be downgraded.
    pub warnings: Vec<String>,
    pub cancelled: bool,
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
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplySchemaRequest {
    pub compare_id: String,
    #[serde(default)]
    pub selection: Option<SyncSelection>,
    #[serde(default)]
    pub stop_on_error: bool,
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
        .run_script_reporting(&sql, request.stop_on_error, &reporter)
        .await;
    end_job(&state, job);
    reporter.finish("done", "结构同步完成", "结构同步结束");
    Ok(summarize(
        run.outcomes,
        started.elapsed().as_millis() as u64,
        "结构同步",
        warnings,
        run.cancelled,
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
    ]
}

/// Kept for symmetry with `ChangeKind` imports used in tests / future filters.
#[allow(dead_code)]
fn _kind_is_change(kind: ChangeKind) -> bool {
    !matches!(kind, ChangeKind::Identical)
}
