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

use crate::commands::{
    apply_data_plan, compare_data_core, compare_schema_core, plan_alter_table, plan_drop_scope,
    preview_table_data_core, ColumnRename,
};
use crate::data::import::{run_import, suggest_mapping, ImportOptions, ImportStrategy};
use crate::data::view::{build_cell_update, CellEdit, FilterCondition, FilterOp, RowKeyValue, SortSpec};
use crate::excel;
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

/// Apply a data plan through the same core the command layer uses, so the
/// materialised-view refresh is covered too.
async fn apply_data(target_pool: &Pool, target_ns: &str, statements: Vec<String>) -> Vec<String> {
    let (outcomes, warnings, cancelled) = apply_data_plan(
        target_pool,
        &statements,
        true,
        target_ns,
        true,
        &ProgressReporter::inert(),
    )
    .await
    .unwrap_or_else(|e| panic!("数据同步失败: {e}"));
    assert!(!cancelled, "数据同步不应被取消");
    let failures: Vec<String> = outcomes
        .iter()
        .filter(|o| !o.ok)
        .map(|o| format!("{}\n  -> {}", o.sql, o.error.clone().unwrap_or_default()))
        .collect();
    assert!(failures.is_empty(), "数据同步有语句失败:\n{}", failures.join("\n"));
    assert!(warnings.is_empty(), "数据同步产生告警: {warnings:?}");
    warnings
}

/// Run a generated schema plan and fail loudly on the first error, printing the SQL.
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

/// Prefix every namespace created or destroyed by these tests must carry.
///
/// These tests are pointed at whatever server the environment names, and that
/// may well be a production box. The guard makes it structurally impossible for
/// a test to drop or overwrite a real database or schema: a typo in a test can
/// only ever produce a refusal, never a `DROP DATABASE datasync1`.
const TEST_NAMESPACE_PREFIX: &str = "datasync_live_";

fn assert_test_namespace(name: &str) {
    assert!(
        name.starts_with(TEST_NAMESPACE_PREFIX),
        "拒绝操作 {name:?}：测试只允许处理以 {TEST_NAMESPACE_PREFIX:?} 开头的命名空间。\
         这条断言的存在是为了让误操作线上库变成不可能，请勿放宽。"
    );
}

async fn create_namespace(pool: &Pool, dialect: Dialect, name: &str) {
    assert_test_namespace(name);
    let sql = match dialect {
        Dialect::Mysql => format!("CREATE DATABASE IF NOT EXISTS `{name}` DEFAULT CHARSET utf8mb4"),
        Dialect::Postgres => format!("CREATE SCHEMA IF NOT EXISTS \"{name}\""),
    };
    exec(pool, &sql).await;
}

async fn drop_namespace(pool: &Pool, dialect: Dialect, name: &str) {
    assert_test_namespace(name);
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
    let mood = dialect.qualified(Some(ns), "order_state");
    let amount = dialect.qualified(Some(ns), "positive_amount");
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
            // Types must exist before the tables that use them.
            format!("CREATE TYPE {mood} AS ENUM ('new', 'paid', 'shipped')"),
            format!(
                "CREATE DOMAIN {amount} AS numeric(12,2) CHECK (VALUE > (0)::numeric)"
            ),
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
                    \"state\" {mood} NOT NULL DEFAULT 'new',
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

/// Views on top of the source tables: one direct, one layered on the first, and
/// on PostgreSQL additionally a materialised view.
fn source_view_ddl(dialect: Dialect, ns: &str) -> Vec<String> {
    let users = dialect.qualified(Some(ns), "users");
    let v_users = dialect.qualified(Some(ns), "v_users");
    let v_emails = dialect.qualified(Some(ns), "v_user_emails");
    let mv_users = dialect.qualified(Some(ns), "mv_users");
    match dialect {
        Dialect::Mysql => vec![
            format!("CREATE VIEW {v_users} AS SELECT `id`, `email`, `name` FROM {users}"),
            format!("CREATE VIEW {v_emails} AS SELECT `email` FROM {v_users}"),
        ],
        Dialect::Postgres => vec![
            format!("CREATE VIEW {v_users} AS SELECT \"id\", \"email\", \"name\" FROM {users}"),
            format!("CREATE VIEW {v_emails} AS SELECT \"email\" FROM {v_users}"),
            format!(
                "CREATE MATERIALIZED VIEW {mv_users} AS SELECT \"id\", \"email\" FROM {users} WITH DATA"
            ),
        ],
    }
}

/// A **nullable** column plus real values.
///
/// This is the shape the tool can handle while keeping the two syncs separate:
/// the structure sync adds the column without touching a single row, the data
/// sync writes the values, and a second structure sync tightens the constraint.
fn late_column_ddl(dialect: Dialect, ns: &str) -> Vec<String> {
    let users = dialect.qualified(Some(ns), "users");
    let column = dialect.quote("channel");
    let type_name = match dialect {
        Dialect::Mysql => "VARCHAR(32)",
        Dialect::Postgres => "character varying(32)",
    };
    let fill = match dialect {
        Dialect::Mysql => format!("UPDATE {users} SET {column} = CONCAT('web-', `id`)"),
        Dialect::Postgres => format!("UPDATE {users} SET {column} = 'web-' || \"id\""),
    };
    vec![
        format!("ALTER TABLE {users} ADD COLUMN {column} {type_name} NULL"),
        fill,
    ]
}

/// Tighten the column to NOT NULL once the data has been synchronised.
///
/// The syntax differs per engine, which is exactly why the tool generates the
/// statement rather than the test.
fn tighten_column_ddl(dialect: Dialect, ns: &str) -> Vec<String> {
    let users = dialect.qualified(Some(ns), "users");
    let column = dialect.quote("channel");
    match dialect {
        Dialect::Mysql => vec![format!(
            "ALTER TABLE {users} MODIFY COLUMN {column} VARCHAR(32) NOT NULL"
        )],
        Dialect::Postgres => vec![format!(
            "ALTER TABLE {users} ALTER COLUMN {column} SET NOT NULL"
        )],
    }
}

/// Drop the source-side views in reverse dependency order.
fn drop_view_ddl(dialect: Dialect, ns: &str) -> Vec<String> {
    let mut names: Vec<&str> = view_names(dialect);
    names.reverse();
    names
        .into_iter()
        .map(|name| {
            let keyword = if name.starts_with("mv_") && dialect == Dialect::Postgres {
                "MATERIALIZED VIEW"
            } else {
                "VIEW"
            };
            format!(
                "DROP {keyword} IF EXISTS {}",
                dialect.qualified(Some(ns), name)
            )
        })
        .collect()
}

/// Structural drift applied to the *source* after the first sync: one added
/// column, one widened column, one dropped column, one retyped column, one new
/// index and one new column default.
fn drift_ddl(dialect: Dialect, ns: &str) -> Vec<String> {
    let users = dialect.qualified(Some(ns), "users");
    let mood = dialect.qualified(Some(ns), "order_state");
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
            // A new label must be propagated as ALTER TYPE ... ADD VALUE.
            format!("ALTER TYPE {mood} ADD VALUE 'delivered' AFTER 'shipped'"),
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
                "INSERT INTO {orders} (\"id\",\"user_id\",\"amount\",\"note\",\"state\") VALUES
                   (10,1,10.00,'first','paid'),(11,1,20.50,NULL,'shipped'),(12,2,5.25,'third','new')"
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

fn view_names(dialect: Dialect) -> Vec<&'static str> {
    match dialect {
        Dialect::Mysql => vec!["v_users", "v_user_emails"],
        Dialect::Postgres => vec!["v_users", "v_user_emails", "mv_users"],
    }
}

/// Refresh the *source* materialised views after seeding.
///
/// The test creates them before the data exists, and nothing syncs into the
/// source, so a source-side DBA would refresh them by hand. The target side is
/// left alone: refreshing it is what the tool must do.
async fn refresh_source_matviews(pool: &Pool, dialect: Dialect, ns: &str) {
    if dialect != Dialect::Postgres {
        return;
    }
    let views = crate::schema::introspect::list_views(pool, ns)
        .await
        .expect("list source views");
    for view in views.iter().filter(|v| v.is_materialized()) {
        exec(
            pool,
            &format!(
                "REFRESH MATERIALIZED VIEW {}",
                dialect.qualified(Some(ns), &view.name)
            ),
        )
        .await;
    }
}

/// Read one column from the first row, distinguishing NULL from a value.
async fn scalar_opt(
    pool: &Pool,
    namespace: &str,
    table: &str,
    column: &str,
    dialect: Dialect,
) -> Option<String> {
    let sql = format!(
        "SELECT {} FROM {}",
        dialect.quote(column),
        dialect.qualified(Some(namespace), table)
    );
    let rows = pool
        .fetch(&sql, &[], &[crate::db::value::ColumnKind::Str])
        .await
        .unwrap_or_else(|e| panic!("读取 {table}.{column} 失败: {e}"));
    rows.first()
        .and_then(|row| row.first())
        .and_then(|value| value.display())
}

/// Read one text column from the first row of a table.
async fn scalar_text(
    pool: &Pool,
    namespace: &str,
    table: &str,
    column: &str,
    dialect: Dialect,
) -> String {
    let sql = format!(
        "SELECT {} FROM {}",
        dialect.quote(column),
        dialect.qualified(Some(namespace), table)
    );
    let rows = pool
        .fetch(&sql, &[], &[crate::db::value::ColumnKind::Str])
        .await
        .unwrap_or_else(|e| panic!("读取 {table}.{column} 失败: {e}"));
    rows.first()
        .and_then(|row| row.first())
        .and_then(|value| value.display())
        .unwrap_or_else(|| panic!("{table}.{column} 没有值"))
}

/// `SELECT COUNT(*)` through a view on the target side.
async fn scalar_count(pool: &Pool, namespace: &str, name: &str, dialect: Dialect) -> i64 {
    let sql = format!(
        "SELECT COUNT(*) FROM {}",
        dialect.qualified(Some(namespace), name)
    );
    let rows = pool
        .fetch(&sql, &[], &[crate::db::value::ColumnKind::Int])
        .await
        .unwrap_or_else(|e| panic!("查询视图 {name} 失败: {e}"));
    rows.first()
        .and_then(|row| row.first())
        .and_then(|value| value.display())
        .and_then(|text| text.parse().ok())
        .unwrap_or_else(|| panic!("视图 {name} 未返回计数: {rows:?}"))
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
    exec_all(source_pool, &source_view_ddl(source_dialect, source_ns)).await;

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
    let expected_types = if source_dialect == Dialect::Postgres { 2 } else { 0 };
    assert_eq!(
        diff.summary.new_types, expected_types,
        "{tag}: 期望空目标库有 {expected_types} 个自定义类型, 实际 {:?}",
        diff.summary
    );
    let expected_views = if source_dialect == Dialect::Postgres { 3 } else { 2 };
    assert_eq!(
        diff.summary.new_views, expected_views,
        "{tag}: 期望空目标库有 {expected_views} 个视图, 实际 {:?}",
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
    for name in view_names(source_dialect) {
        let count = scalar_count(target_pool, target_ns, name, target_dialect).await;
        assert_eq!(count, 0, "{tag}: 目标端视图 {name} 应可查询且为空");
    }
    // The enum's labels and the domain must have travelled with the schema.
    if source_dialect == Dialect::Postgres {
        let target_types = crate::schema::introspect::list_custom_types(target_pool, target_ns)
            .await
            .expect("list target types");
        let mood = target_types
            .iter()
            .find(|t| t.name == "order_state")
            .expect("目标库应有 order_state 枚举");
        assert_eq!(
            mood.labels,
            vec!["new", "paid", "shipped"],
            "{tag}: 枚举值不一致"
        );
        let domain = target_types
            .iter()
            .find(|t| t.name == "positive_amount")
            .expect("目标库应有 positive_amount 域");
        assert_eq!(domain.base_type.as_deref(), Some("numeric(12,2)"));
        assert_eq!(domain.constraints.len(), 1, "{domain:?}");
    }

    // Regression guard: the copied view body must read from the *target*
    // namespace. Copying it verbatim would leave the target's views reading the
    // source database, which stays invisible while the two hold the same rows.
    for view in crate::schema::introspect::list_views(target_pool, target_ns)
        .await
        .expect("list target views")
    {
        assert!(
            !view.definition.contains(source_ns),
            "{tag}: 目标端视图 {} 仍然引用源库 {source_ns}：{}",
            view.name,
            view.definition
        );
    }

    // ---- 4. introduce structural drift on the source -------------------
    // PostgreSQL refuses to alter a column that a view depends on, so the
    // test cycles its own source views around the drift. The *target* side is
    // left untouched: rebuilding its views is exactly what is under test.
    exec_all(source_pool, &drop_view_ddl(source_dialect, source_ns)).await;
    exec_all(source_pool, &drift_ddl(source_dialect, source_ns)).await;
    exec_all(source_pool, &source_view_ddl(source_dialect, source_ns)).await;

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
    if source_dialect == Dialect::Postgres {
        // The new enum label must have been added, not dropped or ignored.
        let labels = crate::schema::introspect::list_custom_types(target_pool, target_ns)
            .await
            .expect("list target types")
            .into_iter()
            .find(|t| t.name == "order_state")
            .map(|t| t.labels)
            .unwrap_or_default();
        assert_eq!(
            labels,
            vec!["new", "paid", "shipped", "delivered"],
            "{tag}: 新增枚举值未同步"
        );
    }
    // `users.name` was widened and `bio` dropped while views depended on the
    // table: PostgreSQL only allows that because the views were rebuilt.
    for name in view_names(source_dialect) {
        let count = scalar_count(target_pool, target_ns, name, target_dialect).await;
        assert_eq!(count, 0, "{tag}: 漂移同步后视图 {name} 仍应可查询");
    }

    // ---- 5. seed data on the source only -------------------------------
    exec_all(source_pool, &data_ddl(source_dialect, source_ns)).await;
    refresh_source_matviews(source_pool, source_dialect, source_ns).await;
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

    apply_data(target_pool, target_ns, plan.select(true, true, true)).await;

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

    apply_data(target_pool, target_ns, plan.select(true, true, true)).await;

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
    apply_data(target_pool, target_ns, plan.select(true, true, true)).await;

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

    // ---- 7b. the target's views now see the synchronised rows -------------
    for name in view_names(source_dialect) {
        let source_count = scalar_count(source_pool, source_ns, name, source_dialect).await;
        let target_count = scalar_count(target_pool, target_ns, name, target_dialect).await;
        assert_eq!(
            source_count, target_count,
            "{tag}: 视图 {name} 的行数应与源端一致"
        );
        assert!(source_count > 0, "{tag}: 视图 {name} 应有数据");
    }

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

    // ---- 9. a column added while the target already holds rows -------------
    // Structure sync only adds the column; it must not write a single row.
    exec_all(source_pool, &late_column_ddl(source_dialect, source_ns)).await;

    let (late, late_sources, _) = compare_schema_core(
        source_pool,
        source_ns,
        target_pool,
        target_ns,
        CompareOptions::default(),
        None,
        &ProgressReporter::inert(),
    )
    .await
    .expect("compare late column");
    assert_eq!(
        late.summary.new_columns, 1,
        "{tag}: 期望识别出 1 个新增列, 实际 {:?}",
        late.summary
    );

    let mut late_warnings = Vec::new();
    let late_plan = generate(
        &late,
        &late_sources,
        None,
        Some(target_ns),
        &mut late_warnings,
    );
    assert_eq!(
        late_plan.iter().filter(|s| s.category == "add_column").count(),
        1,
        "{tag}: 只应产生一条 ADD COLUMN"
    );
    run_plan(target_pool, late_plan.into_iter().map(|s| s.sql).collect()).await;

    let target_structure = crate::schema::introspect::load_table(target_pool, target_ns, "users")
        .await
        .expect("load target users");
    let channel = target_structure
        .column("channel")
        .expect("目标表应有 channel 列");
    assert!(
        channel.nullable,
        "{tag}: 结构同步只应加可空列，NOT NULL 由后续流程加上"
    );
    // The decisive assertion: structure sync wrote no row data.
    let untouched = scalar_opt(target_pool, target_ns, "users", "channel", target_dialect).await;
    assert_eq!(
        untouched, None,
        "{tag}: 结构同步不得向现有行写入任何值，实际 {untouched:?}"
    );

    // ---- 10. the data sync fills the column with the real values -----------
    let (pending, fill_plan, _) = compare_data_core(
        source_pool,
        &source_ep,
        target_pool,
        &target_ep,
        &[],
        &[],
        200,
        &ProgressReporter::inert(),
    )
    .await
    .expect("compare data for the new column");
    assert!(
        pending.summary.updates > 0,
        "{tag}: 新增列的真实值应被识别为待更新, 实际 {:?}",
        pending.summary
    );
    apply_data(target_pool, target_ns, fill_plan.select(true, true, true)).await;

    let (filled_pass, _, _) = compare_data_core(
        source_pool,
        &source_ep,
        target_pool,
        &target_ep,
        &[],
        &[],
        200,
        &ProgressReporter::inert(),
    )
    .await
    .expect("re-compare after backfill");
    assert_eq!(
        (
            filled_pass.summary.inserts,
            filled_pass.summary.updates,
            filled_pass.summary.deletes
        ),
        (0, 0, 0),
        "{tag}: 回填后 users 应完全一致"
    );
    let backfilled = scalar_text(target_pool, target_ns, "users", "channel", target_dialect).await;
    assert_eq!(backfilled, "web-1", "{tag}: 数据同步应写入源端真实值");

    // ---- 11. now the constraint can be applied ----------------------------
    exec_all(source_pool, &tighten_column_ddl(source_dialect, source_ns)).await;
    let (tighten, tighten_sources, _) = compare_schema_core(
        source_pool,
        source_ns,
        target_pool,
        target_ns,
        CompareOptions::default(),
        None,
        &ProgressReporter::inert(),
    )
    .await
    .expect("compare tightened column");
    assert_eq!(
        tighten.summary.changed_columns, 1,
        "{tag}: 可空 -> 非空应被识别为一处列变更, 实际 {:?}",
        tighten.summary
    );
    let tighten_plan = generate(
        &tighten,
        &tighten_sources,
        None,
        Some(target_ns),
        &mut Vec::new(),
    );
    run_plan(target_pool, tighten_plan.into_iter().map(|s| s.sql).collect()).await;

    let (final_state, _, _) = compare_schema_core(
        source_pool,
        source_ns,
        target_pool,
        target_ns,
        CompareOptions::default(),
        None,
        &ProgressReporter::inert(),
    )
    .await
    .expect("re-compare after tightening");
    assert_structurally_identical(&final_state, &format!("{tag} 非空约束同步"));
    let tightened = crate::schema::introspect::load_table(target_pool, target_ns, "users")
        .await
        .expect("load target users");
    assert!(
        !tightened.column("channel").expect("channel").nullable,
        "{tag}: channel 最终应为非空"
    );

    // The data survived all of it.
    let survivor = scalar_text(target_pool, target_ns, "users", "channel", target_dialect).await;
    assert_eq!(survivor, "web-1", "{tag}: 收紧约束不应影响数据");
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

    apply_data(pool, dst, plan.select(true, true, true)).await;

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

    apply_data(pool, dst, plan.select(true, true, true)).await;

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

/// Table data browsing: pagination, `has_more`, and the column kinds the grid
/// switches on. PostgreSQL's `uuid`/`inet`/`text[]`/enum columns and MySQL's
/// TEXT columns (reported as BLOB on the wire) are the interesting cases.
async fn browse_scenario(pool: &Pool, dialect: Dialect, ns: &str, tag: &str) {
    drop_namespace(pool, dialect, ns).await;
    create_namespace(pool, dialect, ns).await;

    let table = dialect.qualified(Some(ns), "browse");
    let quoted = |name: &str| dialect.quote(name);

    match dialect {
        Dialect::Mysql => {
            exec(
                pool,
                &format!(
                    "CREATE TABLE {table} (
                        `id` INT NOT NULL,
                        `title` VARCHAR(64) NOT NULL,
                        `body` TEXT,
                        `amount` DECIMAL(10,2),
                        `flag` TINYINT(1) NOT NULL,
                        `created_at` DATETIME NOT NULL,
                        `meta` JSON,
                        `payload` LONGBLOB,
                        `status` ENUM('draft','live') NOT NULL,
                        PRIMARY KEY (`id`)
                     ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
                ),
            )
            .await;
        }
        Dialect::Postgres => {
            exec(
                pool,
                &format!(
                    "CREATE TYPE {}.\"mood\" AS ENUM ('draft','live')",
                    dialect.quote(ns)
                ),
            )
            .await;
            exec(
                pool,
                &format!(
                    "CREATE TABLE {table} (
                        \"id\" integer NOT NULL,
                        \"title\" character varying(64) NOT NULL,
                        \"body\" text,
                        \"amount\" numeric(10,2),
                        \"flag\" boolean NOT NULL,
                        \"created_at\" timestamp NOT NULL,
                        \"meta\" jsonb,
                        \"payload\" bytea,
                        \"status\" {}.\"mood\" NOT NULL,
                        \"uid\" uuid,
                        \"addr\" inet,
                        \"tags\" text[],
                        CONSTRAINT \"browse_pkey\" PRIMARY KEY (\"id\")
                     )",
                    dialect.quote(ns)
                ),
            )
            .await;
        }
    }

    let total = 10usize;
    for id in 1..=total {
        let sql = match dialect {
            Dialect::Mysql => format!(
                "INSERT INTO {table} (`id`,`title`,`body`,`amount`,`flag`,`created_at`,`meta`,`payload`,`status`)
                 VALUES ({id}, 'title-{id}', 'body-{id}', {id}.25, {}, '2024-01-{:02} 08:00:00',
                         '{{\"n\": {id}}}', X'0102', '{}')",
                if id % 2 == 0 { 1 } else { 0 },
                id,
                if id % 2 == 0 { "live" } else { "draft" }
            ),
            Dialect::Postgres => format!(
                "INSERT INTO {table} (\"id\",\"title\",\"body\",\"amount\",\"flag\",\"created_at\",\"meta\",\"payload\",\"status\",\"uid\",\"addr\",\"tags\")
                 VALUES ({id}, 'title-{id}', 'body-{id}', {id}.25, {}, '2024-01-{:02} 08:00:00',
                         '{{\"n\": {id}}}', '\\x0102'::bytea, '{}',
                         '00000000-0000-0000-0000-{:012}'::uuid, '10.0.0.{id}'::inet,
                         ARRAY['a','b']::text[])",
                if id % 2 == 0 { "true" } else { "false" },
                id,
                if id % 2 == 0 { "live" } else { "draft" },
                id
            ),
        };
        exec(pool, &sql).await;
    }

    // ---- first page -------------------------------------------------------
    let page1 = preview_table_data_core(pool, ns, "browse", Some(3), Some(0), &[], &[])
        .await
        .expect("browse page 1");
    assert_eq!(page1.rows.len(), 3, "{tag}: 首页应有 3 行");
    assert!(page1.has_more, "{tag}: 10 行取 3 行应当还有更多");
    assert_eq!(page1.limit, 3);
    assert_eq!(page1.offset, 0);
    assert_eq!(page1.primary_key, vec!["id".to_string()]);

    // ---- column kinds -----------------------------------------------------
    use crate::db::value::ColumnKind;
    let kind_of = |name: &str| -> ColumnKind {
        page1
            .columns
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{tag}: 缺少列 {name}"))
            .kind
    };
    assert_eq!(kind_of("id"), ColumnKind::Int, "{tag}");
    assert_eq!(kind_of("amount"), ColumnKind::Decimal, "{tag}");
    assert_eq!(kind_of("flag"), ColumnKind::Bool, "{tag}");
    assert_eq!(kind_of("created_at"), ColumnKind::DateTime, "{tag}");
    assert_eq!(kind_of("meta"), ColumnKind::Json, "{tag}");
    assert_eq!(kind_of("payload"), ColumnKind::Bytes, "{tag}");
    assert_eq!(kind_of("title"), ColumnKind::Str, "{tag}");
    assert_eq!(kind_of("body"), ColumnKind::Str, "{tag}");
    // MySQL has a real ENUM type; PostgreSQL's is a user-defined type read as text.
    match dialect {
        Dialect::Mysql => assert_eq!(kind_of("status"), ColumnKind::Str, "{tag}"),
        Dialect::Postgres => {
            assert_eq!(kind_of("status"), ColumnKind::Text, "{tag}");
            // Types the driver cannot decode natively must still come back.
            assert_eq!(kind_of("uid"), ColumnKind::Str, "{tag}");
            assert_eq!(kind_of("addr"), ColumnKind::Text, "{tag}");
            assert_eq!(kind_of("tags"), ColumnKind::Text, "{tag}");
        }
    }
    assert!(page1.columns.iter().any(|c| c.primary_key && c.name == "id"));

    // ---- cells ------------------------------------------------------------
    let column_index = |name: &str| -> usize {
        page1
            .columns
            .iter()
            .position(|c| c.name == name)
            .unwrap_or_else(|| panic!("{tag}: 缺少列 {name}"))
    };
    let text_at = |row: usize, name: &str| -> String {
        page1.rows[row][column_index(name)]
            .text
            .clone()
            .unwrap_or_else(|| panic!("{tag}: 第 {row} 行 {name} 为空"))
    };
    assert_eq!(page1.rows.len(), 3);
    for (offset, row) in page1.rows.iter().enumerate() {
        assert_eq!(
            row.len(),
            page1.columns.len(),
            "{tag}: 第 {offset} 行的单元格数与列数不一致"
        );
    }
    // Regression guard: MySQL reports TEXT columns as BLOB on the wire, and the
    // information_schema text columns carry the binary flag. Reading any of them
    // as bytes would show up here as a hex string.
    for name in ["title", "body", "status"] {
        let value = text_at(0, name);
        assert_eq!(value, format!("{name}-1").replace("status-1", "draft"), "{tag}: {name}");
    }
    assert_eq!(text_at(0, "meta"), "{\"n\":1}", "{tag}: JSON 应渲染为文本");
    if dialect == Dialect::Postgres {
        assert_eq!(text_at(0, "uid"), "00000000-0000-0000-0000-000000000001");
        // PostgreSQL renders `inet` as text with its netmask.
        assert!(
            text_at(0, "addr").starts_with("10.0.0.1"),
            "{tag}: inet 应可读，实际 {}",
            text_at(0, "addr")
        );
        // PostgreSQL renders `text[]` as `{a,b}` (quotes only when needed).
        let tags = text_at(0, "tags");
        assert!(
            tags.contains('a') && tags.contains('b') && tags.starts_with('{'),
            "{tag}: 数组应以 text 形式返回，实际 {tags}"
        );
    }

    // ---- second page continues the ordering without overlapping -----------
    let page2 = preview_table_data_core(pool, ns, "browse", Some(3), Some(3), &[], &[])
        .await
        .expect("browse page 2");
    assert_eq!(page2.offset, 3);
    let id_at = |page: &crate::commands::TableData, row: usize| -> i64 {
        page.rows[row]
            .iter()
            .zip(page.columns.iter())
            .find(|(_, column)| column.name == "id")
            .and_then(|(cell, _)| cell.text.as_deref())
            .and_then(|text| text.parse().ok())
            .unwrap_or_else(|| panic!("{tag}: 无法读取 id"))
    };
    assert_eq!(id_at(&page1, 0), 1);
    assert_eq!(id_at(&page1, 2), 3);
    assert_eq!(id_at(&page2, 0), 4, "{tag}: 第二页应从第 4 行开始");

    // ---- last page reports no more ---------------------------------------
    let last = preview_table_data_core(pool, ns, "browse", Some(3), Some(9), &[], &[])
        .await
        .expect("browse last page");
    assert_eq!(last.rows.len(), 1, "{tag}: 最后一页只剩 1 行");
    assert!(!last.has_more, "{tag}: 最后一页不应还有更多");

    // ---- a limit beyond the row count ------------------------------------
    let all = preview_table_data_core(pool, ns, "browse", Some(50), Some(0), &[], &[])
        .await
        .expect("browse everything");
    assert_eq!(all.rows.len(), total, "{tag}: 应返回全部 {total} 行");
    assert!(!all.has_more, "{tag}: 全部返回后不应还有更多");

    // ---- the generated SQL is the one that ran ---------------------------
    assert!(
        last.sql.to_uppercase().contains("ORDER BY"),
        "{tag}: 分页查询必须按主键排序: {}",
        last.sql
    );

    let _ = quoted;
    drop_namespace(pool, dialect, ns).await;
}

#[tokio::test]
async fn live_mysql_table_browsing() {
    let Some(target) = env_target("DATASYNC_MYSQL", Dialect::Mysql) else {
        eprintln!("跳过：未设置 DATASYNC_MYSQL_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Mysql).await;
    browse_scenario(&pool, Dialect::Mysql, "datasync_live_browse", "mysql 浏览").await;
}

#[tokio::test]
async fn live_postgres_table_browsing() {
    let Some(target) = env_target("DATASYNC_PG", Dialect::Postgres) else {
        eprintln!("跳过：未设置 DATASYNC_PG_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Postgres).await;
    browse_scenario(&pool, Dialect::Postgres, "datasync_live_browse", "pg 浏览").await;
}

/// Dropping tables that are still referenced.
///
/// Covers two shapes that a naive "drop the constraints I was told about, then
/// drop the tables" plan gets wrong: a reference cycle between two tables, and a
/// target-only table referenced by a table whose own changes were not selected.
async fn fk_drop_scenario(pool: &Pool, dialect: Dialect, src: &str, dst: &str, tag: &str) {
    drop_namespace(pool, dialect, src).await;
    drop_namespace(pool, dialect, dst).await;
    create_namespace(pool, dialect, src).await;
    create_namespace(pool, dialect, dst).await;

    // Everything goes into the target namespace.
    let q = |name: &str| dialect.qualified(Some(dst), name);
    let col = |name: &str| dialect.quote(name);
    let engine = match dialect {
        Dialect::Mysql => " ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        Dialect::Postgres => "",
    };
    let int = match dialect {
        Dialect::Mysql => "INT",
        Dialect::Postgres => "integer",
    };

    // The target holds a reference cycle plus a table that is referenced by a
    // table the source also has.
    let target_setup = vec![
        format!("CREATE TABLE {} ({c} {int} NOT NULL, {b} {int}, PRIMARY KEY ({c})){engine}", q("node_a"), c = col("id"), b = col("b_id")),
        format!("CREATE TABLE {} ({c} {int} NOT NULL, {a} {int}, PRIMARY KEY ({c})){engine}", q("node_b"), c = col("id"), a = col("a_id")),
        format!("ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({b}) REFERENCES {} ({c})",
            q("node_a"), col("fk_a_b"), q("node_b"), c = col("id"), b = col("b_id")),
        format!("ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({a}) REFERENCES {} ({c})",
            q("node_b"), col("fk_b_a"), q("node_a"), c = col("id"), a = col("a_id")),
        format!("CREATE TABLE {} ({c} {int} NOT NULL, PRIMARY KEY ({c})){engine}", q("leftover"), c = col("id")),
        format!("CREATE TABLE {} ({c} {int} NOT NULL, {l} {int}, PRIMARY KEY ({c})){engine}",
            q("keeper"), c = col("id"), l = col("leftover_id")),
        format!("ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({l}) REFERENCES {} ({c})",
            q("keeper"), col("fk_keeper_leftover"), q("leftover"), c = col("id"), l = col("leftover_id")),
    ];
    exec_all(pool, &target_setup).await;

    // The source has only `keeper`, and without the foreign key.
    exec_all(
        pool,
        &[format!(
            "CREATE TABLE {} ({c} {int} NOT NULL, {l} {int}, PRIMARY KEY ({c})){engine}",
            dialect.qualified(Some(src), "keeper"),
            c = col("id"),
            l = col("leftover_id")
        )],
    )
    .await;

    let (diff, sources, _) = compare_schema_core(
        pool,
        src,
        pool,
        dst,
        CompareOptions::default(),
        None,
        &ProgressReporter::inert(),
    )
    .await
    .expect("compare");
    let _ = sources;
    assert_eq!(
        diff.summary.missing_tables, 3,
        "{tag}: node_a / node_b / leftover 都应被识别为待删除, 实际 {:?}",
        diff.summary
    );

    let mut warnings = Vec::new();
    let mut plan = generate(&diff, &HashMap::new(), None, Some(dst), &mut warnings);
    let fk_drops = plan.iter().filter(|s| s.category == "drop_foreign_key").count();
    let table_drops = plan.iter().filter(|s| s.category == "drop_table").count();
    assert_eq!(table_drops, 3, "{tag}: 应删除 3 张表");
    assert_eq!(fk_drops, 3, "{tag}: 三个外键都要先解除");
    let last_fk = plan
        .iter()
        .rposition(|s| s.category == "drop_foreign_key")
        .expect("有外键删除");
    let first_table = plan
        .iter()
        .position(|s| s.category == "drop_table")
        .expect("有表删除");
    assert!(
        last_fk < first_table,
        "{tag}: 外键删除必须全部先于表删除: {:?}",
        plan.iter().map(|s| (&s.category, &s.sql)).collect::<Vec<_>>()
    );

    // Executing the plan must actually succeed, which is the whole point.
    run_plan(pool, plan.drain(..).map(|s| s.sql).collect()).await;

    let (after, _, _) = compare_schema_core(
        pool,
        src,
        pool,
        dst,
        CompareOptions::default(),
        None,
        &ProgressReporter::inert(),
    )
    .await
    .expect("re-compare");
    assert_structurally_identical(&after, &format!("{tag} 删表"));

    drop_namespace(pool, dialect, src).await;
    drop_namespace(pool, dialect, dst).await;
}

#[tokio::test]
async fn live_mysql_dropping_referenced_tables() {
    let Some(target) = env_target("DATASYNC_MYSQL", Dialect::Mysql) else {
        eprintln!("跳过：未设置 DATASYNC_MYSQL_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Mysql).await;
    fk_drop_scenario(
        &pool,
        Dialect::Mysql,
        "datasync_live_fk_src",
        "datasync_live_fk_dst",
        "mysql 删表",
    )
    .await;
}

#[tokio::test]
async fn live_postgres_dropping_referenced_tables() {
    let Some(target) = env_target("DATASYNC_PG", Dialect::Postgres) else {
        eprintln!("跳过：未设置 DATASYNC_PG_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Postgres).await;
    fk_drop_scenario(
        &pool,
        Dialect::Postgres,
        "datasync_live_fk_src",
        "datasync_live_fk_dst",
        "pg 删表",
    )
    .await;
}

/// A failing DDL statement must not leave the target half-migrated.
///
/// The plan deliberately contains a statement that succeeds followed by one that
/// cannot: adding a NOT NULL column with no default to a populated table. On
/// PostgreSQL the whole plan is one transaction, so the first change must be
/// undone as well.
#[tokio::test]
async fn live_postgres_ddl_rolls_back_on_failure() {
    let Some(target) = env_target("DATASYNC_PG", Dialect::Postgres) else {
        eprintln!("跳过：未设置 DATASYNC_PG_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Postgres).await;
    let (src, dst) = ("datasync_live_rb_src", "datasync_live_rb_dst");
    drop_namespace(&pool, Dialect::Postgres, src).await;
    drop_namespace(&pool, Dialect::Postgres, dst).await;
    create_namespace(&pool, Dialect::Postgres, src).await;
    create_namespace(&pool, Dialect::Postgres, dst).await;

    assert!(
        pool.supports_ddl_transaction(),
        "PostgreSQL 应支持 DDL 事务"
    );

    let source_table = Dialect::Postgres.qualified(Some(src), "users");
    let target_table = Dialect::Postgres.qualified(Some(dst), "users");

    // The target already holds rows.
    exec(
        &pool,
        &format!(
            "CREATE TABLE {target_table} (\"id\" integer NOT NULL, \"email\" character varying(32), CONSTRAINT \"users_pkey\" PRIMARY KEY (\"id\"))"
        ),
    )
    .await;
    exec(
        &pool,
        &format!("INSERT INTO {target_table} VALUES (1, 'a'), (2, 'b')"),
    )
    .await;

    // The source has an extra nullable column and an extra NOT NULL column.
    exec(
        &pool,
        &format!(
            "CREATE TABLE {source_table} (
                \"id\" integer NOT NULL,
                \"email\" character varying(32),
                \"extra\" character varying(32),
                \"code\" character varying(32) NOT NULL,
                CONSTRAINT \"users_pkey\" PRIMARY KEY (\"id\"))"
        ),
    )
    .await;
    exec(
        &pool,
        &format!("INSERT INTO {source_table} VALUES (1, 'a', 'x', 'c1'), (2, 'b', 'y', 'c2')"),
    )
    .await;

    let (diff, sources, _) = compare_schema_core(
        &pool,
        src,
        &pool,
        dst,
        CompareOptions::default(),
        None,
        &ProgressReporter::inert(),
    )
    .await
    .expect("compare");
    assert_eq!(diff.summary.new_columns, 2, "{:?}", diff.summary);

    let plan = generate(&diff, &sources, None, Some(dst), &mut Vec::new());
    let sql: Vec<String> = plan
        .iter()
        .filter(|s| s.category == "add_column")
        .map(|s| s.sql.clone())
        .collect();
    assert_eq!(sql.len(), 2, "{sql:?}");

    let run = pool
        .run_statements(&sql, true, &ProgressReporter::inert())
        .await
        .expect("run ddl");

    assert!(run.transactional, "应使用事务");
    assert!(run.rolled_back, "失败后应整体回滚");
    assert_eq!(
        run.outcomes.iter().filter(|o| o.ok).count(),
        1,
        "第一条语句应当成功执行过"
    );
    assert_eq!(run.outcomes.iter().filter(|o| !o.ok).count(), 1);
    assert_eq!(run.skipped, 0, "两条语句都已尝试");

    // The decisive check: the column added by the *successful* statement is gone.
    let after = crate::schema::introspect::load_table(&pool, dst, "users")
        .await
        .expect("load target users");
    assert!(
        after.column("extra").is_none(),
        "回滚后不应留下任何变更，实际列: {:?}",
        after.columns.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    assert!(after.column("code").is_none());
    // And the rows are untouched.
    let rows = scalar_count(&pool, dst, "users", Dialect::Postgres).await;
    assert_eq!(rows, 2);

    // Running the same plan without a transaction would leave the first change
    // behind, which is why the option matters.
    let run = pool
        .run_statements(&sql, false, &ProgressReporter::inert())
        .await
        .expect("run ddl without a transaction");
    assert!(!run.transactional);
    assert!(!run.rolled_back);
    // Both statements were attempted (the second one failed), so nothing was
    // skipped; the point is that the first one's change survives.
    assert_eq!(run.skipped, 0);
    assert_eq!(run.outcomes.iter().filter(|o| o.ok).count(), 1);
    let after = crate::schema::introspect::load_table(&pool, dst, "users")
        .await
        .expect("load target users");
    assert!(
        after.column("extra").is_some(),
        "非事务模式下已执行的变更会保留 —— 这正是需要提示用户的事实"
    );

    drop_namespace(&pool, Dialect::Postgres, src).await;
    drop_namespace(&pool, Dialect::Postgres, dst).await;
}

#[tokio::test]
async fn live_mysql_reports_that_ddl_cannot_be_rolled_back() {
    let Some(target) = env_target("DATASYNC_MYSQL", Dialect::Mysql) else {
        eprintln!("跳过：未设置 DATASYNC_MYSQL_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Mysql).await;
    // MySQL commits every DDL statement implicitly, so the tool must say so
    // rather than implying a rollback it cannot perform.
    assert!(!pool.supports_ddl_transaction());
}

/// Table level operations: edit columns, rename a table, drop a table that is
/// still referenced. These are the actions offered by the tree context menu.
async fn table_ops_scenario(pool: &Pool, dialect: Dialect, ns: &str, tag: &str) {
    drop_namespace(pool, dialect, ns).await;
    create_namespace(pool, dialect, ns).await;

    let q = |name: &str| dialect.qualified(Some(ns), name);
    let c = |name: &str| dialect.quote(name);
    let engine = match dialect {
        Dialect::Mysql => " ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        Dialect::Postgres => "",
    };
    let int = match dialect {
        Dialect::Mysql => "INT",
        Dialect::Postgres => "integer",
    };
    let text = match dialect {
        Dialect::Mysql => "VARCHAR(32)",
        Dialect::Postgres => "character varying(32)",
    };

    exec_all(
        pool,
        &[
            format!(
                "CREATE TABLE {} ({id} {int} NOT NULL, {name} {text}, PRIMARY KEY ({id})){engine}",
                q("parent"),
                id = c("id"),
                name = c("name")
            ),
            format!(
                "CREATE TABLE {} ({id} {int} NOT NULL, {pid} {int}, {note} {text}, PRIMARY KEY ({id})){engine}",
                q("child"),
                id = c("id"),
                pid = c("parent_id"),
                note = c("note")
            ),
            format!(
                "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({pid}) REFERENCES {} ({id})",
                q("child"),
                c("fk_child_parent"),
                q("parent"),
                pid = c("parent_id"),
                id = c("id")
            ),
            format!("INSERT INTO {} VALUES (1, 'p1')", q("parent")),
            format!(
                "INSERT INTO {} VALUES (10, 1, 'keep-me'), (11, 1, 'keep-me-too')",
                q("child")
            ),
        ],
    )
    .await;

    // ---- 1. edit columns: add one, rename another --------------------------
    let current = crate::schema::introspect::load_table(pool, ns, "child")
        .await
        .expect("load child");
    let mut desired: Vec<crate::schema::model::ColumnInfo> = current
        .columns
        .iter()
        .map(|column| {
            let mut copy = column.clone();
            if copy.name == "note" {
                copy.name = "remark".into();
            }
            copy
        })
        .collect();
    let mut extra = crate::schema::model::ColumnInfo {
        name: "extra".into(),
        ordinal: desired.len() as i64 + 1,
        data_type: "VARCHAR".into(),
        full_type: text.into(),
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
    };
    desired.push(extra.clone());
    extra.name = "extra".into();

    let (statements, _) = plan_alter_table(
        pool,
        ns,
        "child",
        &desired,
        None,
        &[ColumnRename {
            from: "note".into(),
            to: "remark".into(),
        }],
    )
    .await
    .expect("plan alter");

    let rename_count = statements
        .iter()
        .filter(|s| s.category == "rename_column")
        .count();
    assert_eq!(rename_count, 1, "{tag}: 改名应产生一条重命名语句");
    assert!(
        !statements.iter().any(|s| s.category == "create_table"
            || s.category == "drop_table"),
        "{tag}: 字段编辑器绝不能创建或删除表"
    );
    // A rename must not be expressed as drop + add.
    let dropped: Vec<&str> = statements
        .iter()
        .filter(|s| s.category == "drop_column")
        .map(|s| s.object.as_str())
        .collect();
    assert!(dropped.is_empty(), "{tag}: 改名不应表现为删除字段: {dropped:?}");

    run_plan(pool, statements.into_iter().map(|s| s.sql).collect()).await;

    let after = crate::schema::introspect::load_table(pool, ns, "child")
        .await
        .expect("reload child");
    let names: Vec<&str> = after.columns.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"remark"), "{tag}: 应有重命名后的列: {names:?}");
    assert!(names.contains(&"extra"), "{tag}: 应有新增列: {names:?}");
    assert!(!names.contains(&"note"), "{tag}: 旧列名应消失: {names:?}");

    // The decisive check: renaming kept the data.
    let kept = scalar_text(pool, ns, "child", "remark", dialect).await;
    assert_eq!(kept, "keep-me", "{tag}: 重命名必须保留数据");

    // ---- 1b. indexes: add, then remove -------------------------------------
    let desired_columns = crate::schema::introspect::load_table(pool, ns, "child")
        .await
        .expect("reload child")
        .columns;

    // Without a submitted index list the planner must leave them alone.
    let (untouched, _) = plan_alter_table(pool, ns, "child", &desired_columns, None, &[])
        .await
        .expect("plan without indexes");
    assert!(
        !untouched
            .iter()
            .any(|s| s.category == "create_index" || s.category == "drop_index"),
        "{tag}: 未提交索引列表时不应改动索引"
    );

    let new_index = |name: &str, columns: &[&str], is_unique: bool| crate::schema::model::IndexInfo {
        name: name.into(),
        columns: columns.iter().map(|c| (*c).to_string()).collect(),
        is_unique,
        index_type: None,
        required_by_foreign_key: false,
    };
    // Deliberately replaces the index MySQL created for the foreign key with a
    // wider one that also covers `parent_id`. On MySQL the drop and the add have
    // to travel in the same ALTER, otherwise the drop is refused.
    let wanted = vec![
        new_index("idx_child_remark", &["remark"], false),
        new_index("uq_child_pid_remark", &["parent_id", "remark"], true),
    ];
    let (plan, warnings) = plan_alter_table(
        pool,
        ns,
        "child",
        &desired_columns,
        Some(&wanted),
        &[],
    )
    .await
    .expect("plan indexes");
    // MySQL merges the index changes into one ALTER TABLE, so the count comes
    // from the specifications rather than from the number of statements.
    let created: usize = plan
        .iter()
        .filter_map(|s| match s.category.as_str() {
            "alter_indexes" => Some(s.sql.matches("ADD ").count()),
            "create_index" => Some(1),
            _ => None,
        })
        .sum();
    assert_eq!(
        created,
        2,
        "{tag}: 应创建 2 个索引: {:?}",
        plan.iter().map(|s| &s.sql).collect::<Vec<_>>()
    );
    let _ = &warnings;
    run_plan(pool, plan.into_iter().map(|s| s.sql).collect()).await;

    let after = crate::schema::introspect::load_table(pool, ns, "child")
        .await
        .expect("reload after indexes");
    let names: Vec<&str> = after.indexes.iter().map(|i| i.name.as_str()).collect();
    assert!(
        names.contains(&"idx_child_remark") && names.contains(&"uq_child_pid_remark"),
        "{tag}: 索引应已存在: {names:?}"
    );
    let unique = after
        .indexes
        .iter()
        .find(|i| i.name == "uq_child_pid_remark")
        .expect("unique index");
    assert!(unique.is_unique, "{tag}: 唯一索引应保持唯一");
    assert_eq!(
        unique.columns,
        vec!["parent_id".to_string(), "remark".to_string()],
        "{tag}: 复合索引的列顺序必须保留"
    );

    // Dropping an index the keys do not need is a plain removal.
    let keep_wide = vec![new_index("uq_child_pid_remark", &["parent_id", "remark"], true)];
    let (plan, _) = plan_alter_table(
        pool,
        ns,
        "child",
        &desired_columns,
        Some(&keep_wide),
        &[],
    )
    .await
    .expect("plan index drop");
    let dropped: Vec<&str> = plan
        .iter()
        .filter(|s| s.category == "drop_index" || s.category == "alter_indexes")
        .filter(|s| s.sql.contains("DROP INDEX"))
        .map(|s| s.object.as_str())
        .collect();
    assert!(!dropped.is_empty(), "{tag}: 应删除被移除的索引");
    run_plan(pool, plan.into_iter().map(|s| s.sql).collect()).await;

    let after = crate::schema::introspect::load_table(pool, ns, "child")
        .await
        .expect("reload after drop");
    let names: Vec<&str> = after.indexes.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, vec!["uq_child_pid_remark"], "{tag}: 只应剩一个索引");

    // The remaining index supports the foreign key, so it must be protected
    // even though the caller left it out of the list.
    let (plan, warnings) = plan_alter_table(
        pool,
        ns,
        "child",
        &desired_columns,
        Some(&[]),
        &[],
    )
    .await
    .expect("plan removing everything");
    assert!(
        !plan
            .iter()
            .any(|s| s.sql.contains("DROP INDEX") && s.sql.contains("uq_child_pid_remark")),
        "{tag}: 外键唯一的支撑索引不得被删除"
    );
    assert!(
        warnings.iter().any(|w| w.contains("支撑索引")),
        "{tag}: 应说明为何保留: {warnings:?}"
    );

    // Index definitions are validated before anything is executed.
    let bad = vec![crate::schema::model::IndexInfo {
        name: "idx_bad".into(),
        columns: vec!["does_not_exist".into()],
        is_unique: false,
        index_type: None,
        required_by_foreign_key: false,
    }];
    let error = plan_alter_table(pool, ns, "child", &desired_columns, Some(&bad), &[])
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("不存在的字段"),
        "{tag}: {error}"
    );

    // ---- 2. rename the table ----------------------------------------------
    let sql = vec![crate::schema::ddl::rename_table_ddl(
        "child",
        "child_renamed",
        dialect,
        Some(ns),
    )];
    let run = pool
        .run_statements(&sql, true, &ProgressReporter::inert())
        .await
        .expect("rename table");
    assert_eq!(run.outcomes.iter().filter(|o| !o.ok).count(), 0);
    let tables: Vec<String> = crate::schema::introspect::list_tables(pool, ns)
        .await
        .expect("list tables")
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(tables.contains(&"child_renamed".to_string()), "{tables:?}");
    assert!(!tables.contains(&"child".to_string()), "{tables:?}");
    let kept = scalar_text(pool, ns, "child_renamed", "remark", dialect).await;
    assert_eq!(kept, "keep-me", "{tag}: 重命名表必须保留数据");

    // ---- 3. drop a table that is still referenced -------------------------
    let (statements, dependent_views, _) = plan_drop_scope(
        pool,
        ns,
        &["parent".to_string()],
    )
    .await
    .expect("plan drop");
    assert!(dependent_views.is_empty(), "{dependent_views:?}");
    let fk_drops = statements
        .iter()
        .filter(|s| s.category == "drop_foreign_key")
        .count();
    assert_eq!(
        fk_drops, 1,
        "{tag}: 删表前必须解除 child_renamed 上的外键: {:?}",
        statements.iter().map(|s| (&s.category, &s.sql)).collect::<Vec<_>>()
    );
    run_plan(pool, statements.into_iter().map(|s| s.sql).collect()).await;

    let tables: Vec<String> = crate::schema::introspect::list_tables(pool, ns)
        .await
        .expect("list tables")
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(!tables.contains(&"parent".to_string()), "{tag}: parent 应已删除");
    assert!(
        tables.contains(&"child_renamed".to_string()),
        "{tag}: 未被选中的表必须保留: {tables:?}"
    );
    // Its data survived the foreign key being dropped.
    assert_eq!(
        crate::schema::introspect::load_table(pool, ns, "child_renamed")
            .await
            .expect("reload")
            .foreign_keys
            .len(),
        0,
        "{tag}: 外键应已被解除"
    );
    let rows = scalar_count(pool, ns, "child_renamed", dialect).await;
    assert_eq!(rows, 2, "{tag}: 删除父表不应影响子表数据");

    drop_namespace(pool, dialect, ns).await;
}

#[tokio::test]
async fn live_mysql_table_operations() {
    let Some(target) = env_target("DATASYNC_MYSQL", Dialect::Mysql) else {
        eprintln!("跳过：未设置 DATASYNC_MYSQL_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Mysql).await;
    table_ops_scenario(&pool, Dialect::Mysql, "datasync_live_ops", "mysql 表操作").await;
}

#[tokio::test]
async fn live_postgres_table_operations() {
    let Some(target) = env_target("DATASYNC_PG", Dialect::Postgres) else {
        eprintln!("跳过：未设置 DATASYNC_PG_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Postgres).await;
    table_ops_scenario(&pool, Dialect::Postgres, "datasync_live_ops", "pg 表操作").await;
}

/// Fetch a single value through SQL that casts to text.
async fn text_of(pool: &Pool, sql: &str) -> Option<String> {
    let rows = pool
        .fetch(sql, &[], &[crate::db::value::ColumnKind::Str])
        .await
        .unwrap_or_else(|e| panic!("查询失败 {sql}: {e}"));
    rows.first()
        .and_then(|row| row.first())
        .and_then(|value| value.display())
}

fn excel_fixture() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/import.xlsx")
        .to_string_lossy()
        .to_string()
}

/// Import the fixture workbook into a real table.
///
/// The fixture carries a genuine Excel date serial, a boolean, numbers stored as
/// floats, blank cells and one row whose values do not fit — so this covers the
/// whole path from file to stored value.
async fn excel_import_scenario(pool: &Pool, dialect: Dialect, ns: &str, tag: &str) {
    drop_namespace(pool, dialect, ns).await;
    create_namespace(pool, dialect, ns).await;

    let q = |name: &str| dialect.qualified(Some(ns), name);
    let c = |name: &str| dialect.quote(name);
    let engine = match dialect {
        Dialect::Mysql => " ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        Dialect::Postgres => "",
    };
    let ddl = match dialect {
        Dialect::Mysql => format!(
            "CREATE TABLE {} (
                {id} INT NOT NULL,
                {name} VARCHAR(64),
                {amount} DECIMAL(12,2),
                {ratio} DECIMAL(10,4),
                {active} TINYINT(1),
                {created_at} DATETIME,
                {payload} JSON,
                {extra} VARCHAR(32),
                {derived} VARCHAR(64) GENERATED ALWAYS AS (UPPER({name})) STORED,
                PRIMARY KEY ({id})
             ){engine}",
            q("sheet_import"),
            id = c("id"),
            name = c("name"),
            amount = c("amount"),
            ratio = c("ratio"),
            active = c("active"),
            created_at = c("created_at"),
            payload = c("payload"),
            extra = c("extra"),
            derived = c("name_upper"),
        ),
        Dialect::Postgres => format!(
            "CREATE TABLE {} (
                \"id\" integer NOT NULL,
                \"name\" character varying(64),
                \"amount\" numeric(12,2),
                \"ratio\" numeric(10,4),
                \"active\" boolean,
                \"created_at\" timestamp,
                \"payload\" jsonb,
                \"extra\" character varying(32),
                \"name_upper\" character varying(64) GENERATED ALWAYS AS (upper(\"name\")) STORED,
                PRIMARY KEY (\"id\")
             )",
            q("sheet_import"),
        ),
    };
    exec_all(pool, &[ddl]).await;

    let structure = crate::schema::introspect::load_table(pool, ns, "sheet_import")
        .await
        .expect("load table");
    let (sheet, _, _) = excel::read_sheet(&excel_fixture(), Some("数据"), Some(0), None)
        .expect("read fixture");
    assert_eq!(sheet.total_rows, 4, "{tag}: 夹具应有 4 行数据");

    // Headers match column names, so the suggestion should cover every writable
    // column and leave the generated one alone.
    let suggested = suggest_mapping(&structure, &sheet.headers);
    let suggested_names: Vec<&str> = suggested.iter().map(|m| m.column.as_str()).collect();
    assert!(
        suggested_names.contains(&"name") && suggested_names.contains(&"created_at"),
        "{tag}: 建议映射应包含同名列: {suggested_names:?}"
    );
    assert!(
        !suggested_names.contains(&"name_upper"),
        "{tag}: 生成列不应出现在建议映射里: {suggested_names:?}"
    );

    let mapping = suggested;
    let outcome = run_import(
        pool,
        ns,
        &structure,
        &sheet,
        &mapping,
        &ImportOptions::default(),
        &ProgressReporter::inert(),
    )
    .await
    .expect("import");

    assert_eq!(outcome.read, 4, "{tag}: 应读到 4 行");
    assert_eq!(outcome.prepared, 3, "{tag}: 3 行可转换");
    assert_eq!(outcome.skipped, 1, "{tag}: 1 行数值非法应被跳过");
    assert_eq!(outcome.failed, 0, "{tag}: 不应有语句级失败");
    assert_eq!(outcome.inserted, 3, "{tag}: 应写入 3 行");
    assert_eq!(outcome.errors.len(), 1, "{tag}: 应记录 1 条错误");
    assert_eq!(outcome.errors[0].row, 4, "{tag}: 出问题的应是第 4 行");
    assert_eq!(
        outcome.errors[0].column.as_deref(),
        Some("amount"),
        "{tag}: 应指出是 amount 列"
    );

    // Values arrived converted, not as raw strings.
    let rows = scalar_count(pool, ns, "sheet_import", dialect).await;
    assert_eq!(rows, 3, "{tag}: 表中应有 3 行");

    let name = text_of(
        pool,
        &format!(
            "SELECT {} FROM {} WHERE {} = 1",
            c("name"),
            q("sheet_import"),
            c("id")
        ),
    )
    .await;
    assert_eq!(name.as_deref(), Some("苹果"), "{tag}: 中文与主键匹配");

    let escaped = text_of(
        pool,
        &format!(
            "SELECT {} FROM {} WHERE {} = 2",
            c("name"),
            q("sheet_import"),
            c("id")
        ),
    )
    .await;
    assert_eq!(
        escaped.as_deref(),
        Some("O'Brien"),
        "{tag}: 含引号的字符串应完整写入"
    );

    // The fixture stores this as an Excel date serial with a date format; a
    // reader that ignored the format would store 45356 here.
    let created_sql = match dialect {
        Dialect::Mysql => format!(
            "SELECT CAST({} AS CHAR) FROM {} WHERE {} = 1",
            c("created_at"),
            q("sheet_import"),
            c("id")
        ),
        Dialect::Postgres => format!(
            "SELECT {}::text FROM {} WHERE \"id\" = 1",
            c("created_at"),
            q("sheet_import")
        ),
    };
    let created = text_of(pool, &created_sql).await;
    assert!(
        created.as_deref().is_some_and(|value| value.starts_with("2024-03-05")),
        "{tag}: Excel 日期序列号必须还原为日期, 实际 {created:?}"
    );

    let amount_sql = match dialect {
        Dialect::Mysql => format!(
            "SELECT CAST({} AS CHAR) FROM {} WHERE {} = 1",
            c("amount"),
            q("sheet_import"),
            c("id")
        ),
        Dialect::Postgres => format!(
            "SELECT {}::text FROM {} WHERE \"id\" = 1",
            c("amount"),
            q("sheet_import")
        ),
    };
    let amount = text_of(pool, &amount_sql).await;
    assert_eq!(amount.as_deref(), Some("10.50"), "{tag}: 数值应按列精度存储");

    // Row 3 had blank cells, which must land as NULL rather than empty strings.
    let blank = text_of(
        pool,
        &format!(
            "SELECT {} FROM {} WHERE {} = 3",
            c("amount"),
            q("sheet_import"),
            c("id")
        ),
    )
    .await;
    assert_eq!(blank, None, "{tag}: 空单元格应写入 NULL");

    let payload_sql = match dialect {
        Dialect::Mysql => format!(
            "SELECT CAST({} AS CHAR) FROM {} WHERE {} = 1",
            c("payload"),
            q("sheet_import"),
            c("id")
        ),
        Dialect::Postgres => format!(
            "SELECT {}::text FROM {} WHERE \"id\" = 1",
            c("payload"),
            q("sheet_import")
        ),
    };
    let payload = text_of(pool, &payload_sql).await;
    assert!(
        payload.as_deref().is_some_and(|value| value.contains('1')),
        "{tag}: JSON 应被接受, 实际 {payload:?}"
    );

    // ---- 覆盖模式：同一份数据再导一次，应更新而不是插入重复行 ----------
    let name_sql = |id: i32| {
        format!(
            "SELECT {} FROM {} WHERE {} = {id}",
            c("name"),
            q("sheet_import"),
            c("id")
        )
    };
    exec(
        pool,
        &format!(
            "UPDATE {} SET {} = 'changed' WHERE {} = 1",
            q("sheet_import"),
            c("name"),
            c("id")
        ),
    )
    .await;

    let upsert_options = ImportOptions {
        strategy: ImportStrategy::Upsert,
        ..ImportOptions::default()
    };
    let overwritten = run_import(
        pool,
        ns,
        &structure,
        &sheet,
        &mapping,
        &upsert_options,
        &ProgressReporter::inert(),
    )
    .await
    .expect("upsert import");

    assert_eq!(overwritten.failed, 0, "{tag}: 覆盖模式不应有失败");
    assert_eq!(
        scalar_count(pool, ns, "sheet_import", dialect).await,
        3,
        "{tag}: 覆盖模式不得产生重复行"
    );
    assert_eq!(
        overwritten.existing_keys,
        Some(3),
        "{tag}: 3 行的主键都已存在，应被识别为覆盖"
    );
    let restored = text_of(pool, &name_sql(1)).await;
    assert_eq!(
        restored.as_deref(),
        Some("苹果"),
        "{tag}: 覆盖模式应把被改动的值写回"
    );

    // 新增模式遇到同样的数据则应报主键冲突，证明两种策略确实不同。
    let append_again = run_import(
        pool,
        ns,
        &structure,
        &sheet,
        &mapping,
        &ImportOptions::default(),
        &ProgressReporter::inert(),
    )
    .await
    .expect("append import");
    assert!(
        append_again.failed > 0,
        "{tag}: 新增模式遇到已存在的主键应当失败"
    );

    // ---- a duplicate key must not leave a half-imported table --------------
    exec(
        pool,
        &format!("DELETE FROM {}", q("sheet_import")),
    )
    .await;
    exec(
        pool,
        &format!(
            "INSERT INTO {} ({}) VALUES (2)",
            q("sheet_import"),
            c("id")
        ),
    )
    .await;

    let one_at_a_time = ImportOptions {
        batch_size: 1,
        use_transaction: true,
        ..ImportOptions::default()
    };
    let second = run_import(
        pool,
        ns,
        &structure,
        &sheet,
        &mapping,
        &one_at_a_time,
        &ProgressReporter::inert(),
    )
    .await
    .expect("second import");

    assert!(second.failed > 0, "{tag}: 主键冲突应被报告");
    let after = scalar_count(pool, ns, "sheet_import", dialect).await;
    if dialect == Dialect::Postgres {
        assert!(second.rolled_back, "{tag}: PostgreSQL 应整体回滚");
        assert_eq!(second.inserted, 0, "{tag}: 回滚后插入数应为 0");
        assert_eq!(
            after, 1,
            "{tag}: 回滚后只应剩下导入前的那一行, 实际 {after}"
        );
        assert!(
            second.warnings.iter().any(|w| w.contains("事务已回滚")),
            "{tag}: 应提示已回滚: {:?}",
            second.warnings
        );
    } else {
        // MySQL commits DDL — and here every statement — so a failure cannot be
        // undone; the tool must say so rather than imply otherwise.
        assert!(!second.rolled_back, "{tag}: MySQL 无法回滚");
        assert!(!second.transactional);
        assert!(
            second
                .warnings
                .iter()
                .any(|w| w.contains("不支持事务回滚") && w.contains("已经写入")),
            "{tag}: 应明确说明已写入的行无法回滚: {:?}",
            second.warnings
        );
        // Execution stops at the first failure, so the batch before it stays and
        // the batch after it is never attempted: the pre-existing row plus one.
        assert_eq!(second.inserted, 1, "{tag}: 失败前的行会保留");
        assert_eq!(
            after, 2,
            "{tag}: 应剩下导入前的 1 行 + 失败前写入的 1 行, 实际 {after}"
        );
    }

    drop_namespace(pool, dialect, ns).await;
}

#[tokio::test]
async fn live_mysql_excel_import() {
    let Some(target) = env_target("DATASYNC_MYSQL", Dialect::Mysql) else {
        eprintln!("跳过：未设置 DATASYNC_MYSQL_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Mysql).await;
    excel_import_scenario(&pool, Dialect::Mysql, "datasync_live_import", "mysql 导入").await;
}

#[tokio::test]
async fn live_postgres_excel_import() {
    let Some(target) = env_target("DATASYNC_PG", Dialect::Postgres) else {
        eprintln!("跳过：未设置 DATASYNC_PG_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Postgres).await;
    excel_import_scenario(&pool, Dialect::Postgres, "datasync_live_import", "pg 导入").await;
}

/// Text of one column across every row of a page.
fn column_texts(data: &crate::commands::TableData, column: &str) -> Vec<Option<String>> {
    let index = data
        .columns
        .iter()
        .position(|entry| entry.name == column)
        .unwrap_or_else(|| panic!("结果中没有列 {column}"));
    data.rows
        .iter()
        .map(|row| row.get(index).and_then(|cell| cell.text.clone()))
        .collect()
}

fn filter(column: &str, operator: FilterOp, value: Option<&str>) -> FilterCondition {
    FilterCondition {
        column: column.into(),
        operator,
        value: value.map(str::to_string),
    }
}

fn sort_by(column: &str, descending: bool) -> SortSpec {
    SortSpec {
        column: column.into(),
        descending,
    }
}

/// Search conditions, column sorting and inline cell editing against a real
/// server — the three things the data grid does beyond scrolling.
async fn grid_scenario(pool: &Pool, dialect: Dialect, ns: &str, tag: &str) {
    drop_namespace(pool, dialect, ns).await;
    create_namespace(pool, dialect, ns).await;

    let q = |name: &str| dialect.qualified(Some(ns), name);
    let c = |name: &str| dialect.quote(name);
    let engine = match dialect {
        Dialect::Mysql => " ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        Dialect::Postgres => "",
    };
    let text_type = match dialect {
        Dialect::Mysql => "VARCHAR(64)",
        Dialect::Postgres => "character varying(64)",
    };
    let int_type = match dialect {
        Dialect::Mysql => "INT",
        Dialect::Postgres => "integer",
    };

    exec_all(
        pool,
        &[
            format!(
                "CREATE TABLE {} (
                    {id} {int_type} NOT NULL,
                    {name} {text_type},
                    {score} {int_type},
                    {note} {text_type},
                    {code} {text_type} NOT NULL,
                    PRIMARY KEY ({id})
                 ){engine}",
                q("grid"),
                id = c("id"),
                name = c("name"),
                score = c("score"),
                note = c("note"),
                code = c("code"),
            ),
            format!(
                "INSERT INTO {} ({id}, {name}, {score}, {note}, {code}) VALUES
                    (1, 'alice', 10, NULL, 'A'),
                    (2, 'bob', 20, 'x', 'B'),
                    (3, '100% cotton', 30, NULL, 'C'),
                    (4, 'Carol', 20, 'y', 'D'),
                    (5, 'dave', 10, 'z', 'E')",
                q("grid"),
                id = c("id"),
                name = c("name"),
                score = c("score"),
                note = c("note"),
                code = c("code"),
            ),
        ],
    )
    .await;

    let page = |filters: Vec<FilterCondition>, sort: Vec<SortSpec>, limit: u64, offset: u64| {
        let ns = ns.to_string();
        async move {
            preview_table_data_core(pool, &ns, "grid", Some(limit), Some(offset), &filters, &sort)
                .await
                .expect("grid query")
        }
    };

    // ---- search conditions -------------------------------------------------
    let all = page(Vec::new(), Vec::new(), 50, 0).await;
    assert_eq!(all.rows.len(), 5, "{tag}: 应读到 5 行");
    assert_eq!(all.total, Some(5), "{tag}: 未筛选时总数应为 5");

    // A literal `%` must not act as a wildcard.
    let literal = page(vec![filter("name", FilterOp::Contains, Some("100%"))], Vec::new(), 50, 0).await;
    assert_eq!(
        column_texts(&literal, "id"),
        vec![Some("3".into())],
        "{tag}: LIKE 通配符必须转义，否则会匹配到所有行"
    );
    assert_eq!(literal.total, Some(1), "{tag}: 计数应反映筛选结果");

    let wildcard = page(vec![filter("name", FilterOp::Contains, Some("%"))], Vec::new(), 50, 0).await;
    assert_eq!(
        column_texts(&wildcard, "id"),
        vec![Some("3".into())],
        "{tag}: 搜索 % 应只匹配真的含有 % 的行"
    );

    let equals = page(vec![filter("score", FilterOp::Eq, Some("20"))], Vec::new(), 50, 0).await;
    assert_eq!(
        column_texts(&equals, "id"),
        vec![Some("2".into()), Some("4".into())],
        "{tag}: 数值条件不应被当成字符串"
    );

    let nulls = page(vec![filter("note", FilterOp::IsNull, None)], Vec::new(), 50, 0).await;
    assert_eq!(
        column_texts(&nulls, "id"),
        vec![Some("1".into()), Some("3".into())],
        "{tag}: 为 NULL 条件"
    );

    let listed = page(vec![filter("id", FilterOp::In, Some("1, 3,5"))], Vec::new(), 50, 0).await;
    assert_eq!(
        column_texts(&listed, "id"),
        vec![Some("1".into()), Some("3".into()), Some("5".into())],
        "{tag}: IN 条件"
    );

    let combined = page(
        vec![
            filter("score", FilterOp::Ge, Some("20")),
            filter("note", FilterOp::NotNull, None),
        ],
        Vec::new(),
        50,
        0,
    )
    .await;
    assert_eq!(
        column_texts(&combined, "id"),
        vec![Some("2".into()), Some("4".into())],
        "{tag}: 多个条件之间是 AND"
    );

    // An invalid value is refused before it reaches the server.
    let error = preview_table_data_core(
        pool,
        ns,
        "grid",
        Some(10),
        Some(0),
        &[filter("score", FilterOp::Eq, Some("abc"))],
        &[],
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("不是整数"), "{tag}: {error}");

    // ---- sorting ----------------------------------------------------------
    let ascending = page(Vec::new(), vec![sort_by("score", false)], 50, 0).await;
    assert_eq!(
        column_texts(&ascending, "score"),
        vec![
            Some("10".into()),
            Some("10".into()),
            Some("20".into()),
            Some("20".into()),
            Some("30".into())
        ],
        "{tag}: 升序"
    );
    let descending = page(Vec::new(), vec![sort_by("score", true)], 50, 0).await;
    assert_eq!(
        column_texts(&descending, "score"),
        vec![
            Some("30".into()),
            Some("20".into()),
            Some("20".into()),
            Some("10".into()),
            Some("10".into())
        ],
        "{tag}: 降序"
    );

    // Paging over a column with ties must not repeat or skip rows; without the
    // primary-key tiebreaker the order within equal scores is undefined.
    let mut seen: Vec<String> = Vec::new();
    for offset in [0u64, 2, 4] {
        let page_data = page(Vec::new(), vec![sort_by("score", false)], 2, offset).await;
        seen.extend(
            column_texts(&page_data, "id")
                .into_iter()
                .flatten(),
        );
    }
    assert_eq!(
        seen,
        vec!["1", "5", "2", "4", "3"],
        "{tag}: 分页在排序值相同时必须稳定"
    );

    // ---- inline editing ---------------------------------------------------
    let edit = |column: &str, value: Option<&str>, previous: Option<&str>| CellEdit {
        column: column.into(),
        value: value.map(str::to_string),
        previous: previous.map(str::to_string),
    };
    let key = |value: &str| {
        vec![RowKeyValue {
            column: "id".into(),
            value: Some(value.to_string()),
        }]
    };
    let structure = crate::schema::introspect::load_table(pool, ns, "grid")
        .await
        .expect("load grid");

    let run = |key: Vec<RowKeyValue>, edit: CellEdit| {
        let sql = build_cell_update(dialect, ns, "grid", &structure, &key, &edit).expect("plan");
        let sqls = vec![sql.clone()];
        async move {
            let outcome = pool
                .run_statements(&sqls, true, &ProgressReporter::inert())
                .await
                .expect("run update");
            let first = outcome.outcomes.first().expect("an outcome");
            assert!(first.ok, "修改失败: {:?}", first.error);
            (first.rows_affected, sql)
        }
    };

    // A plain edit updates exactly one row.
    let (affected, sql) = run(key("1"), edit("note", Some("hello"), None)).await;
    assert_eq!(affected, 1, "{tag}: 应更新 1 行");
    assert!(sql.contains("IS NOT DISTINCT FROM") || sql.contains("<=>"), "{sql}");
    let after = page(vec![filter("id", FilterOp::Eq, Some("1"))], Vec::new(), 1, 0).await;
    assert_eq!(column_texts(&after, "note"), vec![Some("hello".into())]);

    // A stale "previous" value means someone else changed the row first; the
    // statement must match nothing rather than overwrite.
    let (affected, _) = run(key("1"), edit("note", Some("clobbered"), Some("not-the-current-value"))).await;
    assert_eq!(affected, 0, "{tag}: 旧值不匹配时不得写入");
    let after = page(vec![filter("id", FilterOp::Eq, Some("1"))], Vec::new(), 1, 0).await;
    assert_eq!(
        column_texts(&after, "note"),
        vec![Some("hello".into())],
        "{tag}: 冲突时原值应保持不变"
    );

    // Clearing a value writes NULL, and the guard still matches that NULL on a
    // second attempt (a plain `= NULL` would never match).
    let (affected, _) = run(key("1"), edit("note", None, Some("hello"))).await;
    assert_eq!(affected, 1, "{tag}: 清空应写入 NULL");
    let after = page(vec![filter("id", FilterOp::Eq, Some("1"))], Vec::new(), 1, 0).await;
    assert_eq!(column_texts(&after, "note"), vec![None], "{tag}: 应为 NULL");
    let (affected, _) = run(key("1"), edit("note", Some("back"), None)).await;
    assert_eq!(
        affected, 1,
        "{tag}: 从 NULL 改回值时必须能匹配到那一行"
    );

    // A NOT NULL column cannot be cleared, and the message says so.
    let error = build_cell_update(
        dialect,
        ns,
        "grid",
        &structure,
        &key("1"),
        &edit("code", None, Some("A")),
    )
    .unwrap_err();
    assert!(error.to_string().contains("不允许为空"), "{tag}: {error}");

    // A typed value that does not fit the column is refused with its name.
    let error = build_cell_update(
        dialect,
        ns,
        "grid",
        &structure,
        &key("1"),
        &edit("score", Some("abc"), Some("10")),
    )
    .unwrap_err();
    assert!(error.to_string().contains("不是整数"), "{tag}: {error}");

    drop_namespace(pool, dialect, ns).await;
}

#[tokio::test]
async fn live_mysql_data_grid() {
    let Some(target) = env_target("DATASYNC_MYSQL", Dialect::Mysql) else {
        eprintln!("跳过：未设置 DATASYNC_MYSQL_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Mysql).await;
    grid_scenario(&pool, Dialect::Mysql, "datasync_live_grid", "mysql 网格").await;
}

#[tokio::test]
async fn live_postgres_data_grid() {
    let Some(target) = env_target("DATASYNC_PG", Dialect::Postgres) else {
        eprintln!("跳过：未设置 DATASYNC_PG_* 环境变量");
        return;
    };
    let pool = connect(&target, Dialect::Postgres).await;
    grid_scenario(&pool, Dialect::Postgres, "datasync_live_grid", "pg 网格").await;
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
        "datasync_live_probe_src",
        &pg,
        "datasync_live_probe_dst",
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
        namespace: "datasync_live_probe_src".into(),
        table: "t".into(),
    };
    let target = TableEndpoint {
        connection_id: "dst".into(),
        namespace: "datasync_live_probe_dst".into(),
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
