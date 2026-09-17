//! Live database integration tests.
//!
//! These drive the exact same code paths the application uses (introspection →
//! diff → DDL/DML generation → execution) against real servers. They are
//! skipped automatically when the connection environment variables are absent.
//!
//! MySQL:
//!   DATASYNC_MYSQL_HOST, DATASYNC_MYSQL_PORT, DATASYNC_MYSQL_USER,
//!   DATASYNC_MYSQL_PASSWORD
//! PostgreSQL:
//!   DATASYNC_PG_HOST, DATASYNC_PG_PORT, DATASYNC_PG_USER,
//!   DATASYNC_PG_PASSWORD, DATASYNC_PG_DATABASE (default `postgres`)
//!
//! The tests create and drop throw-away namespaces named `datasync_live_*`.

use std::collections::HashMap;

use crate::commands::{compare_data_core, compare_schema_core};
use crate::data::TableEndpoint;
use crate::db::{open_pool, Pool};
use crate::dialect::Dialect;
use crate::jobs::ProgressReporter;
use crate::schema::ddl::generate;
use crate::schema::diff::{ChangeKind, CompareOptions};
use crate::store::ConnectionConfig;

#[derive(Debug, Clone)]
struct LiveTarget {
    host: String,
    port: u16,
    user: String,
    password: String,
    database: Option<String>,
}

fn env_target(prefix: &str, dialect: Dialect) -> Option<LiveTarget> {
    let host = std::env::var(format!("{prefix}_HOST")).ok()?;
    let user = std::env::var(format!("{prefix}_USER")).ok()?;
    let port = std::env::var(format!("{prefix}_PORT"))
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| dialect.default_port());
    let password = std::env::var(format!("{prefix}_PASSWORD")).unwrap_or_default();
    let database = std::env::var(format!("{prefix}_DATABASE"))
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| (dialect == Dialect::Postgres).then(|| "postgres".to_string()));
    Some(LiveTarget {
        host,
        port,
        user,
        password,
        database,
    })
}

fn config_for(target: &LiveTarget, dialect: Dialect) -> ConnectionConfig {
    ConnectionConfig {
        id: "live-test".into(),
        name: "live-test".into(),
        dialect,
        host: target.host.clone(),
        port: target.port,
        username: target.user.clone(),
        database: target.database.clone(),
        ssl_mode: "disable".into(),
        group: None,
        color: None,
        remark: None,
        save_password: false,
        password: Some(target.password.clone()),
        has_password: false,
    }
}

async fn connect(target: &LiveTarget, dialect: Dialect) -> Pool {
    open_pool(&config_for(target, dialect), Some(&target.password))
        .await
        .unwrap_or_else(|err| panic!("连接 {dialect:?} 失败: {err}"))
}

async fn exec(pool: &Pool, sql: &str) {
    pool.execute(sql)
        .await
        .unwrap_or_else(|err| panic!("执行失败: {sql}\n{err}"));
}

async fn exec_all(pool: &Pool, statements: &[String]) {
    for sql in statements {
        exec(pool, sql).await;
    }
}

/// Run a generated plan and fail loudly on the first error, printing the SQL.
async fn run_plan(pool: &Pool, statements: Vec<String>) {
    let run = pool
        .run_script_reporting(&statements, false, &ProgressReporter::inert())
        .await;
    let outcomes = run.outcomes;
    let failures: Vec<String> = outcomes
        .iter()
        .filter(|o| !o.ok)
        .map(|o| format!("{}\n  -> {}", o.sql, o.error.clone().unwrap_or_default()))
        .collect();
    assert!(failures.is_empty(), "有 {} 条语句执行失败:\n{}", failures.len(), failures.join("\n"));
}

async fn create_namespace(pool: &Pool, dialect: Dialect, name: &str) {
    let sql = match dialect {
        Dialect::Mysql => format!("CREATE DATABASE IF NOT EXISTS `{name}` DEFAULT CHARSET utf8mb4"),
        Dialect::Postgres => format!("CREATE SCHEMA IF NOT EXISTS \"{name}\""),
    };
    exec(pool, &sql).await;
}

async fn drop_namespace(pool: &Pool, dialect: Dialect, name: &str) {
    let sql = match dialect {
        Dialect::Mysql => format!("DROP DATABASE IF EXISTS `{name}`"),
        Dialect::Postgres => format!("DROP SCHEMA IF EXISTS \"{name}\" CASCADE"),
    };
    exec(pool, &sql).await;
}

/// Populated schema: a parent table with a wide spread of types plus a child
/// table carrying a foreign key and its own index.
///
/// Table names are schema/database qualified because a pooled connection must
/// never depend on a session default database or `search_path`.
fn source_ddl(dialect: Dialect, ns: &str) -> Vec<String> {
    let users = dialect.qualified(Some(ns), "users");
    let orders = dialect.qualified(Some(ns), "orders");
    match dialect {
        Dialect::Mysql => vec![
            format!(
                "CREATE TABLE {users} (
                    `id` INT NOT NULL AUTO_INCREMENT,
                    `email` VARCHAR(120) NOT NULL,
                    `name` VARCHAR(80) DEFAULT NULL,
                    `age` INT DEFAULT NULL,
                    `salary` DECIMAL(10,2) DEFAULT NULL,
                    `bio` TEXT,
                    `active` TINYINT(1) NOT NULL DEFAULT 1,
                    `created_at` DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    `meta` JSON DEFAULT NULL,
                    `avatar` LONGBLOB,
                    PRIMARY KEY (`id`),
                    UNIQUE KEY `uk_users_email` (`email`),
                    KEY `idx_users_age` (`age`)
                  ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
            ),
            format!(
                "CREATE TABLE {orders} (
                    `id` BIGINT NOT NULL AUTO_INCREMENT,
                    `user_id` INT NOT NULL,
                    `amount` DECIMAL(12,2) NOT NULL,
                    `note` VARCHAR(200) DEFAULT NULL,
                    `total` DECIMAL(12,2) GENERATED ALWAYS AS (`amount` * 2) STORED,
                    PRIMARY KEY (`id`),
                    KEY `idx_orders_user` (`user_id`),
                    CONSTRAINT `fk_orders_user` FOREIGN KEY (`user_id`)
                      REFERENCES {users} (`id`) ON DELETE CASCADE
                  ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
            ),
        ],
        Dialect::Postgres => vec![
            format!(
                "CREATE TABLE {users} (
                    \"id\" integer NOT NULL GENERATED BY DEFAULT AS IDENTITY,
                    \"email\" character varying(120) NOT NULL,
                    \"name\" character varying(80),
                    \"age\" integer,
                    \"salary\" numeric(10,2),
                    \"bio\" text,
                    \"active\" boolean NOT NULL DEFAULT true,
                    \"created_at\" timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    \"meta\" jsonb,
                    \"avatar\" bytea,
                    CONSTRAINT \"users_pkey\" PRIMARY KEY (\"id\"),
                    CONSTRAINT \"uk_users_email\" UNIQUE (\"email\")
                  )"
            ),
            format!("CREATE INDEX \"idx_users_age\" ON {users} (\"age\")"),
            format!(
                "CREATE TABLE {orders} (
                    \"id\" bigint NOT NULL GENERATED BY DEFAULT AS IDENTITY,
                    \"user_id\" integer NOT NULL,
                    \"amount\" numeric(12,2) NOT NULL,
                    \"note\" character varying(200),
                    \"total\" numeric(12,2) GENERATED ALWAYS AS (\"amount\" * 2) STORED,
                    CONSTRAINT \"orders_pkey\" PRIMARY KEY (\"id\"),
                    CONSTRAINT \"fk_orders_user\" FOREIGN KEY (\"user_id\")
                      REFERENCES {users} (\"id\") ON DELETE CASCADE
                  )"
            ),
            format!("CREATE INDEX \"idx_orders_user\" ON {orders} (\"user_id\")"),
        ],
    }
}

/// Structural drift applied to the *source* after the first sync: one added
/// column, one widened column, one dropped column, one retyped column, one new
/// index and one new column default.
fn drift_ddl(dialect: Dialect, ns: &str) -> Vec<String> {
    let users = dialect.qualified(Some(ns), "users");
    match dialect {
        Dialect::Mysql => vec![
            format!("ALTER TABLE {users} ADD COLUMN `nickname` VARCHAR(64) DEFAULT NULL AFTER `name`"),
            format!("ALTER TABLE {users} MODIFY COLUMN `name` VARCHAR(160) DEFAULT NULL"),
            format!("ALTER TABLE {users} DROP COLUMN `bio`"),
            format!("ALTER TABLE {users} MODIFY COLUMN `salary` DECIMAL(12,2) DEFAULT NULL"),
            format!("ALTER TABLE {users} ADD KEY `idx_users_name` (`name`)"),
            format!("ALTER TABLE {users} MODIFY COLUMN `age` INT DEFAULT 18"),
        ],
        Dialect::Postgres => vec![
            format!("ALTER TABLE {users} ADD COLUMN \"nickname\" character varying(64)"),
            format!("ALTER TABLE {users} ALTER COLUMN \"name\" TYPE character varying(160)"),
            format!("ALTER TABLE {users} DROP COLUMN \"bio\""),
            format!("ALTER TABLE {users} ALTER COLUMN \"salary\" TYPE numeric(12,2)"),
            format!("CREATE INDEX \"idx_users_name\" ON {users} (\"name\")"),
            format!("ALTER TABLE {users} ALTER COLUMN \"age\" SET DEFAULT 18"),
        ],
    }
}

fn data_ddl(dialect: Dialect, ns: &str) -> Vec<String> {
    let users = dialect.qualified(Some(ns), "users");
    let orders = dialect.qualified(Some(ns), "orders");
    match dialect {
        Dialect::Mysql => vec![
            format!(
                "INSERT INTO {users} (`id`,`email`,`name`,`nickname`,`age`,`salary`,`active`,`created_at`,`meta`,`avatar`) VALUES
                   (1,'a@example.com','Alice','Ali',30,1234.56,1,'2024-01-01 10:00:00','{{\"tier\": \"gold\", \"n\": 1}}',X'0102'),
                   (2,'b@example.com','Bob',NULL,25,99.90,0,'2024-02-02 11:30:00',NULL,NULL),
                   (3,'c@example.com','Carol',NULL,NULL,NULL,1,'2024-03-03 12:45:00','[1,2,3]',X'DEADBEEF')"
            ),
            format!(
                "INSERT INTO {orders} (`id`,`user_id`,`amount`,`note`) VALUES
                   (10,1,10.00,'first'),(11,1,20.50,NULL),(12,2,5.25,'third')"
            ),
        ],
        Dialect::Postgres => vec![
            format!(
                "INSERT INTO {users} (\"id\",\"email\",\"name\",\"nickname\",\"age\",\"salary\",\"active\",\"created_at\",\"meta\",\"avatar\") VALUES
                   (1,'a@example.com','Alice','Ali',30,1234.56,true,'2024-01-01 10:00:00','{{\"tier\": \"gold\", \"n\": 1}}','\\x0102'::bytea),
                   (2,'b@example.com','Bob',NULL,25,99.90,false,'2024-02-02 11:30:00',NULL,NULL),
                   (3,'c@example.com','Carol',NULL,NULL,NULL,true,'2024-03-03 12:45:00','[1,2,3]','\\xdeadbeef'::bytea)"
            ),
            format!(
                "INSERT INTO {orders} (\"id\",\"user_id\",\"amount\",\"note\") VALUES
                   (10,1,10.00,'first'),(11,1,20.50,NULL),(12,2,5.25,'third')"
            ),
        ],
    }
}

fn drift_data_ddl(dialect: Dialect, ns: &str) -> Vec<String> {
    let users = dialect.qualified(Some(ns), "users");
    let orders = dialect.qualified(Some(ns), "orders");
    match dialect {
        Dialect::Mysql => vec![
            format!("UPDATE {users} SET `name` = 'Alice Cooper', `salary` = 2000.00 WHERE `id` = 1"),
            format!("DELETE FROM {orders} WHERE `id` = 11"),
        ],
        Dialect::Postgres => vec![
            format!("UPDATE {users} SET \"name\" = 'Alice Cooper', \"salary\" = 2000.00 WHERE \"id\" = 1"),
            format!("DELETE FROM {orders} WHERE \"id\" = 11"),
        ],
    }
}

fn assert_structurally_identical(diff: &crate::schema::diff::SchemaDiff, context: &str) {
    let remaining: Vec<String> = diff
        .tables
        .iter()
        .filter(|t| t.kind != ChangeKind::Identical)
        .map(|t| {
            let columns: Vec<String> = t
                .columns
                .iter()
                .filter(|c| c.kind != ChangeKind::Identical)
                .map(|c| {
                    format!(
                        "{} {:?} {:?}",
                        c.name,
                        c.kind,
                        c.differences
                            .iter()
                            .map(|d| format!("{}: {:?} -> {:?}", d.field, d.target, d.source))
                            .collect::<Vec<_>>()
                    )
                })
                .collect();
            let indexes: Vec<String> = t
                .indexes
                .iter()
                .filter(|i| i.kind != ChangeKind::Identical)
                .map(|i| format!("idx {} {:?}", i.name, i.kind))
                .collect();
            let fks: Vec<String> = t
                .foreign_keys
                .iter()
                .filter(|f| f.kind != ChangeKind::Identical)
                .map(|f| format!("fk {} {:?}", f.name, f.kind))
                .collect();
            let pk = (t.primary_key.kind != ChangeKind::Identical)
                .then(|| format!("pk {:?}", t.primary_key.kind));
            format!(
                "{} {:?} columns={columns:?} indexes={indexes:?} fks={fks:?} {pk:?}",
                t.name, t.kind
            )
        })
        .collect();
    assert!(
        remaining.is_empty(),
        "{context}: 同步后仍存在结构差异:\n{}",
        remaining.join("\n")
    );
}

/// The full scenario, parameterised by engine pair.
async fn roundtrip(
    source_pool: &Pool,
    source_ns: &str,
    target_pool: &Pool,
    target_ns: &str,
    source_dialect: Dialect,
    target_dialect: Dialect,
    tag: &str,
) {
    // ---- 1. build the source schema ------------------------------------
    exec_all(source_pool, &source_ddl(source_dialect, source_ns)).await;

    // ---- 2. sync structure into an empty target ------------------------
    let (diff, sources, _warnings) = compare_schema_core(
        source_pool,
        source_ns,
        target_pool,
        target_ns,
        CompareOptions::default(),
        None, &ProgressReporter::inert(),
    )
    .await
    .expect("compare schema");
    assert_eq!(
        diff.summary.new_tables, 2,
        "{tag}: 期望空目标库有 2 张新表, 实际 {:?}",
        diff.summary
    );

    let plan = generate(&diff, &sources, None, Some(target_ns), &mut Vec::new());
    assert!(!plan.is_empty(), "{tag}: 生成的 DDL 不应为空");
    run_plan(target_pool, plan.into_iter().map(|s| s.sql).collect()).await;

    // ---- 3. verify the target now matches ------------------------------
    let (after, _, _) = compare_schema_core(
        source_pool,
        source_ns,
        target_pool,
        target_ns,
        CompareOptions::default(),
        None, &ProgressReporter::inert(),
    )
    .await
    .expect("re-compare schema");
    assert_structurally_identical(&after, &format!("{tag} 首次同步"));

    // ---- 4. introduce structural drift on the source -------------------
    exec_all(source_pool, &drift_ddl(source_dialect, source_ns)).await;

    let (drift, drift_sources, _) = compare_schema_core(
        source_pool,
        source_ns,
        target_pool,
        target_ns,
        CompareOptions::default(),
        None, &ProgressReporter::inert(),
    )
    .await
    .expect("compare drift");
    assert_eq!(
        drift.summary.changed_tables, 1,
        "{tag}: 期望仅 users 表发生变化, 实际 {:?}",
        drift.summary
    );
    assert!(
        drift.summary.new_columns >= 1
            && drift.summary.changed_columns >= 2
            && drift.summary.missing_columns >= 1
            && drift.summary.index_changes >= 1,
        "{tag}: 漂移未被完整识别: {:?}",
        drift.summary
    );

    let drift_plan = generate(&drift, &drift_sources, None, Some(target_ns), &mut Vec::new());
    run_plan(target_pool, drift_plan.into_iter().map(|s| s.sql).collect()).await;

    let (after_drift, _, _) = compare_schema_core(
        source_pool,
        source_ns,
        target_pool,
        target_ns,
        CompareOptions::default(),
        None, &ProgressReporter::inert(),
    )
    .await
    .expect("re-compare after drift");
    assert_structurally_identical(&after_drift, &format!("{tag} 结构漂移同步"));

    // ---- 5. seed data on the source only -------------------------------
    exec_all(source_pool, &data_ddl(source_dialect, source_ns)).await;
    let _ = target_dialect;

    let source_ep = TableEndpoint {
        connection_id: "src".into(),
        namespace: source_ns.into(),
        table: "users".into(),
    };
    let target_ep = TableEndpoint {
        connection_id: "dst".into(),
        namespace: target_ns.into(),
        table: "users".into(),
    };

    let (result, plan, _) = compare_data_core(source_pool, &source_ep, target_pool, &target_ep, &[], &[], 100, &ProgressReporter::inert())
    .await
    .expect("compare users data");
    assert_eq!(result.summary.inserts, 3, "{tag}: users 期望 3 行待插入");
    assert_eq!(result.summary.updates, 0);
    assert_eq!(result.summary.deletes, 0);

    run_plan(target_pool, plan.select(true, true, true)).await;

    let (settled, _, _) = compare_data_core(source_pool, &source_ep, target_pool, &target_ep, &[], &[], 100, &ProgressReporter::inert())
    .await
    .expect("re-compare users data");
    assert_eq!(
        (
            settled.summary.inserts,
            settled.summary.updates,
            settled.summary.deletes
        ),
        (0, 0, 0),
        "{tag}: users 数据同步后应完全一致, 实际 {:?}",
        settled.summary
    );

    // ---- 6. update + delete on the source, then re-sync -----------------
    exec_all(source_pool, &drift_data_ddl(source_dialect, source_ns)).await;

    let (changed, plan, _) = compare_data_core(source_pool, &source_ep, target_pool, &target_ep, &[], &[], 100, &ProgressReporter::inert())
    .await
    .expect("compare changed users data");
    assert_eq!(changed.summary.updates, 1, "{tag}: 期望 1 行待更新");
    assert_eq!(changed.summary.inserts, 0, "{tag}: 不应有新增");

    run_plan(target_pool, plan.select(true, true, true)).await;

    let (settled, _, _) = compare_data_core(source_pool, &source_ep, target_pool, &target_ep, &[], &[], 100, &ProgressReporter::inert())
    .await
    .expect("re-compare changed users data");
    assert_eq!(
        (settled.summary.inserts, settled.summary.updates, settled.summary.deletes),
        (0, 0, 0),
        "{tag}: 更新后 users 应完全一致, 实际 {:?}",
        settled.summary
    );

    // ---- 7. child table: delete propagation + typed comparison ----------
    let source_orders = TableEndpoint {
        connection_id: "src".into(),
        namespace: source_ns.into(),
        table: "orders".into(),
    };
    let target_orders = TableEndpoint {
        connection_id: "dst".into(),
        namespace: target_ns.into(),
        table: "orders".into(),
    };
    let (orders, plan, _) = compare_data_core(source_pool, &source_orders, target_pool, &target_orders, &[], &[], 100, &ProgressReporter::inert())
    .await
    .expect("compare orders data");
    assert_eq!(orders.summary.inserts, 2, "{tag}: orders 期望 2 行待插入 (源已删除 1 行)");
    assert_eq!(orders.summary.deletes, 0, "{tag}: 目标为空, 不应有删除");
    run_plan(target_pool, plan.select(true, true, true)).await;

    let (orders_settled, _, _) = compare_data_core(source_pool, &source_orders, target_pool, &target_orders, &[], &[], 100, &ProgressReporter::inert())
    .await
    .expect("re-compare orders data");
    assert_eq!(
        (
            orders_settled.summary.inserts,
            orders_settled.summary.updates,
            orders_settled.summary.deletes
        ),
        (0, 0, 0),
        "{tag}: orders 同步后应完全一致, 实际 {:?}",
        orders_settled.summary
    );

    // ---- 8. the generated column survived as a *generated* column ----------
    // It must be recomputed by the target engine, not copied or left empty.
    let target_dialect = target_pool.dialect();
    let total_col = target_dialect.quote("total");
    let orders_ref = target_dialect.qualified(Some(target_ns), "orders");
    let id_col = target_dialect.quote("id");
    let sql = format!("SELECT {total_col} FROM {orders_ref} WHERE {id_col} = 10");
    let rows = target_pool
        .fetch(&sql, &[], &[crate::db::value::ColumnKind::Decimal])
        .await
        .unwrap_or_else(|e| panic!("{tag}: 读取生成列失败: {e}"));
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|value| value.display().unwrap_or_default())
                .collect()
        })
        .collect();
    let first_row: &Vec<String> = rows
        .first()
        .unwrap_or_else(|| panic!("{tag}: 目标表没有 id=10 的行"));
    let raw: &String = first_row
        .first()
        .unwrap_or_else(|| panic!("{tag}: 生成列没有值"));
    let value: f64 = raw
        .parse()
        .unwrap_or_else(|_| panic!("{tag}: 生成列值无法解析为数值: {raw:?}"));
    assert!(
        (value - 20.0).abs() < 1e-6,
        "{tag}: 生成列应由目标端重算为 amount*2 = 20.00, 实际 {value}"
    );
}

#[tokio::test]
async fn live_mysql_to_mysql_roundtrip() {
    let Some(target) = env_target("DATASYNC_MYSQL", Dialect::Mysql) else {
        eprintln!("跳过：未设置 DATASYNC_MYSQL_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Mysql).await;
    let (src, dst) = ("datasync_live_mysql_src", "datasync_live_mysql_dst");
    drop_namespace(&pool, Dialect::Mysql, src).await;
    drop_namespace(&pool, Dialect::Mysql, dst).await;
    create_namespace(&pool, Dialect::Mysql, src).await;
    create_namespace(&pool, Dialect::Mysql, dst).await;

    roundtrip(&pool, src, &pool, dst, Dialect::Mysql, Dialect::Mysql, "mysql→mysql").await;

    drop_namespace(&pool, Dialect::Mysql, src).await;
    drop_namespace(&pool, Dialect::Mysql, dst).await;
}

#[tokio::test]
async fn live_postgres_to_postgres_roundtrip() {
    let Some(target) = env_target("DATASYNC_PG", Dialect::Postgres) else {
        eprintln!("跳过：未设置 DATASYNC_PG_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Postgres).await;
    let (src, dst) = ("datasync_live_pg_src", "datasync_live_pg_dst");
    drop_namespace(&pool, Dialect::Postgres, src).await;
    drop_namespace(&pool, Dialect::Postgres, dst).await;
    create_namespace(&pool, Dialect::Postgres, src).await;
    create_namespace(&pool, Dialect::Postgres, dst).await;

    roundtrip(&pool, src, &pool, dst, Dialect::Postgres, Dialect::Postgres, "pg→pg").await;

    drop_namespace(&pool, Dialect::Postgres, src).await;
    drop_namespace(&pool, Dialect::Postgres, dst).await;
}

/// Streaming comparison: many keyset pages, a textual primary key whose
/// ordering is collation dependent, and (on MySQL) two sides with *different*
/// collations. Without an explicit byte-order collation the two orderings would
/// disagree and the merge would produce phantom inserts and deletes.
async fn pagination_scenario(pool: &Pool, dialect: Dialect, src: &str, dst: &str, tag: &str) {
    drop_namespace(pool, dialect, src).await;
    drop_namespace(pool, dialect, dst).await;
    create_namespace(pool, dialect, src).await;
    create_namespace(pool, dialect, dst).await;

    let source_table = dialect.qualified(Some(src), "page_test");
    let target_table = dialect.qualified(Some(dst), "page_test");
    let k = dialect.quote("k");
    let n = dialect.quote("n");
    let payload = dialect.quote("payload");

    let create = |table: &str, collation: &str| match dialect {
        Dialect::Mysql => format!(
            "CREATE TABLE {table} (
                {k} VARCHAR(32) NOT NULL,
                {n} INT NOT NULL,
                {payload} VARCHAR(64) DEFAULT NULL,
                PRIMARY KEY ({k})
             ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE={collation}"
        ),
        Dialect::Postgres => format!(
            "CREATE TABLE {table} (
                {k} character varying(32) NOT NULL,
                {n} integer NOT NULL,
                {payload} character varying(64),
                CONSTRAINT \"page_test_pkey\" PRIMARY KEY ({k})
             )"
        ),
    };
    // The two sides deliberately use collations that order ASCII letters
    // differently: case-insensitive versus byte order.
    let source_collation = "utf8mb4_general_ci";
    let target_collation = "utf8mb4_bin";
    exec(pool, &create(&source_table, source_collation)).await;
    exec(pool, &create(&target_table, target_collation)).await;

    // Keys whose byte order and case-insensitive order genuinely differ.
    let letters = ["A", "a", "B", "b", "Z", "z"];
    let row_count = 1_500_usize;
    let keys: Vec<String> = (0..row_count)
        .map(|i| format!("{}{:05}", letters[i % letters.len()], i))
        .collect();

    for (chunk_index, chunk) in keys.chunks(500).enumerate() {
        let values: Vec<String> = chunk
            .iter()
            .enumerate()
            .map(|(offset, key)| {
                let value = chunk_index * 500 + offset;
                format!("('{key}', {value}, 'seed-{value}')")
            })
            .collect();
        exec(
            pool,
            &format!(
                "INSERT INTO {source_table} ({k}, {n}, {payload}) VALUES {}",
                values.join(", ")
            ),
        )
        .await;
    }

    let source_ep = TableEndpoint {
        connection_id: "src".into(),
        namespace: src.into(),
        table: "page_test".into(),
    };
    let target_ep = TableEndpoint {
        connection_id: "dst".into(),
        namespace: dst.into(),
        table: "page_test".into(),
    };
    // A deliberately small page size: 1500 rows read in pages of 150, so the
    // keyset pagination and the scattered updates both cross page boundaries.
    let batch = 150;

    let (result, plan, _) =
        compare_data_core(
            pool,
            &source_ep,
            pool,
            &target_ep,
            &[],
            &[],
            batch,
            &ProgressReporter::inert(),
        )
            .await
            .expect("compare paginated data");
    assert_eq!(
        result.summary.inserts, row_count,
        "{tag}: 期望 {row_count} 行待插入, 实际 {:?}",
        result.summary
    );
    assert_eq!(result.summary.deletes, 0, "{tag}: 空目标不应有删除");
    assert_eq!(result.summary.updates, 0, "{tag}: 空目标不应有更新");

    run_plan(pool, plan.select(true, true, true)).await;

    let (settled, _, _) = compare_data_core(
        pool,
        &source_ep,
        pool,
        &target_ep,
        &[],
        &[],
        batch,
        &ProgressReporter::inert(),
    )
        .await
        .expect("re-compare paginated data");
    assert_eq!(
        (
            settled.summary.inserts,
            settled.summary.updates,
            settled.summary.deletes
        ),
        (0, 0, 0),
        "{tag}: 分页同步后应完全一致, 实际 {:?}",
        settled.summary
    );

    // Scattered updates and deletes, again crossing page boundaries. A row that
    // is deleted on the source can no longer be updated, so the two sets are
    // kept disjoint.
    let remove: Vec<String> = keys
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 11 == 3)
        .map(|(_, key)| key.clone())
        .collect();
    let removed: std::collections::HashSet<&str> =
        remove.iter().map(|key| key.as_str()).collect();
    let update_keys: Vec<String> = keys
        .iter()
        .enumerate()
        .filter(|(index, key)| index % 7 == 0 && !removed.contains(key.as_str()))
        .map(|(_, key)| key.clone())
        .collect();
    let touched = update_keys.len();

    let quoted: Vec<String> = update_keys.iter().map(|key| format!("'{key}'")).collect();
    for chunk in quoted.chunks(200) {
        exec(
            pool,
            &format!(
                "UPDATE {source_table} SET {n} = {n} + 1000 WHERE {k} IN ({})",
                chunk.join(", ")
            ),
        )
        .await;
    }
    let quoted: Vec<String> = remove.iter().map(|key| format!("'{key}'")).collect();
    for chunk in quoted.chunks(200) {
        exec(
            pool,
            &format!("DELETE FROM {source_table} WHERE {k} IN ({})", chunk.join(", ")),
        )
        .await;
    }

    let (changed, plan, _) = compare_data_core(
        pool,
        &source_ep,
        pool,
        &target_ep,
        &[],
        &[],
        batch,
        &ProgressReporter::inert(),
    )
        .await
        .expect("compare changed paginated data");
    assert_eq!(changed.summary.updates, touched, "{tag}: 更新数不符");
    assert_eq!(changed.summary.deletes, remove.len(), "{tag}: 删除数不符");

    run_plan(pool, plan.select(true, true, true)).await;

    let (final_pass, _, _) = compare_data_core(
        pool,
        &source_ep,
        pool,
        &target_ep,
        &[],
        &[],
        batch,
        &ProgressReporter::inert(),
    )
        .await
        .expect("final re-compare");
    assert_eq!(
        (
            final_pass.summary.inserts,
            final_pass.summary.updates,
            final_pass.summary.deletes
        ),
        (0, 0, 0),
        "{tag}: 增量同步后应完全一致, 实际 {:?}",
        final_pass.summary
    );

    drop_namespace(pool, dialect, src).await;
    drop_namespace(pool, dialect, dst).await;
}

#[tokio::test]
async fn live_mysql_streaming_pagination() {
    let Some(target) = env_target("DATASYNC_MYSQL", Dialect::Mysql) else {
        eprintln!("跳过：未设置 DATASYNC_MYSQL_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Mysql).await;
    pagination_scenario(
        &pool,
        Dialect::Mysql,
        "datasync_live_page_src",
        "datasync_live_page_dst",
        "mysql 分页",
    )
    .await;
}

#[tokio::test]
async fn live_postgres_streaming_pagination() {
    let Some(target) = env_target("DATASYNC_PG", Dialect::Postgres) else {
        eprintln!("跳过：未设置 DATASYNC_PG_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Postgres).await;
    pagination_scenario(
        &pool,
        Dialect::Postgres,
        "datasync_live_page_src",
        "datasync_live_page_dst",
        "pg 分页",
    )
    .await;
}

/// Cross-engine synchronisation is deliberately refused, and the refusal must
/// happen before any database work.
#[tokio::test]
async fn live_cross_engine_sync_is_rejected() {
    let (Some(mysql_target), Some(pg_target)) = (
        env_target("DATASYNC_MYSQL", Dialect::Mysql),
        env_target("DATASYNC_PG", Dialect::Postgres),
    ) else {
        eprintln!("跳过：需要同时设置 DATASYNC_MYSQL_* 与 DATASYNC_PG_* 环境变量");
        return;
    };
    let mysql = connect(&mysql_target, Dialect::Mysql).await;
    let pg = connect(&pg_target, Dialect::Postgres).await;

    let schema_error = compare_schema_core(
        &mysql,
        "datasync_probe_src",
        &pg,
        "datasync_probe_dst",
        CompareOptions::default(),
        None,
        &ProgressReporter::inert(),
    )
    .await
    .expect_err("MySQL → PostgreSQL 的结构对比必须被拒绝");
    let message = schema_error.to_string();
    assert!(message.contains("不支持跨数据库类型同步"), "{message}");

    let source = TableEndpoint {
        connection_id: "src".into(),
        namespace: "datasync_probe_src".into(),
        table: "t".into(),
    };
    let target = TableEndpoint {
        connection_id: "dst".into(),
        namespace: "datasync_probe_dst".into(),
        table: "t".into(),
    };
    let data_error = compare_data_core(&mysql, &source, &pg, &target, &[], &[], 100, &ProgressReporter::inert())
        .await
        .err()
        .expect("MySQL → PostgreSQL 的数据对比必须被拒绝");
    assert!(data_error.to_string().contains("不支持跨数据库类型同步"));

    // The same server compared with itself is accepted.
    let (ok, _, _) = compare_schema_core(
        &mysql,
        "datasync_live_mysql_src",
        &mysql,
        "datasync_live_mysql_src",
        CompareOptions::default(),
        None,
        &ProgressReporter::inert(),
    )
    .await
    .expect("同类型对比应当被接受");
    assert!(ok.tables.is_empty());
}

/// Guard against silently doing nothing when the environment is not configured.
#[test]
fn live_env_configuration_is_reported() {
    let has_mysql = env_target("DATASYNC_MYSQL", Dialect::Mysql).is_some();
    let has_pg = env_target("DATASYNC_PG", Dialect::Postgres).is_some();
    eprintln!("live 测试环境: mysql={has_mysql} postgres={has_pg}");
    let _ = HashMap::<String, String>::new();
}
