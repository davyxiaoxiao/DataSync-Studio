//! Unit tests for the pure planning logic (no database server needed) plus
//! live integration tests driven by environment variables.

pub mod contract;
pub mod live;

use std::collections::HashMap;

use crate::data::{
    build_delete_statements, build_insert_statements, build_update_statements, ColumnPair,
};
use crate::db::value::{ColumnKind, Value};
use crate::dialect::Dialect;
use crate::schema::ddl::{generate, SyncSelection, TableSelection};
use crate::schema::diff::{compare_everything, ChangeKind, CompareOptions};
use crate::schema::model::{
    ColumnInfo, CustomType, ForeignKeyInfo, IndexInfo, TableStructure, ViewStructure,
};

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
        udt_name: None,
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
        views: Vec::new(),
        types: Vec::new(),
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




// ---------------------------------------------------------------------------
// Diffing
// ---------------------------------------------------------------------------

#[test]
fn diff_reports_new_and_missing_tables() {
    let mut pk = col("id", "INT", "int");
    pk.nullable = false;
    let source = vec![table("only_in_source", vec![pk.clone()], &["id"])];
    let target = vec![table("only_in_target", vec![pk], &["id"])];

    let diff = compare_everything(&source, &target, &[], &[], &[], &[], Dialect::Mysql, Dialect::Mysql, "",
        "",
        CompareOptions::default());
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

    let diff = compare_everything(&source, &target, &[], &[], &[], &[], Dialect::Mysql, Dialect::Mysql, "",
        "",
        CompareOptions::default());
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
    let diff = compare_everything(&source, &target, &[], &[], &[], &[], Dialect::Mysql, Dialect::Mysql, "",
        "",
        CompareOptions::default());
    assert_eq!(diff.tables[0].kind, ChangeKind::Identical);
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
    let diff = compare_everything(&source, &[], &[], &[], &[], &[], Dialect::Mysql, Dialect::Mysql, "",
        "",
        CompareOptions::default());
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
    let diff = compare_everything(&source, &[], &[], &[], &[], &[], Dialect::Postgres, Dialect::Postgres, "",
        "",
        CompareOptions::default());
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
    let diff = compare_everything(&source, &target, &[], &[], &[], &[], Dialect::Mysql, Dialect::Mysql, "",
        "",
        CompareOptions::default());
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
    let diff = compare_everything(&source, &target, &[], &[], &[], &[], Dialect::Mysql, Dialect::Mysql, "",
        "",
        CompareOptions::default());

    let mut sources = HashMap::new();
    sources.insert("t".to_string(), source_structure);

    // Nothing selected -> empty plan.
    let empty = SyncSelection {
        tables: Vec::new(),
        views: Vec::new(),
        types: Vec::new(),
    };
    assert!(generate(&diff, &sources, Some(&empty), None, &mut Vec::new()).is_empty());

    // Only the `added` column explicitly.
    let selective = SyncSelection {
        views: Vec::new(),
        types: Vec::new(),
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
    let diff = compare_everything(&source, &target, &[], &[], &[], &[], Dialect::Mysql, Dialect::Mysql, "",
        "",
        CompareOptions::default());
    assert_eq!(diff.tables[0].kind, ChangeKind::Missing);

    // Default selection (everything on) drops it.
    let plan = generate(&diff, &HashMap::new(), None, None, &mut Vec::new());
    assert_eq!(plan.len(), 1);
    assert!(plan[0].sql.contains("DROP TABLE IF EXISTS `obsolete`"));

    // Explicitly deselected -> nothing happens.
    let none = SyncSelection {
        views: Vec::new(),
        types: Vec::new(),
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
fn key_ordering_matches_byte_order_for_text() {
    use crate::data::stream::compare_keys;
    use std::cmp::Ordering;

    // The databases are asked to order text byte-wise, so uppercase letters
    // must sort before lowercase ones regardless of any column collation.
    assert_eq!(
        compare_keys(&Value::Str("Apple".into()), &Value::Str("apple".into())),
        Ordering::Less
    );
    assert_eq!(
        compare_keys(&Value::Str("Banana".into()), &Value::Str("apple".into())),
        Ordering::Less
    );
    assert_eq!(
        compare_keys(&Value::Str("same".into()), &Value::Str("same".into())),
        Ordering::Equal
    );
}

#[test]
fn key_ordering_is_numeric_not_lexicographic() {
    use crate::data::stream::compare_key_tuples;
    use std::cmp::Ordering;

    assert_eq!(
        compare_key_tuples(&[Value::Int(9)], &[Value::Int(10)]),
        Ordering::Less
    );
    assert_eq!(
        compare_key_tuples(
            &[Value::Decimal("1.50".into())],
            &[Value::Decimal("1.5".into())]
        ),
        Ordering::Equal
    );
    // Composite keys compare left to right.
    assert_eq!(
        compare_key_tuples(&[Value::Int(1), Value::Int(2)], &[Value::Int(1), Value::Int(3)]),
        Ordering::Less
    );
    assert_eq!(
        compare_key_tuples(&[Value::Int(2), Value::Int(1)], &[Value::Int(1), Value::Int(9)]),
        Ordering::Greater
    );
}

#[test]
fn key_ordering_places_nulls_first() {
    use crate::data::stream::compare_keys;
    use std::cmp::Ordering;

    assert_eq!(
        compare_keys(&Value::Null, &Value::Int(1)),
        Ordering::Less
    );
    assert_eq!(
        compare_keys(&Value::Null, &Value::Null),
        Ordering::Equal
    );
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
    let diff = compare_everything(&source, &[], &[], &[], &[], &[], Dialect::Mysql, Dialect::Mysql, "",
        "",
        CompareOptions::default());
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
    let diff = compare_everything(&source, &[], &[], &[], &[], &[], Dialect::Postgres, Dialect::Postgres, "",
        "",
        CompareOptions::default());
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
    let diff = compare_everything(&source, &target, &[], &[], &[], &[], Dialect::Mysql, Dialect::Mysql, "",
        "",
        CompareOptions::default());
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
    // The server may re-render an expression; only the semantic text matters.
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
        "((`amount`  *  2))",
    ))];
    let diff = compare_everything(&source, &target, &[], &[], &[], &[], Dialect::Mysql, Dialect::Mysql, "",
        "",
        CompareOptions::default());
    assert_eq!(diff.tables[0].kind, ChangeKind::Identical, "{:#?}", diff.tables[0]);
}

// ---------------------------------------------------------------------------
// Job progress and cancellation
// ---------------------------------------------------------------------------

#[test]
fn inert_reporter_never_cancels() {
    let reporter = crate::jobs::ProgressReporter::inert();
    assert!(!reporter.is_cancelled());
    assert!(reporter.check_cancelled().is_ok());
    // Emitting without an AppHandle must be a harmless no-op.
    reporter.force("apply", "执行语句", 0, Some(3), "开始");
    reporter.tick("apply", "执行语句", 1, Some(3), "进行中");
    reporter.finish("done", "完成", "结束");
}

#[test]
fn cancelling_an_unknown_job_is_reported_as_not_running() {
    let registry = crate::jobs::JobRegistry::default();
    assert!(!registry.cancel("nope"));

    let flag = registry.begin("job-1");
    assert!(!flag.load(std::sync::atomic::Ordering::SeqCst));
    assert!(registry.cancel("job-1"));
    assert!(flag.load(std::sync::atomic::Ordering::SeqCst));

    registry.finish("job-1");
    assert!(!registry.cancel("job-1"), "已结束的任务不应再被取消");
}

#[test]
fn reporter_observes_the_cancellation_flag() {
    let registry = crate::jobs::JobRegistry::default();
    let flag = registry.begin("job-2");
    let reporter = crate::jobs::ProgressReporter::new("job-2".into(), None, flag);
    assert!(reporter.check_cancelled().is_ok());

    registry.cancel("job-2");
    assert!(reporter.is_cancelled());
    let error = reporter.check_cancelled().expect_err("取消后应返回错误");
    assert!(error.to_string().contains("已取消"), "{error}");
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

fn view(name: &str, definition: &str, dependencies: &[&str]) -> ViewStructure {
    ViewStructure {
        name: name.into(),
        kind: "view".into(),
        definition: definition.into(),
        comment: None,
        dependencies: dependencies.iter().map(|d| (*d).to_string()).collect(),
    }
}

fn base_table() -> TableStructure {
    table(
        "users",
        vec![col("id", "INT", "int"), col("email", "VARCHAR", "varchar(64)")],
        &["id"],
    )
}

fn plan_for(
    source_tables: Vec<TableStructure>,
    source_views: Vec<ViewStructure>,
    target_tables: Vec<TableStructure>,
    target_views: Vec<ViewStructure>,
    dialect: Dialect,
) -> Vec<crate::schema::ddl::DdlStatement> {
    let diff = compare_everything(
        &source_tables,
        &target_tables,
        &source_views,
        &target_views,
        &[],
        &[],
        dialect,
        dialect,
        "",
        "",
        CompareOptions::default(),
    );
    let sources: HashMap<String, TableStructure> =
        source_tables.into_iter().map(|t| (t.name.clone(), t)).collect();
    let selection = SyncSelection {
        tables: Vec::new(),
        views: Vec::new(),
        types: Vec::new(),
    };
    // An empty selection means "nothing"; use a permissive one instead.
    let permissive = SyncSelection {
        types: diff.types.iter().map(|entry| entry.name.clone()).collect(),
        tables: diff
            .tables
            .iter()
            .filter(|t| t.kind != ChangeKind::Identical)
            .map(|t| TableSelection {
                name: t.name.clone(),
                whole_table: true,
                columns: t.columns.iter().map(|c| c.name.clone()).collect(),
                primary_key: true,
                indexes: t.indexes.iter().map(|i| i.name.clone()).collect(),
                foreign_keys: t.foreign_keys.iter().map(|f| f.name.clone()).collect(),
            })
            .collect(),
        views: diff.views.iter().map(|v| v.name.clone()).collect(),
    };
    let _ = selection;
    generate(&diff, &sources, Some(&permissive), None, &mut Vec::new())
}

#[test]
fn view_diff_classifies_each_case() {
    let diff = compare_everything(
        &[base_table()],
        &[base_table()],
        &[
            view("v_new", "select 1", &[]),
            view("v_changed", "select id from users", &["users"]),
            view("v_same", "select 1", &[]),
        ],
        &[
            view("v_changed", "select email from users", &["users"]),
            view("v_same", "select 1", &[]),
            view("v_gone", "select 1", &[]),
        ],
        &[],
        &[],
        Dialect::Mysql,
        Dialect::Mysql,
        "",
        "",
        CompareOptions::default(),
    );
    assert_eq!(diff.summary.new_views, 1);
    assert_eq!(diff.summary.changed_views, 1);
    assert_eq!(diff.summary.missing_views, 1);
    assert_eq!(diff.summary.identical_views, 1);

    let by_name = |name: &str| diff.views.iter().find(|v| v.name == name).unwrap();
    assert_eq!(by_name("v_new").kind, ChangeKind::New);
    assert_eq!(by_name("v_changed").kind, ChangeKind::Changed);
    assert_eq!(by_name("v_same").kind, ChangeKind::Identical);
    assert_eq!(by_name("v_gone").kind, ChangeKind::Missing);
    assert_eq!(by_name("v_changed").differences[0].field, "定义");
}

#[test]
fn view_definition_comparison_ignores_formatting() {
    let diff = compare_everything(
        &[base_table()],
        &[base_table()],
        &[view("v", "SELECT  `id`  FROM `users`", &["users"])],
        &[view("v", "select id from users", &["users"])],
        &[],
        &[],
        Dialect::Mysql,
        Dialect::Mysql,
        "",
        "",
        CompareOptions::default(),
    );
    assert_eq!(diff.views[0].kind, ChangeKind::Identical);
}

#[test]
fn views_are_dropped_first_and_created_last() {
    let plan = plan_for(
        vec![base_table()],
        vec![view("v_users", "select id from users", &["users"])],
        vec![],
        vec![],
        Dialect::Mysql,
    );
    let categories: Vec<&str> = plan.iter().map(|s| s.category.as_str()).collect();
    assert_eq!(
        categories,
        vec!["create_table", "create_view"],
        "视图必须在建表之后创建"
    );
    assert!(plan[1].sql.contains("CREATE OR REPLACE VIEW `v_users` AS select id from users"));
}

#[test]
fn materialised_view_uses_the_postgres_form() {
    let mut matview = view("mv_users", "SELECT id FROM users", &["users"]);
    matview.kind = "materialized".into();
    let plan = plan_for(
        vec![base_table()],
        vec![matview],
        vec![],
        vec![],
        Dialect::Postgres,
    );
    let create = plan
        .iter()
        .find(|s| s.category == "create_view")
        .expect("应生成物化视图");
    assert!(
        create
            .sql
            .contains("CREATE MATERIALIZED VIEW \"mv_users\" AS SELECT id FROM users WITH DATA"),
        "{}",
        create.sql
    );
}

#[test]
fn a_table_change_rebuilds_dependent_views_in_dependency_order() {
    // `users.name` widens, so `v_users` (which reads it) must be rebuilt, and
    // `v_emails` (which reads `v_users`) must be rebuilt after it.
    let mut target_users = base_table();
    target_users.columns[1].full_type = "varchar(32)".into();

    let plan = plan_for(
        vec![base_table()],
        vec![
            view("v_users", "select id, name from users", &["users"]),
            view("v_emails", "select email from v_users", &["v_users"]),
        ],
        vec![target_users],
        vec![
            view("v_users", "select id, name from users", &["users"]),
            view("v_emails", "select email from v_users", &["v_users"]),
        ],
        Dialect::Postgres,
    );

    let categories: Vec<&str> = plan.iter().map(|s| s.category.as_str()).collect();
    assert_eq!(
        categories,
        vec!["drop_view", "drop_view", "modify_column", "create_view", "create_view"],
        "应先删视图（逆依赖序）再改表，最后按依赖序重建：{categories:?}"
    );
    // Reverse dependency order on the way out: the consumer goes first.
    assert!(plan[0].sql.contains("\"v_emails\""), "{}", plan[0].sql);
    assert!(plan[1].sql.contains("\"v_users\""), "{}", plan[1].sql);
    // Dependency order on the way back in.
    assert!(plan[3].sql.contains("\"v_users\""), "{}", plan[3].sql);
    assert!(plan[4].sql.contains("\"v_emails\""), "{}", plan[4].sql);
}

#[test]
fn a_deselected_dependent_view_is_still_rebuilt() {
    let mut target_users = base_table();
    target_users.columns[1].full_type = "varchar(32)".into();

    let diff = compare_everything(
        &[base_table()],
        &[target_users],
        &[view("v_users", "select name from users", &["users"])],
        &[view("v_users", "select name from users", &["users"])],
        &[],
        &[],
        Dialect::Postgres,
        Dialect::Postgres,
        "",
        "",
        CompareOptions::default(),
    );
    let mut warnings = Vec::new();
    let plan = generate(
        &diff,
        &HashMap::new(),
        // The table change is selected but the view is left unchecked.
        Some(&SyncSelection {
            tables: diff
                .tables
                .iter()
                .map(|t| TableSelection {
                    name: t.name.clone(),
                    whole_table: true,
                    columns: t.columns.iter().map(|c| c.name.clone()).collect(),
                    primary_key: true,
                    indexes: Vec::new(),
                    foreign_keys: Vec::new(),
                })
                .collect(),
            views: Vec::new(),
            types: Vec::new(),
        }),
        None,
        &mut warnings,
    );
    let categories: Vec<&str> = plan.iter().map(|s| s.category.as_str()).collect();
    assert!(
        categories.contains(&"drop_view") && categories.contains(&"create_view"),
        "依赖视图必须自动重建: {categories:?}"
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("自动加入重建列表"), "{warnings:?}");
}

#[test]
fn dropping_a_view_is_selected_out_by_default() {
    let diff = compare_everything(
        &[base_table()],
        &[base_table()],
        &[],
        &[view("v_gone", "select 1", &[])],
        &[],
        &[],
        Dialect::Mysql,
        Dialect::Mysql,
        "",
        "",
        CompareOptions::default(),
    );
    // `None` means "apply everything", so the drop is planned.
    let plan = generate(&diff, &HashMap::new(), None, None, &mut Vec::new());
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].category, "drop_view");
    assert!(plan[0].sql.contains("DROP VIEW IF EXISTS `v_gone`"));

    // An explicit selection that omits it applies nothing.
    let plan = generate(
        &diff,
        &HashMap::new(),
        Some(&SyncSelection {
            tables: Vec::new(),
            views: Vec::new(),
            types: Vec::new(),
        }),
        None,
        &mut Vec::new(),
    );
    assert!(plan.is_empty());
}

// ---------------------------------------------------------------------------
// User-defined types (PostgreSQL enum / domain)
// ---------------------------------------------------------------------------

fn enum_type(name: &str, labels: &[&str]) -> CustomType {
    CustomType {
        name: name.into(),
        kind: "enum".into(),
        labels: labels.iter().map(|l| (*l).to_string()).collect(),
        base_type: None,
        domain_nullable: true,
        domain_default: None,
        constraints: Vec::new(),
        comment: None,
    }
}

fn domain_type(name: &str, base: &str, constraints: &[&str]) -> CustomType {
    CustomType {
        name: name.into(),
        kind: "domain".into(),
        labels: Vec::new(),
        base_type: Some(base.into()),
        domain_nullable: true,
        domain_default: None,
        constraints: constraints.iter().map(|c| (*c).to_string()).collect(),
        comment: None,
    }
}

fn plan_types_only(
    source_types: Vec<CustomType>,
    target_types: Vec<CustomType>,
    dialect: Dialect,
    schema: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let diff = compare_everything(
        &[],
        &[],
        &[],
        &[],
        &source_types,
        &target_types,
        dialect,
        dialect,
        "",
        "",
        CompareOptions::default(),
    );
    let mut warnings = Vec::new();
    let plan = generate(&diff, &HashMap::new(), None, schema, &mut warnings);
    (
        plan.iter().map(|s| s.sql.clone()).collect(),
        warnings,
    )
}

#[test]
fn enum_is_created_with_its_labels_in_order() {
    let (sql, warnings) = plan_types_only(
        vec![enum_type("mood", &["sad", "ok", "happy"])],
        vec![],
        Dialect::Postgres,
        Some("app"),
    );
    assert_eq!(
        sql,
        vec!["CREATE TYPE \"app\".\"mood\" AS ENUM ('sad', 'ok', 'happy');"]
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn added_enum_labels_are_positioned_correctly() {
    let (sql, warnings) = plan_types_only(
        vec![enum_type("mood", &["sad", "meh", "ok", "happy"])],
        vec![enum_type("mood", &["sad", "ok", "happy"])],
        Dialect::Postgres,
        None,
    );
    assert_eq!(
        sql,
        vec!["ALTER TYPE \"mood\" ADD VALUE 'meh' AFTER 'sad';"]
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn removing_an_enum_label_is_refused_with_a_warning() {
    let (sql, warnings) = plan_types_only(
        vec![enum_type("mood", &["sad", "happy"])],
        vec![enum_type("mood", &["sad", "ok", "happy"])],
        Dialect::Postgres,
        None,
    );
    assert!(sql.is_empty(), "不应生成无法执行的 DDL: {sql:?}");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("不支持删除枚举值"), "{warnings:?}");
}

#[test]
fn domain_is_created_with_base_nullability_default_and_checks() {
    let mut domain = domain_type("positive_amount", "numeric(12,2)", &["CHECK ((VALUE > (0)::numeric))"]);
    domain.domain_nullable = false;
    domain.domain_default = Some("0".into());
    let (sql, warnings) = plan_types_only(vec![domain], vec![], Dialect::Postgres, None);
    assert_eq!(sql.len(), 1);
    let ddl = &sql[0];
    assert!(ddl.contains("CREATE DOMAIN \"positive_amount\" AS numeric(12,2) NOT NULL"));
    assert!(ddl.contains("DEFAULT 0"));
    assert!(ddl.contains("CHECK ((VALUE > (0)::numeric))"), "{ddl}");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn domain_nullability_and_default_changes_are_altered() {
    let mut source = domain_type("d", "integer", &[]);
    source.domain_nullable = false;
    source.domain_default = Some("7".into());
    let target = domain_type("d", "integer", &[]);
    let (sql, warnings) = plan_types_only(vec![source], vec![target], Dialect::Postgres, None);
    assert!(sql.contains(&"ALTER DOMAIN \"d\" SET NOT NULL;".to_string()), "{sql:?}");
    assert!(sql.contains(&"ALTER DOMAIN \"d\" SET DEFAULT 7;".to_string()), "{sql:?}");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn changing_a_domain_base_type_is_refused_with_a_warning() {
    let (sql, warnings) = plan_types_only(
        vec![domain_type("d", "bigint", &[])],
        vec![domain_type("d", "integer", &[])],
        Dialect::Postgres,
        None,
    );
    assert!(sql.is_empty(), "{sql:?}");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("基础类型"), "{warnings:?}");
}

#[test]
fn missing_types_are_dropped_after_the_tables_that_used_them() {
    let diff = compare_everything(
        &[],
        &[base_table()],
        &[],
        &[],
        &[],
        &[enum_type("mood", &["ok"])],
        Dialect::Postgres,
        Dialect::Postgres,
        "",
        "",
        CompareOptions::default(),
    );
    let plan = generate(&diff, &HashMap::new(), None, None, &mut Vec::new());
    let categories: Vec<&str> = plan.iter().map(|s| s.category.as_str()).collect();
    assert_eq!(
        categories.last(),
        Some(&"drop_type"),
        "类型必须在引用它的表之后才能删除: {categories:?}"
    );
}

#[test]
fn user_defined_column_types_are_schema_qualified() {
    let mut column = col("status", "MOOD", "mood");
    column.udt_name = Some("mood".into());
    let mut structure = table("tasks", vec![column], &[]);
    structure.name = "tasks".into();

    let diff = compare_everything(
        &[structure.clone()],
        &[],
        &[],
        &[],
        &[enum_type("mood", &["ok"])],
        &[],
        Dialect::Postgres,
        Dialect::Postgres,
        "",
        "",
        CompareOptions::default(),
    );
    let mut sources = HashMap::new();
    sources.insert("tasks".to_string(), structure);
    let plan = generate(&diff, &sources, None, Some("app"), &mut Vec::new());

    let create_type = plan
        .iter()
        .find(|s| s.category == "create_type")
        .expect("应先建类型");
    assert!(create_type.sql.contains("CREATE TYPE \"app\".\"mood\""), "{}", create_type.sql);

    let create_table = plan
        .iter()
        .find(|s| s.category == "create_table")
        .expect("应建表");
    assert!(
        create_table.sql.contains("\"status\" \"app\".\"mood\""),
        "自定义类型列必须限定 schema，否则 search_path 不含目标 schema 时会解析失败: {}",
        create_table.sql
    );
}

// ---------------------------------------------------------------------------
// Structure sync never writes row data
// ---------------------------------------------------------------------------

fn plan_add_column(
    source: TableStructure,
    target: TableStructure,
) -> (Vec<crate::schema::ddl::DdlStatement>, Vec<String>) {
    let diff = compare_everything(
        &[source.clone()],
        &[target],
        &[],
        &[],
        &[],
        &[],
        Dialect::Postgres,
        Dialect::Postgres,
        "src",
        "dst",
        CompareOptions::default(),
    );
    let mut sources = HashMap::new();
    sources.insert(source.name.clone(), source);
    let mut warnings = Vec::new();
    let plan = generate(&diff, &sources, None, Some("dst"), &mut warnings);
    (plan, warnings)
}

fn users_with_id() -> TableStructure {
    let mut id = col("id", "INT", "integer");
    id.nullable = false;
    table("users", vec![id], &["id"])
}

/// A `NOT NULL` column with no default cannot be added to a populated table on
/// PostgreSQL. Structure sync must not invent values for the existing rows — it
/// emits the statement as declared and lets the server reject it.
#[test]
fn adding_a_not_null_column_writes_no_data() {
    let base = users_with_id();
    let mut source = base.clone();
    let mut required = col("code", "VARCHAR", "character varying(16)");
    required.nullable = false;
    source.columns.push(required);

    let (plan, warnings) = plan_add_column(source, base);
    assert_eq!(
        plan.len(),
        1,
        "结构同步不得为回填数据生成任何额外语句: {:?}",
        plan.iter().map(|s| &s.sql).collect::<Vec<_>>()
    );
    assert!(plan[0].sql.contains("NOT NULL"), "{}", plan[0].sql);
    assert!(
        !plan[0].sql.contains("DEFAULT"),
        "不得注入占位默认值: {}",
        plan[0].sql
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("结构同步不会代为填充数据")),
        "应提示这是结构同步不负责的部分: {warnings:?}"
    );
}

#[test]
fn a_nullable_column_is_added_without_comment() {
    let base = users_with_id();
    let mut source = base.clone();
    source
        .columns
        .push(col("note", "VARCHAR", "character varying(16)"));
    let (plan, warnings) = plan_add_column(source, base);
    assert_eq!(plan.len(), 1);
    assert!(!plan[0].sql.contains("DEFAULT"), "{}", plan[0].sql);
    assert!(warnings.is_empty(), "可空列不应产生告警: {warnings:?}");
}

#[test]
fn a_not_null_column_with_its_own_default_is_unremarkable() {
    let base = users_with_id();
    let mut source = base.clone();
    let mut active = col("active", "BOOLEAN", "boolean");
    active.nullable = false;
    active.default = Some("true".into());
    source.columns.push(active);

    let (plan, warnings) = plan_add_column(source, base);
    assert_eq!(plan.len(), 1);
    assert!(plan[0].sql.contains("DEFAULT true"), "{}", plan[0].sql);
    assert!(
        warnings.is_empty(),
        "自带默认值就不需要提示: {warnings:?}"
    );
}
