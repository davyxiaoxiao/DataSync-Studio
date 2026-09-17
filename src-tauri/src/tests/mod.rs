//! Unit tests for the pure planning logic (no database server needed) plus
//! live integration tests driven by environment variables.

pub mod live;

use std::collections::HashMap;

use crate::data::{
    build_delete_statements, build_insert_statements, build_update_statements, compare_rows,
    ColumnPair, CompareInput,
};
use crate::db::value::{ColumnKind, Value};
use crate::dialect::Dialect;
use crate::schema::ddl::{generate, map_type, SyncSelection, TableSelection};
use crate::schema::diff::{compare, ChangeKind, CompareOptions};
use crate::schema::model::{ColumnInfo, ForeignKeyInfo, IndexInfo, TableStructure};

fn col(name: &str, family: &str, full_type: &str) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        ordinal: 1,
        data_type: family.into(),
        full_type: full_type.into(),
        nullable: true,
        default: None,
        is_auto_increment: false,
        auto_kind: None,
        is_generated: false,
        generation_expression: None,
        generation_kind: None,
        is_unsigned: full_type.contains("unsigned"),
        comment: None,
        charset: None,
        collation: None,
    }
}

fn table(name: &str, columns: Vec<ColumnInfo>, pk: &[&str]) -> TableStructure {
    TableStructure {
        name: name.into(),
        comment: None,
        engine: Some("InnoDB".into()),
        charset: Some("utf8mb4".into()),
        collation: Some("utf8mb4_general_ci".into()),
        primary_key: pk.iter().map(|s| s.to_string()).collect(),
        columns,
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    }
}

fn selection_for(tables: &[&str]) -> SyncSelection {
    SyncSelection {
        tables: tables
            .iter()
            .map(|name| TableSelection {
                name: (*name).to_string(),
                whole_table: true,
                ..Default::default()
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Type mapping
// ---------------------------------------------------------------------------

#[test]
fn mysql_types_map_to_postgres() {
    let cases = [
        ("VARCHAR", "varchar(255)", "varchar(255)"),
        ("INT", "int(11)", "integer"),
        ("BIGINT", "bigint", "bigint"),
        ("BIGINT UNSIGNED", "bigint unsigned", "numeric(20)"),
        ("BOOLEAN", "tinyint(1)", "boolean"),
        ("DECIMAL", "decimal(10,2)", "numeric(10,2)"),
        ("DATETIME", "datetime", "timestamp"),
        ("TIMESTAMP", "timestamp", "timestamptz"),
        ("LONGTEXT", "longtext", "text"),
        ("LONGBLOB", "longblob", "bytea"),
        ("JSON", "json", "jsonb"),
        ("DOUBLE", "double", "double precision"),
    ];
    for (family, full, expected) in cases {
        let column = col("c", family, full);
        assert_eq!(
            map_type(Dialect::Mysql, Dialect::Postgres, &column),
            expected,
            "mapping {full} -> postgres"
        );
    }
}

#[test]
fn postgres_types_map_to_mysql() {
    let cases = [
        ("VARCHAR", "character varying(255)", "varchar(255)"),
        ("INT", "integer", "int"),
        ("INT", "int4", "int"),
        ("BOOL", "boolean", "tinyint(1)"),
        ("TIMESTAMPTZ", "timestamp with time zone", "datetime"),
        ("TIMESTAMP", "timestamp without time zone", "datetime"),
        ("BYTEA", "bytea", "longblob"),
        ("JSONB", "jsonb", "json"),
        ("UUID", "uuid", "char(36)"),
        ("DECIMAL", "numeric(10,2)", "decimal(10,2)"),
        ("CHAR", "character(3)", "char(3)"),
    ];
    for (family, full, expected) in cases {
        let column = col("c", family, full);
        assert_eq!(
            map_type(Dialect::Postgres, Dialect::Mysql, &column),
            expected,
            "mapping {full} -> mysql"
        );
    }
}

#[test]
fn same_dialect_keeps_declared_type() {
    let column = col("c", "VARCHAR", "varchar(64)");
    assert_eq!(
        map_type(Dialect::Mysql, Dialect::Mysql, &column),
        "varchar(64)"
    );
}

// ---------------------------------------------------------------------------
// Diffing
// ---------------------------------------------------------------------------

#[test]
fn diff_reports_new_and_missing_tables() {
    let mut pk = col("id", "INT", "int");
    pk.nullable = false;
    let source = vec![table("only_in_source", vec![pk.clone()], &["id"])];
    let target = vec![table("only_in_target", vec![pk], &["id"])];

    let diff = compare(
        &source,
        &target,
        Dialect::Mysql,
        Dialect::Mysql,
        CompareOptions::default(),
    );
    assert_eq!(diff.summary.new_tables, 1);
    assert_eq!(diff.summary.missing_tables, 1);

    let new_table = diff.tables.iter().find(|t| t.name == "only_in_source").unwrap();
    assert_eq!(new_table.kind, ChangeKind::New);
    let dropped = diff.tables.iter().find(|t| t.name == "only_in_target").unwrap();
    assert_eq!(dropped.kind, ChangeKind::Missing);
}

#[test]
fn diff_classifies_column_changes() {
    let source = vec![table(
        "t",
        vec![
            {
                let mut c = col("id", "INT", "int");
                c.nullable = false;
                c
            },
            col("added", "VARCHAR", "varchar(32)"),
            col("widened", "VARCHAR", "varchar(128)"),
            col("same", "INT", "int"),
        ],
        &["id"],
    )];
    let target = vec![table(
        "t",
        vec![
            {
                let mut c = col("id", "INT", "int");
                c.nullable = false;
                c
            },
            col("widened", "VARCHAR", "varchar(64)"),
            col("same", "INT", "int"),
            col("removed", "TEXT", "text"),
        ],
        &["id"],
    )];

    let diff = compare(
        &source,
        &target,
        Dialect::Mysql,
        Dialect::Mysql,
        CompareOptions::default(),
    );
    assert_eq!(diff.summary.changed_tables, 1);
    assert_eq!(diff.summary.new_columns, 1);
    assert_eq!(diff.summary.changed_columns, 1);
    assert_eq!(diff.summary.missing_columns, 1);

    let diff_table = &diff.tables[0];
    let by_name = |name: &str| diff_table.columns.iter().find(|c| c.name == name).unwrap();
    assert_eq!(by_name("added").kind, ChangeKind::New);
    assert_eq!(by_name("removed").kind, ChangeKind::Missing);
    assert_eq!(by_name("widened").kind, ChangeKind::Changed);
    assert_eq!(by_name("same").kind, ChangeKind::Identical);
    assert_eq!(by_name("widened").differences[0].field, "类型");
}

#[test]
fn int_display_width_is_not_a_difference() {
    let source = vec![table("t", vec![col("id", "INT", "int")], &[])];
    let target = vec![table("t", vec![col("id", "INT", "int(11)")], &[])];
    let diff = compare(
        &source,
        &target,
        Dialect::Mysql,
        Dialect::Mysql,
        CompareOptions::default(),
    );
    assert_eq!(diff.tables[0].kind, ChangeKind::Identical);
}

#[test]
fn cross_dialect_equivalent_types_are_not_differences() {
    let source = vec![table(
        "t",
        vec![
            col("id", "INT", "int(11)"),
            col("name", "VARCHAR", "varchar(64)"),
            col("created", "DATETIME", "datetime"),
        ],
        &[],
    )];
    let target = vec![table(
        "t",
        vec![
            col("id", "INT", "integer"),
            col("name", "VARCHAR", "character varying(64)"),
            col("created", "TIMESTAMP", "timestamp without time zone"),
        ],
        &[],
    )];
    let diff = compare(
        &source,
        &target,
        Dialect::Mysql,
        Dialect::Postgres,
        CompareOptions::default(),
    );
    assert_eq!(
        diff.tables[0].kind,
        ChangeKind::Identical,
        "columns: {:#?}",
        diff.tables[0]
            .columns
            .iter()
            .map(|c| (&c.name, c.kind, &c.differences))
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// DDL generation
// ---------------------------------------------------------------------------

#[test]
fn create_table_ddl_mysql_inlines_indexes_but_defers_foreign_keys() {
    let mut id = col("id", "INT", "int");
    id.nullable = false;
    id.is_auto_increment = true;
    id.auto_kind = Some("auto_increment".into());
    let mut structure = table(
        "users",
        vec![id, col("email", "VARCHAR", "varchar(128)")],
        &["id"],
    );
    structure.indexes.push(IndexInfo {
        name: "uk_email".into(),
        columns: vec!["email".into()],
        is_unique: true,
        index_type: Some("BTREE".into()),
    });
    structure.foreign_keys.push(ForeignKeyInfo {
        name: "fk_org".into(),
        columns: vec!["email".into()],
        ref_schema: None,
        ref_table: "orgs".into(),
        ref_columns: vec!["email".into()],
        on_update: "NO ACTION".into(),
        on_delete: "CASCADE".into(),
    });

    let source = vec![structure.clone()];
    let diff = compare(
        &source,
        &[],
        Dialect::Mysql,
        Dialect::Mysql,
        CompareOptions::default(),
    );
    let mut sources = HashMap::new();
    sources.insert("users".to_string(), structure);

    let plan = generate(&diff, &sources, Some(&selection_for(&["users"])), Some("app"), &mut Vec::new());
    // The table is created first and every foreign key is added afterwards, so
    // that the referenced table does not have to exist yet.
    let categories: Vec<&str> = plan.iter().map(|s| s.category.as_str()).collect();
    assert_eq!(categories, vec!["create_table", "add_foreign_key"]);

    let sql = &plan[0].sql;
    assert!(sql.starts_with("CREATE TABLE `app`.`users` ("), "{sql}");
    assert!(sql.contains("`id` int NOT NULL AUTO_INCREMENT"), "{sql}");
    assert!(sql.contains("PRIMARY KEY (`id`)"), "{sql}");
    assert!(sql.contains("UNIQUE KEY `uk_email` (`email`)"), "{sql}");
    assert!(!sql.contains("FOREIGN KEY"), "{sql}");
    assert!(sql.contains("ENGINE=InnoDB"), "{sql}");

    // The reference is schema-qualified too, so the statement does not depend on
    // a session default database.
    let fk = &plan[1].sql;
    assert!(
        fk.contains("ALTER TABLE `app`.`users` ADD CONSTRAINT `fk_org` FOREIGN KEY (`email`)")
            && fk.contains("REFERENCES `app`.`orgs` (`email`)"),
        "{fk}"
    );
    assert!(fk.contains("ON DELETE CASCADE"), "{fk}");
}

#[test]
fn create_table_ddl_postgres_splits_indexes_and_comments() {
    let mut id = col("id", "INT", "integer");
    id.nullable = false;
    id.comment = Some("主键".into());
    let mut structure = table(
        "users",
        vec![id, col("email", "VARCHAR", "character varying(128)")],
        &["id"],
    );
    structure.indexes.push(IndexInfo {
        name: "idx_email".into(),
        columns: vec!["email".into()],
        is_unique: false,
        index_type: Some("btree".into()),
    });

    let source = vec![structure.clone()];
    let diff = compare(
        &source,
        &[],
        Dialect::Postgres,
        Dialect::Postgres,
        CompareOptions::default(),
    );
    let mut sources = HashMap::new();
    sources.insert("users".to_string(), structure);

    let plan = generate(&diff, &sources, Some(&selection_for(&["users"])), Some("public"), &mut Vec::new());
    let categories: Vec<&str> = plan.iter().map(|s| s.category.as_str()).collect();
    assert_eq!(
        categories,
        vec!["create_table", "create_index", "column_comment"]
    );
    assert!(plan[0].sql.contains("CONSTRAINT \"users_pkey\" PRIMARY KEY (\"id\")"));
    // The index name itself must stay unqualified: PostgreSQL rejects
    // `CREATE INDEX schema.name`.
    assert!(
        plan[1]
            .sql
            .contains("CREATE INDEX \"idx_email\" ON \"public\".\"users\" (\"email\")"),
        "{}",
        plan[1].sql
    );
    assert!(
        plan[0].sql.starts_with("CREATE TABLE \"public\".\"users\" ("),
        "{}",
        plan[0].sql
    );
    assert!(
        plan[2]
            .sql
            .contains("COMMENT ON COLUMN \"public\".\"users\".\"id\" IS '主键'"),
        "{}",
        plan[2].sql
    );
}

#[test]
fn alter_plan_orders_drops_before_adds() {
    let source = vec![table(
        "t",
        vec![col("id", "INT", "int"), col("added", "VARCHAR", "varchar(32)")],
        &[],
    )];
    let target = vec![table(
        "t",
        vec![col("id", "INT", "int"), col("gone", "TEXT", "text")],
        &[],
    )];
    let diff = compare(
        &source,
        &target,
        Dialect::Mysql,
        Dialect::Mysql,
        CompareOptions::default(),
    );
    let plan = generate(&diff, &HashMap::new(), None, None, &mut Vec::new());
    let categories: Vec<&str> = plan.iter().map(|s| s.category.as_str()).collect();
    assert_eq!(categories, vec!["drop_column", "add_column"]);
    assert!(plan[0].sql.contains("DROP COLUMN `gone`"));
    // `added` follows `id` in the source order.
    assert!(plan[1].sql.contains("ADD COLUMN `added` varchar(32) AFTER `id`"), "{}", plan[1].sql);
}

#[test]
fn selection_filters_out_destructive_and_deselected_work() {
    let mut source_structure = table("t", vec![col("id", "INT", "int")], &[]);
    source_structure
        .columns
        .push(col("added", "VARCHAR", "varchar(32)"));
    let source = vec![source_structure.clone()];
    let target = vec![table("t", vec![col("id", "INT", "int")], &[])];
    let diff = compare(
        &source,
        &target,
        Dialect::Mysql,
        Dialect::Mysql,
        CompareOptions::default(),
    );

    let mut sources = HashMap::new();
    sources.insert("t".to_string(), source_structure);

    // Nothing selected -> empty plan.
    let empty = SyncSelection { tables: Vec::new() };
    assert!(generate(&diff, &sources, Some(&empty), None, &mut Vec::new()).is_empty());

    // Only the `added` column explicitly.
    let selective = SyncSelection {
        tables: vec![TableSelection {
            name: "t".into(),
            whole_table: false,
            columns: vec!["added".into()],
            primary_key: false,
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
        }],
    };
    let plan = generate(&diff, &sources, Some(&selective), None, &mut Vec::new());
    assert_eq!(plan.len(), 1);
    assert!(plan[0].sql.contains("ADD COLUMN `added`"));
}

#[test]
fn dropping_a_table_can_be_selected_out() {
    let source: Vec<TableStructure> = Vec::new();
    let target = vec![table("obsolete", vec![col("id", "INT", "int")], &[])];
    let diff = compare(
        &source,
        &target,
        Dialect::Mysql,
        Dialect::Mysql,
        CompareOptions::default(),
    );
    assert_eq!(diff.tables[0].kind, ChangeKind::Missing);

    // Default selection (everything on) drops it.
    let plan = generate(&diff, &HashMap::new(), None, None, &mut Vec::new());
    assert_eq!(plan.len(), 1);
    assert!(plan[0].sql.contains("DROP TABLE IF EXISTS `obsolete`"));

    // Explicitly deselected -> nothing happens.
    let none = SyncSelection {
        tables: vec![TableSelection {
            name: "obsolete".into(),
            whole_table: false,
            ..Default::default()
        }],
    };
    assert!(generate(&diff, &HashMap::new(), Some(&none), None, &mut Vec::new()).is_empty());
}

// ---------------------------------------------------------------------------
// DML generation and literals
// ---------------------------------------------------------------------------

#[test]
fn string_literals_escape_dialect_specific_characters() {
    assert_eq!(
        Dialect::Mysql.string_literal("O'Brien \\ x"),
        "'O''Brien \\\\ x'"
    );
    assert_eq!(Dialect::Postgres.string_literal("O'Brien \\ x"), "'O''Brien \\ x'");
}

fn simple_pairs(names: &[&str]) -> Vec<ColumnPair> {
    names
        .iter()
        .map(|name| ColumnPair {
            source: (*name).to_string(),
            target: (*name).to_string(),
        })
        .collect()
}

#[test]
fn insert_statement_batches_rows_and_casts_json() {
    let columns = vec!["id".to_string(), "name".to_string(), "meta".to_string()];
    let kinds = vec![ColumnKind::Int, ColumnKind::Str, ColumnKind::Json];
    let pairs = simple_pairs(&["id", "name", "meta"]);
    let rows = vec![
        vec![
            Value::Int(1),
            Value::Str("a'b".into()),
            Value::Json(serde_json::json!({"k": 1})),
        ],
        vec![Value::Int(2), Value::Null, Value::Null],
    ];

    let statements = build_insert_statements(
        Dialect::Mysql,
        "app",
        "t",
        &columns,
        &columns,
        &kinds,
        &pairs,
        &rows,
        100,
    );
    assert_eq!(statements.len(), 1);
    assert!(statements[0].starts_with("INSERT INTO `app`.`t` (`id`, `name`, `meta`) VALUES "));
    assert!(statements[0].contains("(1, 'a''b', CAST('{\"k\":1}' AS JSON))"), "{}", statements[0]);
    assert!(statements[0].contains("(2, NULL, NULL)"), "{}", statements[0]);

    let batched = build_insert_statements(
        Dialect::Postgres,
        "public",
        "t",
        &columns,
        &columns,
        &kinds,
        &pairs,
        &rows,
        1,
    );
    assert_eq!(batched.len(), 2);
    assert!(batched[0].starts_with("INSERT INTO \"public\".\"t\" "));
    assert!(batched[0].contains("'{\"k\":1}'::jsonb"), "{}", batched[0]);
}

#[test]
fn insert_statement_follows_the_column_mapping_not_the_position() {
    // Regression guard: the source row order and the target column order are
    // independent (a column may sit in the middle on one side and at the end on
    // the other). Values must be written against the target's own column list.
    let source_columns = vec![
        "id".to_string(),
        "nickname".to_string(),
        "age".to_string(),
    ];
    let target_columns = vec![
        "id".to_string(),
        "age".to_string(),
        "nickname".to_string(),
    ];
    let kinds = vec![ColumnKind::Int, ColumnKind::Int, ColumnKind::Str];
    let pairs = simple_pairs(&["id", "nickname", "age"]);
    let rows = vec![vec![
        Value::Int(1),
        Value::Str("Ali".into()),
        Value::Int(30),
    ]];

    let statements = build_insert_statements(
        Dialect::Postgres,
        "public",
        "users",
        &source_columns,
        &target_columns,
        &kinds,
        &pairs,
        &rows,
        100,
    );
    assert_eq!(statements.len(), 1);
    // `age` precedes `nickname` in the target, so the integer must come second.
    assert!(
        statements[0].contains("(\"id\", \"nickname\", \"age\") VALUES (1, 'Ali', 30)"),
        "{}",
        statements[0]
    );
}

#[test]
fn insert_statement_can_rename_columns() {
    let source_columns = vec!["user_id".to_string(), "full_name".to_string()];
    let target_columns = vec!["id".to_string(), "name".to_string()];
    let kinds = vec![ColumnKind::Int, ColumnKind::Str];
    let pairs = vec![
        ColumnPair {
            source: "full_name".into(),
            target: "name".into(),
        },
        ColumnPair {
            source: "user_id".into(),
            target: "id".into(),
        },
    ];
    let rows = vec![vec![Value::Int(7), Value::Str("Ada".into())]];

    let statements = build_insert_statements(
        Dialect::Mysql,
        "app",
        "people",
        &source_columns,
        &target_columns,
        &kinds,
        &pairs,
        &rows,
        100,
    );
    assert!(
        statements[0].contains("(`name`, `id`) VALUES ('Ada', 7)"),
        "{}",
        statements[0]
    );
}

#[test]
fn update_and_delete_statements_target_the_primary_key() {
    let columns = vec!["id".to_string(), "name".to_string()];
    let kinds = vec![ColumnKind::Int, ColumnKind::Str];
    let keys = vec![ColumnPair {
        source: "id".into(),
        target: "id".into(),
    }];

    let updates = build_update_statements(
        Dialect::Mysql,
        "app",
        "t",
        &columns,
        &kinds,
        &keys,
        &[(vec![Value::Int(7), Value::Str("old".into())], vec![(1, Value::Str("new".into()))])],
    );
    assert_eq!(updates[0], "UPDATE `app`.`t` SET `name` = 'new' WHERE `id` = 7;");

    let deletes = build_delete_statements(
        Dialect::Postgres,
        "public",
        "t",
        &columns,
        &kinds,
        &keys,
        &[vec![Value::Int(9), Value::Str("x".into())]],
    );
    assert_eq!(deletes[0], "DELETE FROM \"public\".\"t\" WHERE \"id\" = 9;");
}

#[test]
fn numeric_canonicalisation_matches_across_kinds() {
    assert_eq!(Value::Int(1).canonical(), Value::Decimal("1.0".into()).canonical());
    assert_eq!(Value::Float(1.0).canonical(), Value::Int(1).canonical());
    assert_ne!(Value::Int(1).canonical(), Value::Str("1".into()).canonical());
    assert_eq!(
        Value::Bool(true).canonical(),
        Value::Bool(true).canonical()
    );
    assert_ne!(Value::Null.canonical(), Value::Str(String::new()).canonical());
}

// ---------------------------------------------------------------------------
// Row level comparison
// ---------------------------------------------------------------------------

#[test]
fn compare_rows_finds_inserts_updates_and_deletes() {
    let columns = vec!["id".to_string(), "name".to_string()];
    let kinds = vec![ColumnKind::Int, ColumnKind::Str];
    let pairs = vec![
        ColumnPair {
            source: "id".into(),
            target: "id".into(),
        },
        ColumnPair {
            source: "name".into(),
            target: "name".into(),
        },
    ];
    let keys = vec![ColumnPair {
        source: "id".into(),
        target: "id".into(),
    }];

    let source_rows = vec![
        vec![Value::Int(1), Value::Str("updated".into())],
        vec![Value::Int(2), Value::Str("new".into())],
        vec![Value::Int(3), Value::Str("same".into())],
    ];
    let target_rows = vec![
        vec![Value::Int(1), Value::Str("original".into())],
        vec![Value::Int(3), Value::Str("same".into())],
        vec![Value::Int(4), Value::Str("orphan".into())],
    ];

    let outcome = compare_rows(CompareInput {
        source_rows,
        target_rows,
        source_columns: &columns,
        target_columns: &columns,
        column_pairs: &pairs,
        key_pairs: &keys,
        target_kinds: &kinds,
    })
    .unwrap();

    assert_eq!(outcome.inserts.len(), 1);
    assert_eq!(outcome.updates.len(), 1);
    assert_eq!(outcome.deletes.len(), 1);
    assert_eq!(outcome.unchanged, 1);
    assert_eq!(outcome.inserts[0][0], Value::Int(2));
    assert_eq!(outcome.updates[0].1[0].0, 1);
    assert_eq!(outcome.deletes[0][0], Value::Int(4));
}

// ---------------------------------------------------------------------------
// Statement splitting
// ---------------------------------------------------------------------------

#[test]
fn statement_splitter_respects_quotes_and_comments() {
    let script = "INSERT INTO t VALUES ('a;b'); -- trailing ; comment\n/* ; */ SELECT 2;\n\n";
    let statements = crate::commands::split_statements(script);
    assert_eq!(statements.len(), 2, "{statements:?}");
    assert!(statements[0].starts_with("INSERT INTO t VALUES ('a;b')"));
    assert!(statements[1].contains("SELECT 2"));
}

// ---------------------------------------------------------------------------
// Master key stability
// ---------------------------------------------------------------------------

#[test]
fn master_key_is_stable_across_store_reloads() {
    // Regression guard: regenerating the key on every start silently
    // invalidated every stored password.
    let dir = std::env::temp_dir().join(format!("datasync-key-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();

    let first = crate::store::Store::load(&dir);
    let saved = first
        .upsert(crate::store::ConnectionConfig {
            id: String::new(),
            name: "t".into(),
            dialect: Dialect::Mysql,
            host: "127.0.0.1".into(),
            port: 3306,
            username: "root".into(),
            database: None,
            ssl_mode: "disable".into(),
            group: None,
            color: None,
            remark: None,
            save_password: true,
            password: Some("s3cret".into()),
            has_password: false,
        })
        .unwrap();

    let second = crate::store::Store::load(&dir);
    let reloaded = second.get(&saved.id).expect("connection survives reload");
    assert_eq!(reloaded.password.as_deref(), Some("s3cret"));

    // A writable directory with a persistable key must not report degradation.
    assert_eq!(
        second.storage_warning(),
        None,
        "可写目录不应报告降级: {:?}",
        second.storage_warning()
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Generated (computed) columns
// ---------------------------------------------------------------------------

fn generated_col(name: &str, family: &str, full_type: &str, expression: &str) -> ColumnInfo {
    let mut column = col(name, family, full_type);
    column.is_generated = true;
    column.generation_expression = Some(expression.into());
    column.generation_kind = Some("stored".into());
    column
}

fn table_with_column(column: ColumnInfo) -> TableStructure {
    let mut id = col("id", "INT", "int");
    id.nullable = false;
    table("orders", vec![id, column], &["id"])
}

#[test]
fn generated_column_ddl_is_reproduced_per_dialect() {
    // MySQL: backticked identifiers, STORED keyword, no DEFAULT.
    let mysql_column = generated_col("total", "DECIMAL", "decimal(12,2)", "(`amount` * 2)");
    let structure = table_with_column(mysql_column);
    let source = vec![structure.clone()];
    let diff = compare(
        &source,
        &[],
        Dialect::Mysql,
        Dialect::Mysql,
        CompareOptions::default(),
    );
    let mut sources = HashMap::new();
    sources.insert("orders".to_string(), structure);
    let mut warnings = Vec::new();
    let plan = generate(&diff, &sources, None, None, &mut warnings);
    assert!(
        plan[0]
            .sql
            .contains("`total` decimal(12,2) GENERATED ALWAYS AS ((`amount` * 2)) STORED"),
        "{}",
        plan[0].sql
    );
    assert!(warnings.is_empty(), "同方言不应产生告警: {warnings:?}");

    // PostgreSQL: quoted identifiers, only STORED exists.
    let pg_column = generated_col("total", "DECIMAL", "numeric(12,2)", "(amount * 2)");
    let structure = table_with_column(pg_column);
    let source = vec![structure.clone()];
    let diff = compare(
        &source,
        &[],
        Dialect::Postgres,
        Dialect::Postgres,
        CompareOptions::default(),
    );
    let mut sources = HashMap::new();
    sources.insert("orders".to_string(), structure);
    let mut warnings = Vec::new();
    let plan = generate(&diff, &sources, None, None, &mut warnings);
    assert!(
        plan[0]
            .sql
            .contains("\"total\" numeric(12,2) GENERATED ALWAYS AS ((amount * 2)) STORED"),
        "{}",
        plan[0].sql
    );
    assert!(warnings.is_empty(), "同方言不应产生告警: {warnings:?}");
}

#[test]
fn generated_column_is_not_a_default() {
    // Regression guard: PostgreSQL exposes the generation expression through
    // pg_attrdef, which used to be read as a DEFAULT.
    let mut plain = col("total", "DECIMAL", "decimal(12,2)");
    plain.default = None;
    let generated = generated_col("total", "DECIMAL", "decimal(12,2)", "(`amount` * 2)");

    let source = vec![table_with_column(generated)];
    let target = vec![table_with_column(plain)];
    let diff = compare(
        &source,
        &target,
        Dialect::Mysql,
        Dialect::Mysql,
        CompareOptions::default(),
    );
    let fields: Vec<&str> = diff.tables[0].columns[1]
        .differences
        .iter()
        .map(|d| d.field.as_str())
        .collect();
    assert!(fields.contains(&"生成列"), "{fields:?}");
    assert!(!fields.contains(&"默认值"), "{fields:?}");
}

#[test]
fn same_generation_expression_with_different_formatting_is_identical() {
    let source = vec![table_with_column(generated_col(
        "total",
        "DECIMAL",
        "decimal(12,2)",
        "(`amount` * 2)",
    ))];
    let target = vec![table_with_column(generated_col(
        "total",
        "DECIMAL",
        "decimal(12,2)",
        "(amount * 2)",
    ))];
    let diff = compare(
        &source,
        &target,
        Dialect::Mysql,
        Dialect::Postgres,
        CompareOptions::default(),
    );
    assert_eq!(diff.tables[0].kind, ChangeKind::Identical, "{:#?}", diff.tables[0]);
}

#[test]
fn non_portable_generation_expression_falls_back_to_a_plain_column() {
    let column = generated_col("doc", "JSONB", "jsonb", "jsonb_build_object('a', \"id\")");
    let structure = table_with_column(column);
    let source = vec![structure.clone()];
    let diff = compare(
        &source,
        &[],
        Dialect::Postgres,
        Dialect::Mysql,
        CompareOptions::default(),
    );
    let mut sources = HashMap::new();
    sources.insert("orders".to_string(), structure);

    let mut warnings = Vec::new();
    let plan = generate(&diff, &sources, None, None, &mut warnings);
    assert!(
        !plan[0].sql.contains("GENERATED ALWAYS"),
        "不应生成无法移植的表达式: {}",
        plan[0].sql
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("改为普通列"), "{warnings:?}");
}

#[test]
fn simple_expression_ports_across_dialects_with_a_warning() {
    let column = generated_col("total", "DECIMAL", "decimal(12,2)", "(`amount` * 2)");
    let structure = table_with_column(column);
    let source = vec![structure.clone()];
    let diff = compare(
        &source,
        &[],
        Dialect::Mysql,
        Dialect::Postgres,
        CompareOptions::default(),
    );
    let mut sources = HashMap::new();
    sources.insert("orders".to_string(), structure);

    let mut warnings = Vec::new();
    let plan = generate(&diff, &sources, None, None, &mut warnings);
    assert!(
        plan[0]
            .sql
            .contains("GENERATED ALWAYS AS ((\"amount\" * 2)) STORED"),
        "{}",
        plan[0].sql
    );
    assert_eq!(warnings.len(), 1, "跨方言移植必须提示复核: {warnings:?}");
}
