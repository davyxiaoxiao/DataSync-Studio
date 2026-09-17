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

    /// Execute a list of statements, each in its own round trip, reporting a
    /// per-statement outcome. Statement failures never abort the remaining
    /// work unless `stop_on_error` is set, because DDL scripts are frequently
    /// partially valid (e.g. a column that already matches).
    pub async fn run_script(&self, statements: &[String], stop_on_error: bool) -> Vec<StatementOutcome> {
        let mut out = Vec::with_capacity(statements.len());
        for sql in statements {
            let started = Instant::now();
            match self.execute(sql).await {
                Ok(affected) => out.push(StatementOutcome {
                    sql: sql.clone(),
                    ok: true,
                    rows_affected: affected,
                    error: None,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                }),
                Err(err) => {
                    out.push(StatementOutcome {
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
        }
        out
    }

    /// Execute statements inside a single transaction. Returns an error string
    /// when the transaction is rolled back.
    pub async fn run_in_transaction(&self, statements: &[String]) -> AppResult<Vec<StatementOutcome>> {
        match self {
            Pool::MySql(pool) => {
                let mut tx = pool.begin().await?;
                let mut out = Vec::with_capacity(statements.len());
                for sql in statements {
                    let started = Instant::now();
                    match tx.execute(sql.as_str()).await {
                        Ok(res) => out.push(StatementOutcome {
                            sql: sql.clone(),
                            ok: true,
                            rows_affected: res.rows_affected(),
                            error: None,
                            elapsed_ms: started.elapsed().as_millis() as u64,
                        }),
                        Err(err) => {
                            tx.rollback().await.ok();
                            return Err(AppError::msg(format!("执行失败并已回滚: {err}\n语句: {sql}")));
                        }
                    }
                }
                tx.commit().await?;
                Ok(out)
            }
            Pool::Postgres(pool) => {
                let mut tx = pool.begin().await?;
                let mut out = Vec::with_capacity(statements.len());
                for sql in statements {
                    let started = Instant::now();
                    match tx.execute(sql.as_str()).await {
                        Ok(res) => out.push(StatementOutcome {
                            sql: sql.clone(),
                            ok: true,
                            rows_affected: res.rows_affected(),
                            error: None,
                            elapsed_ms: started.elapsed().as_millis() as u64,
                        }),
                        Err(err) => {
                            tx.rollback().await.ok();
                            return Err(AppError::msg(format!("执行失败并已回滚: {err}\n语句: {sql}")));
                        }
                    }
                }
                tx.commit().await?;
                Ok(out)
            }
        }
    }
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
