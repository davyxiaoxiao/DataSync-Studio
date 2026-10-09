pub mod value;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlSslMode};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};
use sqlx::{Executor, Row};

use crate::dialect::Dialect;
use crate::error::{AppError, AppResult};
use crate::jobs::ProgressReporter;
use crate::store::ConnectionConfig;
use value::{
    decode_mysql_row, decode_mysql_row_lenient, decode_pg_row, decode_pg_row_lenient,
    ColumnKind, Value,
};

#[derive(Clone)]
pub enum Pool {
    MySql(MySqlPool),
    Postgres(PgPool),
}

impl Pool {
    pub fn dialect(&self) -> Dialect {
        match self {
            Pool::MySql(_) => Dialect::Mysql,
            Pool::Postgres(_) => Dialect::Postgres,
        }
    }

    pub async fn close(&self) {
        match self {
            Pool::MySql(p) => p.close().await,
            Pool::Postgres(p) => p.close().await,
        }
    }

    pub async fn server_version(&self) -> AppResult<String> {
        match self {
            Pool::MySql(p) => {
                let row = sqlx::query("SELECT VERSION() AS v").fetch_one(p).await?;
                Ok(row.try_get::<String, _>("v").unwrap_or_default())
            }
            Pool::Postgres(p) => {
                let row = sqlx::query("SHOW server_version").fetch_one(p).await?;
                Ok(row.try_get::<String, _>("server_version").unwrap_or_default())
            }
        }
    }

    /// Run a query and decode every cell according to `kinds`.
    pub async fn fetch(
        &self,
        sql: &str,
        params: &[&str],
        kinds: &[ColumnKind],
    ) -> AppResult<Vec<Vec<Value>>> {
        match self {
            Pool::MySql(pool) => {
                let mut q = sqlx::query(sql);
                for p in params {
                    q = q.bind(*p);
                }
                let rows = q.fetch_all(pool).await?;
                rows.iter().map(|r| decode_mysql_row(r, kinds)).collect()
            }
            Pool::Postgres(pool) => {
                let mut q = sqlx::query(sql);
                for p in params {
                    q = q.bind(*p);
                }
                let rows = q.fetch_all(pool).await?;
                rows.iter().map(|r| decode_pg_row(r, kinds)).collect()
            }
        }
    }

    pub async fn execute(&self, sql: &str) -> AppResult<u64> {
        match self {
            Pool::MySql(pool) => {
                let res = pool.execute(sql).await?;
                Ok(res.rows_affected())
            }
            Pool::Postgres(pool) => {
                let res = pool.execute(sql).await?;
                Ok(res.rows_affected())
            }
        }
    }

    /// Execute a list of statements one round trip at a time, reporting progress
    /// and honouring cancellation between statements.
    ///
    /// Statement failures never abort the remaining work unless `stop_on_error`
    /// is set: DDL scripts are frequently partially valid (a column that already
    /// matches, for instance).
    pub async fn run_script_reporting(
        &self,
        statements: &[String],
        stop_on_error: bool,
        reporter: &ProgressReporter,
    ) -> ScriptRun {
        let total = statements.len() as u64;
        let mut outcomes = Vec::with_capacity(statements.len());
        reporter.force("apply", "执行语句", 0, Some(total), format!("共 {total} 条语句"));

        for (index, sql) in statements.iter().enumerate() {
            if reporter.is_cancelled() {
                return ScriptRun {
                    outcomes,
                    cancelled: true,
                };
            }
            let started = Instant::now();
            match self.execute(sql).await {
                Ok(affected) => outcomes.push(StatementOutcome {
                    sql: sql.clone(),
                    ok: true,
                    rows_affected: affected,
                    error: None,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                }),
                Err(err) => {
                    outcomes.push(StatementOutcome {
                        sql: sql.clone(),
                        ok: false,
                        rows_affected: 0,
                        error: Some(err.to_string()),
                        elapsed_ms: started.elapsed().as_millis() as u64,
                    });
                    if stop_on_error {
                        break;
                    }
                }
            }
            reporter.tick(
                "apply",
                "执行语句",
                index as u64 + 1,
                Some(total),
                format!("已执行 {}/{} 条", index + 1, total),
            );
        }

        ScriptRun {
            outcomes,
            cancelled: false,
        }
    }

    /// Apply a DDL plan.
    ///
    /// PostgreSQL gets a transaction around the whole plan, so a failure leaves
    /// the target exactly as it was. MySQL commits every DDL statement
    /// implicitly and cannot roll back, so there execution stops at the first
    /// failure and the caller is told that the change is partial.
    pub async fn run_statements(
        &self,
        statements: &[String],
        use_transaction: bool,
        reporter: &ProgressReporter,
    ) -> AppResult<StatementRun> {
        let total = statements.len();
        if statements.is_empty() {
            return Ok(StatementRun {
                transactional: self.supports_ddl_transaction(),
                ..StatementRun::default()
            });
        }

        if !(use_transaction && self.supports_ddl_transaction()) {
            let run = self.run_script_reporting(statements, true, reporter).await;
            let attempted = run.outcomes.len();
            return Ok(StatementRun {
                outcomes: run.outcomes,
                cancelled: run.cancelled,
                rolled_back: false,
                transactional: false,
                skipped: total.saturating_sub(attempted),
            });
        }

        reporter.force(
            "apply",
            "执行 DDL",
            0,
            Some(total as u64),
            format!("共 {total} 条语句（单事务）"),
        );

        macro_rules! ddl_tx {
            ($pool:expr) => {{
                let mut tx = $pool.begin().await?;
                let mut outcomes = Vec::with_capacity(total);
                for (index, sql) in statements.iter().enumerate() {
                    if reporter.is_cancelled() {
                        tx.rollback().await.ok();
                        return Ok(StatementRun {
                            outcomes,
                            cancelled: true,
                            rolled_back: true,
                            transactional: true,
                            skipped: total - index,
                        });
                    }
                    let started = Instant::now();
                    match tx.execute(sql.as_str()).await {
                        Ok(res) => outcomes.push(StatementOutcome {
                            sql: sql.clone(),
                            ok: true,
                            rows_affected: res.rows_affected(),
                            error: None,
                            elapsed_ms: started.elapsed().as_millis() as u64,
                        }),
                        Err(err) => {
                            let elapsed = started.elapsed().as_millis() as u64;
                            outcomes.push(StatementOutcome {
                                sql: sql.clone(),
                                ok: false,
                                rows_affected: 0,
                                error: Some(err.to_string()),
                                elapsed_ms: elapsed,
                            });
                            // Undo everything, including the statements that had
                            // already succeeded.
                            tx.rollback().await.ok();
                            return Ok(StatementRun {
                                outcomes,
                                cancelled: false,
                                rolled_back: true,
                                transactional: true,
                                skipped: total - index - 1,
                            });
                        }
                    }
                    reporter.tick(
                        "apply",
                        "执行 DDL",
                        index as u64 + 1,
                        Some(total as u64),
                        format!("已执行 {}/{} 条", index + 1, total),
                    );
                }
                tx.commit().await?;
                Ok(StatementRun {
                    outcomes,
                    cancelled: false,
                    rolled_back: false,
                    transactional: true,
                    skipped: 0,
                })
            }};
        }

        match self {
            Pool::Postgres(pool) => ddl_tx!(pool),
            // Guarded by `supports_ddl_transaction` above.
            Pool::MySql(_) => unreachable!("MySQL does not support transactional DDL"),
        }
    }

    /// Whether DDL can be rolled back on this engine.
    pub fn supports_ddl_transaction(&self) -> bool {
        matches!(self, Pool::Postgres(_))
    }

    /// Execute statements inside a single transaction, reporting progress.
    /// Cancelling or failing rolls the whole transaction back.
    pub async fn run_in_transaction_reporting(
        &self,
        statements: &[String],
        reporter: &ProgressReporter,
    ) -> AppResult<ScriptRun> {
        let total = statements.len() as u64;
        reporter.force("apply", "事务执行", 0, Some(total), format!("共 {total} 条语句"));
        // The two transaction types are distinct, so the body is expanded per
        // driver by a macro instead of a generic (they do not unify).
        macro_rules! run_tx {
            ($pool:expr) => {{
                let mut tx = $pool.begin().await?;
                let mut outcomes = Vec::with_capacity(statements.len());
                for (index, sql) in statements.iter().enumerate() {
                    if reporter.is_cancelled() {
                        tx.rollback().await.ok();
                        return Ok(ScriptRun {
                            outcomes,
                            cancelled: true,
                        });
                    }
                    let started = Instant::now();
                    match tx.execute(sql.as_str()).await {
                        Ok(res) => outcomes.push(StatementOutcome {
                            sql: sql.clone(),
                            ok: true,
                            rows_affected: res.rows_affected(),
                            error: None,
                            elapsed_ms: started.elapsed().as_millis() as u64,
                        }),
                        Err(err) => {
                            tx.rollback().await.ok();
                            return Err(AppError::msg(format!(
                                "执行失败并已回滚: {err}\n语句: {sql}"
                            )));
                        }
                    }
                    reporter.tick(
                        "apply",
                        "事务执行",
                        index as u64 + 1,
                        Some(total),
                        format!("已执行 {}/{} 条", index + 1, total),
                    );
                }
                tx.commit().await?;
                Ok(ScriptRun {
                    outcomes,
                    cancelled: false,
                })
            }};
        }
        match self {
            Pool::MySql(pool) => run_tx!(pool),
            Pool::Postgres(pool) => run_tx!(pool),
        }
    }
}

/// Whether MySQL refused a statement because it cannot be prepared.
///
/// Error 1295 is the server saying exactly that; only then is it safe to retry
/// through the simple protocol, since nothing was executed.
fn not_preparable(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database) => database
            .code()
            .map(|code| code == "1295")
            .unwrap_or(false),
        _ => false,
    }
}

/// What one statement of a user script produced.
#[derive(Debug, Clone)]
pub enum ConsoleOutcome {
    Rows(DescribedResult),
    Affected(u64),
    Failed(String),
}

/// Run a user script.
///
/// Everything runs inside one transaction on one connection. That is what makes
/// `USE` / `search_path` apply to every statement, and an owned `Transaction`
/// also avoids the higher-ranked `Executor` bound that a reborrowed connection
/// cannot satisfy. The transaction is committed at the end: the console is for
/// real work, not for previewing.
pub async fn run_console(
    pool: &Pool,
    namespace: &str,
    statements: &[String],
    row_limit: usize,
    reporter: &ProgressReporter,
) -> AppResult<Vec<ConsoleOutcome>> {
    match pool {
        Pool::MySql(pool) => {
            let mut tx = pool.begin().await?;
            if !namespace.trim().is_empty() {
                // `USE` cannot go through the prepared-statement protocol.
                let sql = format!("USE {}", Dialect::Mysql.quote(namespace));
                tx.execute(sqlx::raw_sql(&sql))
                    .await
                    .map_err(|error| AppError::msg(format!("切换到 {namespace} 失败: {error}")))?;
            }
            let mut outcomes = Vec::with_capacity(statements.len());
            for statement in statements {
                if reporter.is_cancelled() {
                    break;
                }
                outcomes.push(run_one_mysql(&mut tx, statement, row_limit).await);
            }
            tx.commit().await?;
            Ok(outcomes)
        }
        Pool::Postgres(pool) => {
            let mut tx = pool.begin().await?;
            if !namespace.trim().is_empty() {
                let sql = format!("SET LOCAL search_path TO {}", Dialect::Postgres.quote(namespace));
                tx.execute(sqlx::raw_sql(&sql))
                    .await
                    .map_err(|error| AppError::msg(format!("切换到 {namespace} 失败: {error}")))?;
            }
            let mut outcomes = Vec::with_capacity(statements.len());
            for statement in statements {
                if reporter.is_cancelled() {
                    break;
                }
                outcomes.push(run_one_postgres(&mut tx, statement, row_limit).await);
            }
            match tx.commit().await {
                Ok(()) => {}
                // A failed statement leaves the transaction aborted; the results
                // already carry the error, so a rollback here is expected.
                Err(_) => {}
            }
            Ok(outcomes)
        }
    }
}

fn leading_keyword(statement: &str) -> String {
    statement
        .trim_start_matches(|ch: char| ch.is_whitespace() || ch == '(')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// Whether a statement can be used as a subquery.
///
/// Only these may be wrapped to bound the row count; `EXPLAIN` and `SHOW`
/// return rows but are not valid inside `FROM (...)`.
fn is_query(statement: &str) -> bool {
    matches!(
        leading_keyword(statement).as_str(),
        "SELECT" | "WITH" | "VALUES" | "TABLE"
    )
}

/// Whether a statement produces rows at all, wrap-able or not.
///
/// `describe` would answer this authoritatively, but it goes through sqlx's
/// `Executor` trait, whose higher-ranked bounds the compiler cannot satisfy for
/// a connection held across a loop. Classification by leading keyword is what
/// SQL clients have always done and is predictable enough here.
fn returns_rows(statement: &str) -> bool {
    is_query(statement)
        || matches!(
            leading_keyword(statement).as_str(),
            "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN"
        )
}

/// Read a row-returning statement through a subquery, which bounds the row count
/// without needing to know the column list first.
fn wrap_for_reading(sql: &str, limit: usize) -> String {
    let inner = sql.trim().trim_end_matches(';');
    format!("SELECT * FROM ({inner}) AS datasync_query LIMIT {limit}")
}

async fn run_one_mysql(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    statement: &str,
    limit: usize,
) -> ConsoleOutcome {
    if returns_rows(statement) {
        // A plain query is wrapped so its row count is bounded; a statement that
        // merely returns rows is fetched as written.
        let sql = if is_query(statement) {
            wrap_for_reading(statement, limit)
        } else {
            statement.to_string()
        };
        let rows = match tx.fetch_all(sqlx::query(&sql)).await {
            Ok(rows) => rows,
            Err(error) => return ConsoleOutcome::Failed(error.to_string()),
        };
        // The column types come from the result metadata, never from the values.
        let metas = match rows.first() {
            Some(row) => describe_mysql(row.columns()),
            None => Vec::new(),
        };
        let kinds: Vec<ColumnKind> = metas.iter().map(|meta| meta.kind).collect();
        return ConsoleOutcome::Rows(DescribedResult {
            columns: metas,
            rows: rows
                .iter()
                .map(|row| decode_mysql_row_lenient(row, &kinds))
                .collect(),
        });
    }

    match tx.execute(sqlx::query(statement)).await {
        Ok(result) => ConsoleOutcome::Affected(result.rows_affected()),
        // Session statements (`SET`, `USE`) and some DDL cannot be prepared at
        // all; the simple protocol accepts them.
        Err(error) if not_preparable(&error) => {
            match tx.execute(sqlx::raw_sql(statement)).await {
                Ok(result) => ConsoleOutcome::Affected(result.rows_affected()),
                Err(error) => ConsoleOutcome::Failed(error.to_string()),
            }
        }
        Err(error) => ConsoleOutcome::Failed(error.to_string()),
    }
}

async fn run_one_postgres(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    statement: &str,
    limit: usize,
) -> ConsoleOutcome {
    if returns_rows(statement) {
        let sql = if is_query(statement) {
            wrap_for_reading(statement, limit)
        } else {
            statement.to_string()
        };
        let rows = match tx.fetch_all(sqlx::query(&sql)).await {
            Ok(rows) => rows,
            Err(error) => return ConsoleOutcome::Failed(error.to_string()),
        };
        let metas = match rows.first() {
            Some(row) => describe_pg(row.columns()),
            None => Vec::new(),
        };
        let kinds: Vec<ColumnKind> = metas.iter().map(|meta| meta.kind).collect();
        return ConsoleOutcome::Rows(DescribedResult {
            columns: metas,
            rows: rows
                .iter()
                .map(|row| decode_pg_row_lenient(row, &kinds))
                .collect(),
        });
    }

    match tx.execute(sqlx::query(statement)).await {
        Ok(result) => ConsoleOutcome::Affected(result.rows_affected()),
        Err(error) => ConsoleOutcome::Failed(error.to_string()),
    }
}

/// Map a driver-reported MySQL type onto the kind used for decoding.
///
/// The names here are the driver's own ("VARCHAR", "LONGTEXT", "TINYINT"), which
/// differ from the spellings `information_schema` uses, so this cannot reuse the
/// introspected-type mapping.
fn mysql_column_kind(type_name: &str) -> ColumnKind {
    match type_name.to_ascii_uppercase().as_str() {
        // The signedness flag sqlx carries is crate-private, but the integer
        // decoder already falls back from i64 to u64 to f64, so `Int` covers
        // both.
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" => ColumnKind::Int,
        "FLOAT" | "DOUBLE" => ColumnKind::Float,
        "DECIMAL" | "NEWDECIMAL" => ColumnKind::Decimal,
        "DATE" => ColumnKind::Date,
        "TIME" => ColumnKind::Time,
        "DATETIME" | "TIMESTAMP" => ColumnKind::DateTime,
        "JSON" => ColumnKind::Json,
        "BIT" => ColumnKind::Bytes,
        // MySQL reports TEXT and BLOB under the same protocol type. Reading them
        // as text is correct for both: the decoder validates UTF-8 and falls
        // back to bytes when it is not text, so a real BLOB still comes through.
        "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "GEOMETRY" | "BINARY"
        | "VARBINARY" => ColumnKind::Text,
        // VARCHAR, TEXT, ENUM, SET, CHAR and NULL all read as text.
        _ => ColumnKind::Str,
    }
}

fn describe_mysql(columns: &[sqlx::mysql::MySqlColumn]) -> Vec<DescribedColumn> {
    use sqlx::{Column, TypeInfo};
    columns
        .iter()
        .map(|column| {
            let name = column.type_info().name().to_string();
            DescribedColumn {
                name: column.name().to_string(),
                kind: mysql_column_kind(&name),
                type_name: name,
            }
        })
        .collect()
}

/// Map a driver-reported PostgreSQL type onto the kind used for decoding.
///
/// Types the driver cannot decode natively are marked for a text cast rather
/// than being guessed at.
fn pg_column_kind(type_name: &str) -> ColumnKind {
    match type_name.to_ascii_uppercase().as_str() {
        "INT2" | "INT4" | "INT8" => ColumnKind::Int,
        "FLOAT4" | "FLOAT8" => ColumnKind::Float,
        "NUMERIC" => ColumnKind::Decimal,
        "BOOL" => ColumnKind::Bool,
        "DATE" => ColumnKind::Date,
        "TIME" => ColumnKind::Time,
        "TIMESTAMP" | "TIMESTAMPTZ" => ColumnKind::DateTime,
        "JSON" | "JSONB" => ColumnKind::Json,
        "BYTEA" => ColumnKind::Bytes,
        "VARCHAR" | "BPCHAR" | "TEXT" | "NAME" | "CHAR" | "UNKNOWN" | "UUID" => ColumnKind::Str,
        // Arrays, inet/cidr, ranges, enums and domains have no decoder here.
        // They are decoded leniently, so the cell shows a placeholder naming the
        // type instead of failing the whole statement.
        _ => ColumnKind::Text,
    }
}

fn describe_pg(columns: &[sqlx::postgres::PgColumn]) -> Vec<DescribedColumn> {
    use sqlx::{Column, TypeInfo};
    columns
        .iter()
        .map(|column| {
            let name = column.type_info().name().to_string();
            DescribedColumn {
                name: column.name().to_string(),
                kind: pg_column_kind(&name),
                type_name: name,
            }
        })
        .collect()
}

/// One result set from a user-written statement.
#[derive(Debug, Clone, Default)]
pub struct DescribedResult {
    pub columns: Vec<DescribedColumn>,
    pub rows: Vec<Vec<Value>>,
}

/// A column of an ad-hoc result set.
#[derive(Debug, Clone)]
pub struct DescribedColumn {
    pub name: String,
    /// The type as the driver names it, shown to the user.
    pub type_name: String,
    pub kind: ColumnKind,
}

/// Outcome of executing a statement script.
#[derive(Debug, Clone, Default)]
pub struct ScriptRun {
    pub outcomes: Vec<StatementOutcome>,
    pub cancelled: bool,
}

/// Outcome of applying a DDL plan.
#[derive(Debug, Clone, Default)]
pub struct StatementRun {
    pub outcomes: Vec<StatementOutcome>,
    pub cancelled: bool,
    /// A transaction was used and rolled back, so nothing was applied.
    pub rolled_back: bool,
    /// Whether the engine could run the whole plan atomically.
    pub transactional: bool,
    /// Statements never attempted because execution stopped.
    pub skipped: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementOutcome {
    pub sql: String,
    pub ok: bool,
    pub rows_affected: u64,
    pub error: Option<String>,
    pub elapsed_ms: u64,
}

/// Build a pool from a stored configuration. Kept separate from the registry so
/// the "test connection" command can use it without caching anything.
pub async fn open_pool(config: &ConnectionConfig, password: Option<&str>) -> AppResult<Pool> {
    let timeout = Duration::from_secs(20);
    match config.dialect {
        Dialect::Mysql => {
            let opts = mysql_options(config, password);
            let pool = MySqlPoolOptions::new()
                .max_connections(4)
                .acquire_timeout(timeout)
                .idle_timeout(Some(Duration::from_secs(300)))
                .max_lifetime(Some(Duration::from_secs(1800)))
                .connect_with(opts)
                .await
                .map_err(|e| AppError::msg(format!("连接 MySQL 失败: {e}")))?;
            Ok(Pool::MySql(pool))
        }
        Dialect::Postgres => {
            let opts = pg_options(config, password);
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .acquire_timeout(timeout)
                .idle_timeout(Some(Duration::from_secs(300)))
                .max_lifetime(Some(Duration::from_secs(1800)))
                .connect_with(opts)
                .await
                .map_err(|e| AppError::msg(format!("连接 PostgreSQL 失败: {e}")))?;
            Ok(Pool::Postgres(pool))
        }
    }
}

fn mysql_options(config: &ConnectionConfig, password: Option<&str>) -> MySqlConnectOptions {
    let mut opts = MySqlConnectOptions::new()
        .host(config.host.trim())
        .port(config.port)
        .username(config.username.trim());
    if let Some(pw) = password {
        if !pw.is_empty() {
            opts = opts.password(pw);
        }
    }
    if let Some(db) = config.trimmed_database() {
        opts = opts.database(db);
    }
    opts.ssl_mode(match config.ssl_mode.as_str() {
        "disable" => MySqlSslMode::Disabled,
        "require" | "verify" => MySqlSslMode::Required,
        _ => MySqlSslMode::Preferred,
    })
}

fn pg_options(config: &ConnectionConfig, password: Option<&str>) -> PgConnectOptions {
    let mut opts = PgConnectOptions::new()
        .host(config.host.trim())
        .port(config.port)
        .username(config.username.trim());
    if let Some(pw) = password {
        if !pw.is_empty() {
            opts = opts.password(pw);
        }
    }
    if let Some(db) = config.trimmed_database() {
        opts = opts.database(db);
    }
    opts.ssl_mode(match config.ssl_mode.as_str() {
        "disable" => PgSslMode::Disable,
        "require" | "verify" => PgSslMode::Require,
        _ => PgSslMode::Prefer,
    })
}

/// Cache of live pools keyed by connection id.
#[derive(Default)]
pub struct Registry {
    pools: Mutex<HashMap<String, Arc<Pool>>>,
}

impl Registry {
    pub fn get(&self, id: &str) -> Option<Arc<Pool>> {
        self.pools.lock().ok()?.get(id).cloned()
    }

    pub fn put(&self, id: String, pool: Arc<Pool>) {
        if let Ok(mut guard) = self.pools.lock() {
            guard.insert(id, pool);
        }
    }

    pub fn drop_pool(&self, id: &str) {
        if let Ok(mut guard) = self.pools.lock() {
            guard.remove(id);
        }
    }

}
