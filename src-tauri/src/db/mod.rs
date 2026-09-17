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
use value::{decode_mysql_row, decode_pg_row, ColumnKind, Value};

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
    pub async fn run_ddl(
        &self,
        statements: &[String],
        use_transaction: bool,
        reporter: &ProgressReporter,
    ) -> AppResult<DdlRun> {
        let total = statements.len();
        if statements.is_empty() {
            return Ok(DdlRun {
                transactional: self.supports_transactional_ddl(),
                ..DdlRun::default()
            });
        }

        if !(use_transaction && self.supports_transactional_ddl()) {
            let run = self.run_script_reporting(statements, true, reporter).await;
            let attempted = run.outcomes.len();
            return Ok(DdlRun {
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
                        return Ok(DdlRun {
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
                            return Ok(DdlRun {
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
                Ok(DdlRun {
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
            // Guarded by `supports_transactional_ddl` above.
            Pool::MySql(_) => unreachable!("MySQL does not support transactional DDL"),
        }
    }

    /// Whether DDL can be rolled back on this engine.
    pub fn supports_transactional_ddl(&self) -> bool {
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

/// Outcome of executing a statement script.
#[derive(Debug, Clone, Default)]
pub struct ScriptRun {
    pub outcomes: Vec<StatementOutcome>,
    pub cancelled: bool,
}

/// Outcome of applying a DDL plan.
#[derive(Debug, Clone, Default)]
pub struct DdlRun {
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
            opts = opts.ssl_mode(match config.ssl_mode.as_str() {
                "disable" => MySqlSslMode::Disabled,
                "require" | "verify" => MySqlSslMode::Required,
                _ => MySqlSslMode::Preferred,
            });
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
            opts = opts.ssl_mode(match config.ssl_mode.as_str() {
                "disable" => PgSslMode::Disable,
                "require" | "verify" => PgSslMode::Require,
                _ => PgSslMode::Prefer,
            });
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
