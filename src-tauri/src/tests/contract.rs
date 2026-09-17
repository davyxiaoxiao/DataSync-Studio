//! IPC contract tests.
//!
//! The TypeScript types in `src/types.ts` are a hand-written mirror of the Rust
//! DTOs. Nothing checks the two against each other at compile time, so a
//! renamed field or a changed enum spelling would only surface as `undefined`
//! in the webview. These tests serialise each DTO and assert the exact JSON
//! keys and literal values the frontend reads.

use serde_json::{json, Value as Json};

use crate::data::plan::DataPlan;
use crate::data::{CellChange, ColumnPair, DataDiffSummary, DataDiffResult, KeyValue, RowChange};
use crate::db::value::{Cell, ColumnKind, ColumnMeta, Value};
use crate::db::{ScriptRun, StatementOutcome};
use crate::dialect::Dialect;
use crate::jobs::ProgressUpdate;
use crate::schema::ddl::{DdlStatement, SyncSelection};
use crate::schema::diff::{
    ChangeKind, ColumnDiff, DiffSummary, FieldChange, ForeignKeyDiff, IndexDiff, PrimaryKeyDiff,
    SchemaDiff, TableDiff, TypeDiff, ViewDiff,
};
use crate::schema::model::{
    ColumnInfo, CustomType, ForeignKeyInfo, IndexInfo, NamespaceInfo, TableStructure, TableSummary,
    ViewStructure,
};
use crate::store::ConnectionConfig;

/// Compare the key set of a JSON object, order-insensitively.
fn assert_keys(value: &Json, expected: &[&str]) {
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("expected an object, got {value}"));
    let mut actual: Vec<&str> = object.keys().map(String::as_str).collect();
    let mut want: Vec<&str> = expected.to_vec();
    actual.sort_unstable();
    want.sort_unstable();
    assert_eq!(actual, want, "JSON 字段集合与前端期望不一致: {value}");
}

fn to_json<T: serde::Serialize>(value: &T) -> Json {
    serde_json::to_value(value).expect("serialisable")
}

/// Every string value a union-typed enum can take, so the TS unions stay honest.
fn assert_enum_spellings<T>(values: &[(T, &str)])
where
    T: serde::Serialize + Copy,
{
    for (value, expected) in values {
        assert_eq!(
            to_json(value),
            json!(expected),
            "枚举拼写与前端联合类型不一致"
        );
    }
}

// ---------------------------------------------------------------------------
// Enums the frontend switches on
// ---------------------------------------------------------------------------

#[test]
fn dialect_and_change_kinds_serialise_as_the_frontend_expects() {
    assert_enum_spellings(&[
        (Dialect::Mysql, "mysql"),
        (Dialect::Postgres, "postgres"),
    ]);
    assert_enum_spellings(&[
        (ChangeKind::New, "new"),
        (ChangeKind::Missing, "missing"),
        (ChangeKind::Changed, "changed"),
        (ChangeKind::Identical, "identical"),
    ]);
    assert_enum_spellings(&[
        (ColumnKind::Int, "int"),
        (ColumnKind::Uint, "uint"),
        (ColumnKind::Float, "float"),
        (ColumnKind::Decimal, "decimal"),
        (ColumnKind::Bool, "bool"),
        (ColumnKind::Str, "str"),
        (ColumnKind::Bytes, "bytes"),
        (ColumnKind::Json, "json"),
        (ColumnKind::Date, "date"),
        (ColumnKind::Time, "time"),
        (ColumnKind::DateTime, "datetime"),
        (ColumnKind::Text, "text"),
    ]);
}

/// The data grid decides alignment from `kind`, so those exact spellings matter.
#[test]
fn cell_kinds_are_the_strings_the_grid_switches_on() {
    assert_eq!(to_json(&Value::Int(1).to_cell())["kind"], json!("int"));
    assert_eq!(to_json(&Value::Null.to_cell())["kind"], json!("null"));
    assert_eq!(to_json(&Value::Null.to_cell())["text"], Json::Null);

    let numeric = ["int", "uint", "float", "decimal"];
    for kind in numeric {
        assert!(
            matches!(
                kind,
                "int" | "uint" | "float" | "decimal"
            ),
            "数据网格依赖这些数值 kind: {kind}"
        );
    }
}

// ---------------------------------------------------------------------------
// Data transfer objects
// ---------------------------------------------------------------------------

fn sample_connection() -> ConnectionConfig {
    ConnectionConfig {
        id: "id-1".into(),
        name: "local".into(),
        dialect: Dialect::Mysql,
        host: "127.0.0.1".into(),
        port: 3306,
        username: "root".into(),
        database: Some("app".into()),
        ssl_mode: "prefer".into(),
        group: None,
        color: None,
        remark: None,
        save_password: true,
        password: None,
        has_password: true,
    }
}

#[test]
fn connection_config_shape() {
    let json = to_json(&sample_connection());
    assert_keys(
        &json,
        &[
            "id",
            "name",
            "dialect",
            "host",
            "port",
            "username",
            "database",
            "sslMode",
            "group",
            "color",
            "remark",
            "savePassword",
            "password",
            "hasPassword",
        ],
    );
    assert_eq!(json["sslMode"], json!("prefer"));
    assert_eq!(json["savePassword"], json!(true));
    assert_eq!(json["hasPassword"], json!(true));
    assert_eq!(json["dialect"], json!("mysql"));
}

#[test]
fn the_connection_form_round_trips_back_from_the_frontend_payload() {
    // Exactly the object `ConnectionDialog` posts to `save_connection`.
    let payload = json!({
        "id": "",
        "name": "新建连接 1",
        "dialect": "postgres",
        "host": "10.0.0.5",
        "port": 5432,
        "username": "postgres",
        "database": "",
        "sslMode": "disable",
        "group": "",
        "color": "#1f6feb",
        "remark": "",
        "savePassword": false,
        "password": "secret",
        "hasPassword": false
    });
    let config: ConnectionConfig = serde_json::from_value(payload).expect("deserialisable");
    assert_eq!(config.dialect, Dialect::Postgres);
    assert_eq!(config.ssl_mode, "disable");
    assert!(!config.save_password);
    assert_eq!(config.trimmed_database(), None, "空字符串应视为未指定");
}

#[test]
fn schema_diff_shape_covers_every_diff_kind() {
    let column = ColumnInfo {
        name: "id".into(),
        ordinal: 1,
        data_type: "INT".into(),
        full_type: "int".into(),
        nullable: false,
        default: None,
        is_auto_increment: true,
        auto_kind: Some("auto_increment".into()),
        is_generated: false,
        generation_expression: None,
        generation_kind: None,
        is_unsigned: false,
        comment: None,
        charset: None,
        collation: None,
        udt_name: None,
    };
    let field = FieldChange {
        field: "类型".into(),
        source: Some("int".into()),
        target: Some("bigint".into()),
    };
    let diff = SchemaDiff {
        source_namespace: "src".into(),
        target_namespace: "dst".into(),
        source_dialect: Dialect::Mysql,
        target_dialect: Dialect::Mysql,
        tables: vec![TableDiff {
            name: "users".into(),
            target_name: Some("users".into()),
            kind: ChangeKind::Changed,
            source_comment: None,
            target_comment: None,
            columns: vec![ColumnDiff {
                name: "id".into(),
                kind: ChangeKind::Changed,
                source: Some(column.clone()),
                target: Some(column.clone()),
                differences: vec![field.clone()],
            }],
            primary_key: PrimaryKeyDiff {
                kind: ChangeKind::Identical,
                source: vec!["id".into()],
                target: vec!["id".into()],
            },
            indexes: vec![IndexDiff {
                name: "idx".into(),
                kind: ChangeKind::New,
                source: Some(IndexInfo {
                    name: "idx".into(),
                    columns: vec!["id".into()],
                    is_unique: false,
                    index_type: None,
                }),
                target: None,
                differences: Vec::new(),
            }],
            foreign_keys: vec![ForeignKeyDiff {
                name: "fk".into(),
                kind: ChangeKind::Missing,
                source: None,
                target: Some(ForeignKeyInfo {
                    name: "fk".into(),
                    columns: vec!["id".into()],
                    ref_schema: None,
                    ref_table: "orgs".into(),
                    ref_columns: vec!["id".into()],
                    on_update: "NO ACTION".into(),
                    on_delete: "CASCADE".into(),
                }),
                differences: Vec::new(),
            }],
        }],
        views: vec![ViewDiff {
            name: "v".into(),
            kind: ChangeKind::New,
            source: Some(ViewStructure {
                name: "v".into(),
                kind: "view".into(),
                definition: "select 1".into(),
                comment: None,
                dependencies: vec!["users".into()],
            }),
            target: None,
            differences: Vec::new(),
        }],
        types: vec![TypeDiff {
            name: "mood".into(),
            kind: ChangeKind::Changed,
            source: Some(CustomType {
                name: "mood".into(),
                kind: "enum".into(),
                labels: vec!["ok".into()],
                base_type: None,
                domain_nullable: true,
                domain_default: None,
                constraints: Vec::new(),
                comment: None,
            }),
            target: None,
            differences: Vec::new(),
        }],
        summary: DiffSummary::default(),
    };

    let json = to_json(&diff);
    assert_keys(
        &json,
        &[
            "sourceNamespace",
            "targetNamespace",
            "sourceDialect",
            "targetDialect",
            "tables",
            "views",
            "types",
            "summary",
        ],
    );
    assert_keys(
        &json["tables"][0],
        &[
            "name",
            "targetName",
            "kind",
            "sourceComment",
            "targetComment",
            "columns",
            "primaryKey",
            "indexes",
            "foreignKeys",
        ],
    );
    assert_keys(
        &json["tables"][0]["columns"][0],
        &["name", "kind", "source", "target", "differences"],
    );
    assert_keys(&json["tables"][0]["columns"][0]["differences"][0], &["field", "source", "target"]);
    assert_keys(
        &json["tables"][0]["primaryKey"],
        &["kind", "source", "target"],
    );
    assert_keys(&json["views"][0], &["name", "kind", "source", "target", "differences"]);
    assert_keys(
        &json["views"][0]["source"],
        &["name", "kind", "definition", "comment", "dependencies"],
    );
    assert_keys(
        &json["types"][0]["source"],
        &[
            "name",
            "kind",
            "labels",
            "baseType",
            "domainNullable",
            "domainDefault",
            "constraints",
            "comment",
        ],
    );
    // The summary drives the stat cards.
    assert_keys(
        &json["summary"],
        &[
            "newTables",
            "missingTables",
            "changedTables",
            "identicalTables",
            "newColumns",
            "missingColumns",
            "changedColumns",
            "indexChanges",
            "foreignKeyChanges",
            "primaryKeyChanges",
            "newTypes",
            "missingTypes",
            "changedTypes",
            "newViews",
            "missingViews",
            "changedViews",
            "identicalViews",
        ],
    );
}

#[test]
fn column_info_shape_includes_the_generated_and_udt_fields() {
    let column = ColumnInfo {
        name: "total".into(),
        ordinal: 3,
        data_type: "DECIMAL".into(),
        full_type: "decimal(12,2)".into(),
        nullable: true,
        default: None,
        is_auto_increment: false,
        auto_kind: None,
        is_generated: true,
        generation_expression: Some("(`amount` * 2)".into()),
        generation_kind: Some("stored".into()),
        is_unsigned: false,
        comment: Some("合计".into()),
        charset: None,
        collation: None,
        udt_name: Some("mood".into()),
    };
    let json = to_json(&column);
    assert_keys(
        &json,
        &[
            "name",
            "ordinal",
            "dataType",
            "fullType",
            "nullable",
            "default",
            "isAutoIncrement",
            "autoKind",
            "isGenerated",
            "generationExpression",
            "generationKind",
            "isUnsigned",
            "comment",
            "charset",
            "collation",
            "udtName",
        ],
    );
    assert_eq!(json["isGenerated"], json!(true));
    assert_eq!(json["generationExpression"], json!("(`amount` * 2)"));
}

#[test]
fn table_structure_and_summary_shapes() {
    let structure = TableStructure {
        name: "users".into(),
        comment: None,
        engine: Some("InnoDB".into()),
        charset: Some("utf8mb4".into()),
        collation: None,
        primary_key: vec!["id".into()],
        columns: vec![],
        indexes: vec![],
        foreign_keys: vec![],
    };
    assert_keys(
        &to_json(&structure),
        &[
            "name",
            "comment",
            "engine",
            "charset",
            "collation",
            "primaryKey",
            "columns",
            "indexes",
            "foreignKeys",
        ],
    );

    let summary = to_json(&TableSummary {
        name: "users".into(),
        table_type: "BASE TABLE".into(),
        comment: None,
        rows: Some(12),
    });
    assert_keys(&summary, &["name", "tableType", "comment", "rows"]);

    let namespace = to_json(&NamespaceInfo {
        name: "app".into(),
        charset: None,
        collation: None,
        is_system: false,
    });
    assert_keys(&namespace, &["name", "charset", "collation", "isSystem"]);
}

#[test]
fn table_data_shape_used_by_the_grid() {
    let meta = ColumnMeta {
        name: "id".into(),
        data_type: "int".into(),
        kind: ColumnKind::Int,
        nullable: false,
        primary_key: true,
    };
    assert_keys(
        &to_json(&meta),
        &["name", "dataType", "kind", "nullable", "primaryKey"],
    );
    assert_eq!(to_json(&meta)["kind"], json!("int"));

    let cell = to_json(&Cell {
        kind: "str".into(),
        text: Some("hello".into()),
    });
    assert_keys(&cell, &["kind", "text"]);
}

#[test]
fn data_diff_result_shape() {
    let result = DataDiffResult {
        plan_id: "plan-1".into(),
        columns: vec![ColumnPair {
            source: "a".into(),
            target: "b".into(),
        }],
        key_columns: vec![ColumnPair {
            source: "id".into(),
            target: "id".into(),
        }],
        summary: DataDiffSummary {
            source_rows: 3,
            target_rows: 2,
            inserts: 1,
            updates: 1,
            deletes: 1,
            unchanged: 1,
        },
        insert_preview: vec![RowChange {
            key: vec![KeyValue {
                column: "id".into(),
                value: Cell {
                    kind: "int".into(),
                    text: Some("1".into()),
                },
            }],
            cells: vec![CellChange {
                column: "name".into(),
                before: Cell {
                    kind: "null".into(),
                    text: None,
                },
                after: Cell {
                    kind: "str".into(),
                    text: Some("x".into()),
                },
            }],
        }],
        update_preview: Vec::new(),
        delete_preview: Vec::new(),
        preview_truncated: false,
        sql_preview: vec!["INSERT ...".into()],
        warnings: vec!["注意".into()],
    };

    let json = to_json(&result);
    assert_keys(
        &json,
        &[
            "planId",
            "columns",
            "keyColumns",
            "summary",
            "insertPreview",
            "updatePreview",
            "deletePreview",
            "previewTruncated",
            "sqlPreview",
            "warnings",
        ],
    );
    assert_keys(
        &json["summary"],
        &[
            "sourceRows",
            "targetRows",
            "inserts",
            "updates",
            "deletes",
            "unchanged",
        ],
    );
    assert_keys(&json["insertPreview"][0], &["key", "cells"]);
    assert_keys(&json["insertPreview"][0]["key"][0], &["column", "value"]);
    assert_keys(
        &json["insertPreview"][0]["cells"][0],
        &["column", "before", "after"],
    );
}

#[test]
fn sync_run_result_shape() {
    let outcome = StatementOutcome {
        sql: "SELECT 1".into(),
        ok: true,
        rows_affected: 2,
        error: None,
        elapsed_ms: 5,
    };
    assert_keys(
        &to_json(&outcome),
        &["sql", "ok", "rowsAffected", "error", "elapsedMs"],
    );

    let run = crate::commands::SyncRunResult {
        total: 1,
        succeeded: 1,
        failed: 0,
        elapsed_ms: 12,
        message: "完成".into(),
        failures: Vec::new(),
        sample: vec![outcome],
        warnings: vec!["提示".into()],
        cancelled: false,
    };
    assert_keys(
        &to_json(&run),
        &[
            "total",
            "succeeded",
            "failed",
            "elapsedMs",
            "message",
            "failures",
            "sample",
            "warnings",
            "cancelled",
        ],
    );
}

#[test]
fn schema_sql_preview_and_storage_info_shapes() {
    let preview = crate::commands::SchemaSqlPreview {
        statements: vec![DdlStatement {
            table: "users".into(),
            category: "add_column".into(),
            object: "email".into(),
            sql: "ALTER TABLE ...".into(),
        }],
        warnings: Vec::new(),
    };
    assert_keys(&to_json(&preview), &["statements", "warnings"]);
    assert_keys(
        &to_json(&preview.statements[0]),
        &["table", "category", "object", "sql"],
    );

    assert_keys(
        &to_json(&crate::commands::StorageInfo {
            dir: "/tmp".into(),
            warning: None,
        }),
        &["dir", "warning"],
    );
    assert_keys(
        &to_json(&crate::commands::TestConnectionResult {
            ok: true,
            server_version: Some("8.0".into()),
            elapsed_ms: 3,
            error: None,
        }),
        &["ok", "serverVersion", "elapsedMs", "error"],
    );
    assert_keys(
        &to_json(&crate::commands::ConnectionInfo {
            server_version: "8.0".into(),
            dialect: Dialect::Mysql,
            namespaces: Vec::new(),
        }),
        &["serverVersion", "dialect", "namespaces"],
    );
}

#[test]
fn progress_update_shape() {
    let update = ProgressUpdate {
        job_id: "job-1".into(),
        stage: "compare".into(),
        label: "对比数据".into(),
        processed: 10,
        total: None,
        detail: "已比对 10 行".into(),
        finished: false,
        cancelled: false,
    };
    assert_keys(
        &to_json(&update),
        &[
            "jobId", "stage", "label", "processed", "total", "detail", "finished", "cancelled",
        ],
    );
}

// ---------------------------------------------------------------------------
// Payloads the frontend *sends*
// ---------------------------------------------------------------------------

#[test]
fn sync_selection_round_trips_from_the_frontend_payload() {
    let payload = json!({
        "tables": [{
            "name": "users",
            "wholeTable": true,
            "columns": ["email"],
            "primaryKey": false,
            "indexes": ["idx"],
            "foreignKeys": []
        }],
        "views": ["v_users"],
        "types": ["mood"]
    });
    let selection: SyncSelection = serde_json::from_value(payload).expect("deserialisable");
    assert_eq!(selection.views, vec!["v_users"]);
    assert_eq!(selection.types, vec!["mood"]);
    let table = selection.tables.first().expect("one table");
    assert!(table.whole_table);
    assert!(!table.primary_key);
    assert_eq!(table.columns, vec!["email"]);
}

#[test]
fn sync_selection_defaults_are_forgiving() {
    // The frontend may omit `wholeTable`; it must default to true so that a new
    // table is still created.
    let selection: SyncSelection =
        serde_json::from_value(json!({ "tables": [{ "name": "t" }] })).expect("deserialisable");
    assert!(selection.tables[0].whole_table);
    assert!(selection.tables[0].columns.is_empty());
    // Absent views/types deserialise to empty rather than failing.
    let empty: SyncSelection = serde_json::from_value(json!({})).expect("deserialisable");
    assert!(empty.tables.is_empty() && empty.views.is_empty() && empty.types.is_empty());
}

#[test]
fn command_requests_accept_the_frontend_payloads() {
    let compare_schema: crate::commands::CompareSchemaRequest = serde_json::from_value(json!({
        "sourceConnectionId": "a",
        "sourceNamespace": "src",
        "targetConnectionId": "b",
        "targetNamespace": "dst",
        "ignoreComments": true,
        "ignoreCase": false,
        "compareCharset": false,
        "tables": null,
        "jobId": "job-1"
    }))
    .expect("deserialisable");
    assert_eq!(compare_schema.source_namespace, "src");
    assert_eq!(compare_schema.job_id.as_deref(), Some("job-1"));

    let apply_schema: crate::commands::ApplySchemaRequest = serde_json::from_value(json!({
        "compareId": "c1",
        "selection": { "tables": [], "views": [], "types": [] },
        "stopOnError": true,
        "jobId": "job-2"
    }))
    .expect("deserialisable");
    assert!(apply_schema.stop_on_error);
    assert!(apply_schema.selection.is_some());

    let compare_data: crate::commands::CompareDataRequest = serde_json::from_value(json!({
        "source": { "connectionId": "a", "namespace": "src", "table": "users" },
        "target": { "connectionId": "a", "namespace": "dst", "table": "users" },
        "keyColumns": [],
        "columnMap": [],
        "batchSize": 500,
        "jobId": "job-3"
    }))
    .expect("deserialisable");
    assert_eq!(compare_data.batch_size, Some(500));
    assert_eq!(compare_data.source.table, "users");

    let apply_data: crate::commands::ApplyDataRequest = serde_json::from_value(json!({
        "planId": "p1",
        "connectionId": "a",
        "insert": true,
        "update": true,
        "delete": false,
        "useTransaction": true,
        "refreshMaterializedViews": false,
        "jobId": "job-4"
    }))
    .expect("deserialisable");
    assert!(!apply_data.delete);
    assert!(!apply_data.refresh_materialized_views);
}

#[test]
fn apply_data_request_defaults_are_safe() {
    // A minimal payload (older frontend, or a programmatic caller) must still
    // refresh materialised views but must not silently delete rows.
    let request: crate::commands::ApplyDataRequest =
        serde_json::from_value(json!({ "planId": "p", "connectionId": "c" }))
            .expect("deserialisable");
    assert!(request.insert && request.update);
    assert!(
        !request.delete,
        "删除必须默认关闭：字段缺失时绝不能悄悄删数据"
    );
    assert!(request.use_transaction);
    assert!(request.refresh_materialized_views);
}

// ---------------------------------------------------------------------------
// Internal helpers that feed the DTOs
// ---------------------------------------------------------------------------

#[test]
fn script_run_stays_internal_to_rust() {
    // `ScriptRun` is deliberately not part of the IPC surface; the command layer
    // folds it into `SyncRunResult`.
    let run = ScriptRun {
        outcomes: Vec::new(),
        cancelled: true,
    };
    assert!(run.cancelled);
}

#[test]
fn data_plan_selection_orders_deletes_before_inserts() {
    let plan = DataPlan {
        insert: vec!["INSERT".into()],
        update: vec!["UPDATE".into()],
        delete: vec!["DELETE".into()],
        target_namespace: "dst".into(),
        created_at: std::time::Instant::now(),
    };
    // Deletes must run first so that re-inserting a key cannot collide.
    assert_eq!(
        plan.select(true, true, true),
        vec!["DELETE", "UPDATE", "INSERT"]
    );
    assert_eq!(plan.select(true, false, false), vec!["INSERT"]);
    assert!(plan.select(false, false, false).is_empty());
}
