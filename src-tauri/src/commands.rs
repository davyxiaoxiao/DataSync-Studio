use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::data::{
    build_delete_statements, build_insert_statements, build_read_plan, build_update_statements,
    column_index, compare_rows, preview_deletes, preview_inserts, preview_updates,
    read_rows, ColumnPair, CompareInput, DataDiffResult, DataDiffSummary, TableEndpoint,
    SQL_PREVIEW_LIMIT,
};
use crate::data::plan::DataPlan;
use crate::db::value::ColumnMeta;
use crate::db::{open_pool, Pool, StatementOutcome};
use crate::dialect::Dialect;
use crate::error::{AppError, AppResult};
use crate::schema::ddl::{generate, DdlStatement, SyncSelection};
use crate::schema::diff::{compare, ChangeKind, CompareOptions, SchemaDiff};
use crate::schema::introspect;
use crate::schema::model::{NamespaceInfo, TableStructure, TableSummary};
use crate::state::{AppState, SchemaCompare};
use crate::store::ConnectionConfig;

const DEFAULT_ROW_LIMIT: u64 = 200_000;
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
    let structure = introspect::load_table(&pool, &namespace, &table).await?;
    let limit = limit.unwrap_or(200).clamp(1, 5000);
    let offset = offset.unwrap_or(0);
    let plan = build_read_plan(
        pool.dialect(),
        &namespace,
        &table,
        &structure,
        &structure.primary_key,
        Some(limit + 1),
        Some(offset),
    );
    let started = Instant::now();
    let mut rows = read_rows(&pool, &plan).await?;
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
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareSchemaResult {
    pub compare_id: String,
    pub diff: SchemaDiff,
    pub warnings: Vec<String>,
}

async fn load_structures(
    pool: &Pool,
    namespace: &str,
    only: Option<&Vec<String>>,
) -> AppResult<(Vec<TableStructure>, Vec<String>)> {
    let mut warnings = Vec::new();
    let tables = introspect::list_tables(pool, namespace).await?;
    let mut structures = Vec::new();
    for table in &tables {
        if let Some(filter) = only {
            if !filter.iter().any(|f| f == &table.name) {
                continue;
            }
        }
        if !introspect::is_syncable(table) {
            warnings.push(format!("已跳过视图/外部表: {}", table.name));
            continue;
        }
        match introspect::load_table(pool, namespace, &table.name).await {
            Ok(structure) => structures.push(structure),
            Err(err) => warnings.push(format!("读取 {} 结构失败: {err}", table.name)),
        }
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
) -> AppResult<(SchemaDiff, HashMap<String, TableStructure>, Vec<String>)> {
    let (source_structures, mut warnings) =
        load_structures(source_pool, source_namespace, tables).await?;
    let (target_structures, target_warnings) =
        load_structures(target_pool, target_namespace, None).await?;
    warnings.extend(target_warnings);

    let mut diff = compare(
        &source_structures,
        &target_structures,
        source_pool.dialect(),
        target_pool.dialect(),
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
    state: State<'_, AppState>,
    request: CompareSchemaRequest,
) -> AppResult<CompareSchemaResult> {
    let source_pool = pool_for(&state, &request.source_connection_id).await?;
    let target_pool = pool_for(&state, &request.target_connection_id).await?;

    let options = CompareOptions {
        ignore_comments: request.ignore_comments.unwrap_or(true),
        ignore_case: request.ignore_case.unwrap_or(false),
        compare_charset: request.compare_charset.unwrap_or(false),
    };
    let (diff, structure_map, warnings) = compare_schema_core(
        &source_pool,
        &request.source_namespace,
        &target_pool,
        &request.target_namespace,
        options,
        request.tables.as_ref(),
    )
    .await?;

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
}

const SAMPLE_LIMIT: usize = 300;

fn summarize(
    outcomes: Vec<StatementOutcome>,
    elapsed_ms: u64,
    label: &str,
    warnings: Vec<String>,
) -> SyncRunResult {
    let total = outcomes.len();
    let failed = outcomes.iter().filter(|o| !o.ok).count();
    let succeeded = total - failed;
    let failures: Vec<StatementOutcome> = outcomes.iter().filter(|o| !o.ok).cloned().collect();
    let sample: Vec<StatementOutcome> = outcomes.into_iter().take(SAMPLE_LIMIT).collect();
    let message = if failed == 0 {
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
}

#[tauri::command]
pub async fn apply_schema_sync(
    state: State<'_, AppState>,
    request: ApplySchemaRequest,
) -> AppResult<SyncRunResult> {
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
        return Ok(summarize(Vec::new(), 0, "结构同步", Vec::new()));
    }

    let pool = pool_for(&state, &target_connection).await?;
    let sql: Vec<String> = statements.into_iter().map(|s| s.sql).collect();
    let started = Instant::now();
    let outcomes = pool.run_script(&sql, request.stop_on_error).await;
    Ok(summarize(outcomes, started.elapsed().as_millis() as u64, "结构同步", warnings))
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
    pub row_limit: Option<u64>,
    #[serde(default)]
    pub batch_size: Option<usize>,
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
/// Engine-agnostic core of a data comparison. Returns the UI payload plus the
/// ready-to-run statement plan.
pub(crate) async fn compare_data_core(
    source_pool: &Pool,
    source: &TableEndpoint,
    target_pool: &Pool,
    target: &TableEndpoint,
    requested_keys: &[ColumnPair],
    requested_map: &[ColumnPair],
    row_limit: u64,
    batch_size: usize,
) -> AppResult<(DataDiffResult, DataPlan)> {
    let source_dialect = source_pool.dialect();
    let target_dialect = target_pool.dialect();

    let mut warnings = Vec::new();
    let source_structure =
        introspect::load_table(source_pool, &source.namespace, &source.table)
            .await?;
    let target_structure =
        introspect::load_table(target_pool, &target.namespace, &target.table)
            .await?;

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
    if source_dialect != target_dialect {
        warnings.push(format!(
            "跨数据库对比：{} → {}，类型不同可能导致比较结果偏保守",
            source_dialect.label(),
            target_dialect.label()
        ));
    }

    let source_order: Vec<String> = key_pairs.iter().map(|p| p.source.clone()).collect();
    let target_order: Vec<String> = key_pairs.iter().map(|p| p.target.clone()).collect();

    let source_plan = build_read_plan(
        source_dialect,
        &source.namespace,
        &source.table,
        &source_structure,
        &source_order,
        Some(row_limit + 1),
        None,
    );
    let target_plan = build_read_plan(
        target_dialect,
        &target.namespace,
        &target.table,
        &target_structure,
        &target_order,
        Some(row_limit + 1),
        None,
    );

    let source_rows = read_rows(source_pool, &source_plan).await?;
    let target_rows = read_rows(target_pool, &target_plan).await?;
    let mut truncated = false;
    let mut source_rows = source_rows;
    let mut target_rows = target_rows;
    if source_rows.len() as u64 > row_limit {
        source_rows.truncate(row_limit as usize);
        truncated = true;
    }
    if target_rows.len() as u64 > row_limit {
        target_rows.truncate(row_limit as usize);
        truncated = true;
    }
    if truncated {
        warnings.push(format!(
            "数据量超过 {row_limit} 行，仅比较了前 {row_limit} 行（按主键排序）"
        ));
    }

    let target_kinds: Vec<crate::db::value::ColumnKind> =
        target_structure.columns.iter().map(|c| c.kind()).collect();

    let result = compare_rows(CompareInput {
        source_rows: source_rows.clone(),
        target_rows: target_rows.clone(),
        source_columns: &source_plan.columns,
        target_columns: &target_plan.columns,
        column_pairs: &column_pairs,
        key_pairs: &key_pairs,
        target_kinds: &target_kinds,
    })?;

    let target_columns: Vec<String> = target_structure
        .columns
        .iter()
        .map(|c| c.name.clone())
        .collect();

    let insert_sql = build_insert_statements(
        target_dialect,
        &target.namespace,
        &target.table,
        &source_plan.columns,
        &target_columns,
        &target_kinds,
        &column_pairs,
        &result.inserts,
        batch_size,
    );
    let update_sql = build_update_statements(
        target_dialect,
        &target.namespace,
        &target.table,
        &target_columns,
        &target_kinds,
        &key_pairs,
        &result.updates,
    );
    let delete_sql = build_delete_statements(
        target_dialect,
        &target.namespace,
        &target.table,
        &target_columns,
        &target_kinds,
        &key_pairs,
        &result.deletes,
    );

    let mut sql_preview: Vec<String> = Vec::new();
    for statement in delete_sql.iter().chain(update_sql.iter()).chain(insert_sql.iter()) {
        if sql_preview.len() >= SQL_PREVIEW_LIMIT {
            break;
        }
        sql_preview.push(statement.clone());
    }

    let summary = DataDiffSummary {
        source_rows: source_rows.len(),
        target_rows: target_rows.len(),
        inserts: result.inserts.len(),
        updates: result.updates.len(),
        deletes: result.deletes.len(),
        unchanged: result.unchanged,
    };

    let insert_preview = preview_inserts(
        &source_plan.columns,
        &column_pairs,
        &key_pairs,
        &result.inserts,
    );
    let update_preview = preview_updates(&target_columns, &key_pairs, &result.updates);
    let delete_preview = preview_deletes(
        &target_columns,
        &column_pairs,
        &key_pairs,
        &result.deletes,
    );
    let preview_truncated = summary.inserts > insert_preview.len()
        || summary.updates > update_preview.len()
        || summary.deletes > delete_preview.len();

    // Sanity check that every mapped source column exists on the source side.
    for pair in &column_pairs {
        if column_index(&source_plan.columns, &pair.source).is_none() {
            warnings.push(format!("源表缺少字段 {}", pair.source));
        }
    }

    let plan = DataPlan {
        insert: insert_sql,
        update: update_sql,
        delete: delete_sql,
        created_at: Instant::now(),
    };

    Ok((DataDiffResult {
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
    }, plan))
}

#[tauri::command]
pub async fn compare_data(
    state: State<'_, AppState>,
    request: CompareDataRequest,
) -> AppResult<DataDiffResult> {
    let source_pool = pool_for(&state, &request.source.connection_id).await?;
    let target_pool = pool_for(&state, &request.target.connection_id).await?;
    let (mut result, plan) = compare_data_core(
        &source_pool,
        &request.source,
        &target_pool,
        &request.target,
        &request.key_columns,
        &request.column_map,
        request.row_limit.unwrap_or(DEFAULT_ROW_LIMIT),
        request.batch_size.unwrap_or(DEFAULT_BATCH_SIZE).clamp(1, 1000),
    )
    .await?;
    result.plan_id = state.plans.put(plan);
    Ok(result)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyDataRequest {
    pub plan_id: String,
    pub connection_id: String,
    #[serde(default = "default_true")]
    pub insert: bool,
    #[serde(default = "default_true")]
    pub update: bool,
    #[serde(default = "default_true")]
    pub delete: bool,
    #[serde(default = "default_true")]
    pub use_transaction: bool,
}

fn default_true() -> bool {
    true
}

#[tauri::command]
pub async fn apply_data_sync(
    state: State<'_, AppState>,
    request: ApplyDataRequest,
) -> AppResult<SyncRunResult> {
    let statements = state
        .plans
        .with(&request.plan_id, |plan| {
            plan.select(request.insert, request.update, request.delete)
        })
        .ok_or_else(|| AppError::msg("对比计划已过期，请重新对比"))?;

    if statements.is_empty() {
        return Ok(summarize(Vec::new(), 0, "数据同步", Vec::new()));
    }

    let pool = pool_for(&state, &request.connection_id).await?;
    let started = Instant::now();

    if request.use_transaction {
        // MySQL cannot roll back DDL but DML is transactional on both engines.
        let outcomes = pool.run_in_transaction(&statements).await?;
        state.plans.remove(&request.plan_id);
        return Ok(summarize(
            outcomes,
            started.elapsed().as_millis() as u64,
            "数据同步",
            Vec::new(),
        ));
    }

    let outcomes = pool.run_script(&statements, false).await;
    state.plans.remove(&request.plan_id);
    Ok(summarize(
        outcomes,
        started.elapsed().as_millis() as u64,
        "数据同步",
        Vec::new(),
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
    let outcomes = pool.run_script(&statements, false).await;
    Ok(summarize(outcomes, started.elapsed().as_millis() as u64, "执行", Vec::new()))
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
    ]
}

/// Kept for symmetry with `ChangeKind` imports used in tests / future filters.
#[allow(dead_code)]
fn _kind_is_change(kind: ChangeKind) -> bool {
    !matches!(kind, ChangeKind::Identical)
}
