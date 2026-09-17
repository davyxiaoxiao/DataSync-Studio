use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::dialect::Dialect;
use crate::schema::diff::{ChangeKind, SchemaDiff, TableDiff};
use crate::schema::model::{ColumnInfo, ForeignKeyInfo, IndexInfo, TableStructure, ViewStructure};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DdlStatement {
    pub table: String,
    pub category: String,
    pub object: String,
    pub sql: String,
}

/// Which table/object changes the user accepted in the diff tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableSelection {
    pub name: String,
    /// for `new` / `missing` tables: whether to create / drop the whole table
    #[serde(default = "default_true")]
    pub whole_table: bool,
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub primary_key: bool,
    #[serde(default)]
    pub indexes: Vec<String>,
    #[serde(default)]
    pub foreign_keys: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSelection {
    pub tables: Vec<TableSelection>,
    /// Names of the views/ materialised views to apply.
    #[serde(default)]
    pub views: Vec<String>,
}

impl SyncSelection {
    fn table(&self, name: &str) -> Option<&TableSelection> {
        self.tables.iter().find(|t| t.name == name)
    }
}

// ---------------------------------------------------------------------------
// Default value handling
// ---------------------------------------------------------------------------

fn is_time_keyword(upper: &str) -> bool {
    upper.starts_with("CURRENT_TIMESTAMP")
        || upper.starts_with("CURRENT_DATE")
        || upper.starts_with("CURRENT_TIME")
        || upper.starts_with("NOW()")
        || upper.starts_with("LOCALTIME")
        || upper.starts_with("LOCALTIMESTAMP")
}

fn is_numeric_family(column: &ColumnInfo) -> bool {
    matches!(
        column.data_type.to_ascii_uppercase().as_str(),
        "TINYINT"
            | "SMALLINT"
            | "MEDIUMINT"
            | "INT"
            | "INTEGER"
            | "BIGINT"
            | "DECIMAL"
            | "NUMERIC"
            | "FLOAT"
            | "DOUBLE"
            | "REAL"
            | "YEAR"
            | "BIT"
    )
}

fn is_boolean_family(column: &ColumnInfo) -> bool {
    matches!(
        column.data_type.to_ascii_uppercase().as_str(),
        "BOOLEAN" | "BOOL"
    )
}

/// Render the DEFAULT clause for a column, or `None` when there is no usable
/// default. Only one engine is ever involved, so each dialect is handled with
/// its own native representation.
fn render_default(column: &ColumnInfo, dialect: Dialect) -> Option<String> {
    let raw = column.default.as_ref()?.trim().to_string();
    if raw.is_empty() || raw.eq_ignore_ascii_case("NULL") {
        return None;
    }
    match dialect {
        // `pg_get_expr` already yields a complete, correctly typed expression
        // such as `'abc'::character varying`, `0`, `true` or `now()`.
        Dialect::Postgres => Some(raw),
        Dialect::Mysql => {
            let upper = raw.to_ascii_uppercase();
            if is_time_keyword(&upper) {
                return Some(raw);
            }
            let looks_like_expression =
                raw.starts_with('(') || upper.ends_with(')') || upper.starts_with("ARRAY[");
            if looks_like_expression {
                return Some(raw);
            }
            if is_boolean_family(column) {
                let truthy = matches!(upper.as_str(), "1" | "TRUE" | "B'1'");
                return Some(if truthy { "1" } else { "0" }.to_string());
            }
            if is_numeric_family(column) && raw.parse::<f64>().is_ok() {
                return Some(raw);
            }
            Some(Dialect::Mysql.string_literal(&raw))
        }
    }
}

// ---------------------------------------------------------------------------
// Column definitions
// ---------------------------------------------------------------------------

/// A rendered column definition plus anything the user should know about it.
struct ColumnDdl {
    sql: String,
    warning: Option<String>,
}

fn column_definition(column: &ColumnInfo, target: Dialect) -> ColumnDdl {
    let q = |name: &str| target.quote(name);
    // Both sides are the same engine, so the declared type is reused verbatim.
    let mut parts = vec![format!("{} {}", q(&column.name), column.full_type)];

    if column.is_generated {
        if let Some(expression) = column
            .generation_expression
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            // Same engine on both sides, so the expression is reproduced
            // verbatim. PostgreSQL only supports STORED generated columns.
            let kind = match target {
                Dialect::Postgres => "STORED",
                Dialect::Mysql => match column.generation_kind.as_deref() {
                    Some("virtual") => "VIRTUAL",
                    _ => "STORED",
                },
            };
            parts.push(format!("GENERATED ALWAYS AS ({expression}) {kind}"));
            return ColumnDdl {
                sql: parts.join(" "),
                warning: None,
            };
        }
        // No usable expression: fall back to an ordinary column so the table can
        // still be created, and warn rather than silently producing a column
        // whose values will never be computed.
        if !column.nullable {
            parts.push("NOT NULL".into());
        }
        if let Some(comment) = &column.comment {
            if target == Dialect::Mysql {
                parts.push(format!("COMMENT {}", Dialect::Mysql.string_literal(comment)));
            }
        }
        return ColumnDdl {
            sql: parts.join(" "),
            warning: Some(format!(
                "列 {}：服务端未返回生成表达式，已改为普通列，请手动补回",
                column.name
            )),
        };
    }

    if target == Dialect::Postgres && column.is_auto_increment {
        parts.push("NOT NULL".into());
        parts.push(match column.auto_kind.as_deref() {
            Some("identity_always") => "GENERATED ALWAYS AS IDENTITY".into(),
            _ => "GENERATED BY DEFAULT AS IDENTITY".into(),
        });
    } else {
        if !column.nullable {
            parts.push("NOT NULL".into());
        }
        if let Some(default) = render_default(column, target) {
            parts.push(format!("DEFAULT {default}"));
        }
        if column.is_auto_increment && target == Dialect::Mysql {
            parts.push("AUTO_INCREMENT".into());
        }
    }

    if target == Dialect::Mysql {
        if let Some(collation) = &column.collation {
            if should_emit_collation(column) {
                parts.push(format!("COLLATE {collation}"));
            }
        }
        if let Some(comment) = &column.comment {
            parts.push(format!("COMMENT {}", Dialect::Mysql.string_literal(comment)));
        }
    }
    ColumnDdl {
        sql: parts.join(" "),
        warning: None,
    }
}

fn should_emit_collation(column: &ColumnInfo) -> bool {
    matches!(
        column.kind(),
        crate::db::value::ColumnKind::Str | crate::db::value::ColumnKind::Text
    )
}

/// A foreign key that pointed at a table in the source namespace must point at
/// the equivalent table in the *target* namespace once synchronised; only
/// genuinely cross-namespace references keep their original schema.
fn remap_fk(fk: &ForeignKeyInfo, source_schema: &str, target_schema: Option<&str>) -> ForeignKeyInfo {
    match fk.ref_schema.as_deref() {
        Some(reference) if reference == source_schema => ForeignKeyInfo {
            ref_schema: target_schema.map(str::to_string),
            ..fk.clone()
        },
        _ => fk.clone(),
    }
}

fn index_ddl(index: &IndexInfo, table: &str, target: Dialect, schema: Option<&str>) -> String {
    let q = |name: &str| target.quote(name);
    let tref = target.qualified(schema, table);
    let cols = index
        .columns
        .iter()
        .map(|c| {
            if c.contains('(') {
                c.clone()
            } else {
                q(c)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let unique = if index.is_unique { "UNIQUE " } else { "" };
    match target {
        Dialect::Mysql => format!(
            "ALTER TABLE {tref} ADD {unique}INDEX {} ({cols})",
            q(&index.name)
        ),
        // PostgreSQL does not accept a schema-qualified *index* name in
        // CREATE INDEX; the index is created in the table's schema.
        Dialect::Postgres => format!("CREATE {unique}INDEX {} ON {tref} ({cols})", q(&index.name)),
    }
}

fn view_keyword(view: &ViewStructure, target: Dialect) -> &'static str {
    if view.is_materialized() && target == Dialect::Postgres {
        "MATERIALIZED VIEW"
    } else {
        "VIEW"
    }
}

/// Rewrite a view body so that references to the source namespace point at the
/// target namespace.
///
/// Both engines qualify the relations a view reads: PostgreSQL renders
/// `FROM ns.table` and MySQL renders ``from `ns`.`table` ``. Copying the body
/// verbatim would leave the target's view reading the *source* database, which
/// is both wrong and invisible until the two databases diverge.
fn remap_view_schema(definition: &str, source_schema: &str, target_schema: &str) -> String {
    if source_schema.is_empty() || source_schema == target_schema {
        return definition.to_string();
    }
    let mut out = definition.to_string();
    for quote in ['"', '`'] {
        let from = format!("{quote}{source_schema}{quote}.");
        let to = format!("{quote}{target_schema}{quote}.");
        out = out.replace(&from, &to);
    }
    replace_qualifier(&out, source_schema, target_schema)
}

/// Replace `schema.` when `schema` stands alone as an identifier, case
/// insensitively and without touching longer identifiers that merely contain it.
fn replace_qualifier(input: &str, from: &str, to: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let needle: Vec<char> = from.to_lowercase().chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        let fits = i + needle.len() < chars.len()
            && chars[i..i + needle.len()]
                .iter()
                .map(|c| c.to_ascii_lowercase())
                .eq(needle.iter().copied())
            && chars[i + needle.len()] == '.';
        let boundary = i == 0 || {
            let previous = chars[i - 1];
            !(previous.is_alphanumeric() || previous == '_' || previous == '"' || previous == '`')
        };
        if fits && boundary {
            out.push_str(to);
            out.push('.');
            i += needle.len() + 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn create_view_ddl(
    view: &ViewStructure,
    target: Dialect,
    schema: Option<&str>,
    source_schema: &str,
) -> String {
    let qualified = target.qualified(schema, &view.name);
    let body = remap_view_schema(
        view.definition.trim().trim_end_matches(';'),
        source_schema,
        schema.unwrap_or(""),
    );
    if view.is_materialized() && target == Dialect::Postgres {
        // PostgreSQL has no CREATE OR REPLACE for materialised views; the
        // planner always drops them first.
        format!("CREATE MATERIALIZED VIEW {qualified} AS {body} WITH DATA;")
    } else {
        format!("CREATE OR REPLACE VIEW {qualified} AS {body};")
    }
}

fn drop_view_ddl(view: &ViewStructure, target: Dialect, schema: Option<&str>) -> String {
    let keyword = view_keyword(view, target);
    format!(
        "DROP {keyword} IF EXISTS {};",
        target.qualified(schema, &view.name)
    )
}

/// PostgreSQL cannot replace a materialised view in place.
fn needs_drop_before_create(view: &ViewStructure, target: Dialect) -> bool {
    view.is_materialized() && target == Dialect::Postgres
}

fn drop_index_ddl(index: &IndexInfo, table: &str, target: Dialect, schema: Option<&str>) -> String {
    let q = |name: &str| target.quote(name);
    let tref = target.qualified(schema, table);
    match target {
        Dialect::Mysql => format!("ALTER TABLE {tref} DROP INDEX {}", q(&index.name)),
        Dialect::Postgres => {
            let qualified = match schema {
                Some(s) if !s.is_empty() => {
                    format!("{}.{}", target.quote(s), q(&index.name))
                }
                _ => q(&index.name),
            };
            format!("DROP INDEX IF EXISTS {qualified}")
        }
    }
}

fn fk_ddl(
    fk: &ForeignKeyInfo,
    table: &str,
    target: Dialect,
    schema: Option<&str>,
) -> String {
    let q = |name: &str| target.quote(name);
    let tref = target.qualified(schema, table);
    let cols = fk
        .columns
        .iter()
        .map(|c| q(c))
        .collect::<Vec<_>>()
        .join(", ");
    let ref_cols = fk
        .ref_columns
        .iter()
        .map(|c| q(c))
        .collect::<Vec<_>>()
        .join(", ");
    let ref_schema = fk.ref_schema.as_deref().or(schema);
    let ref_table = target.qualified(ref_schema, &fk.ref_table);
    let mut clause = format!(
        "ALTER TABLE {tref} ADD CONSTRAINT {} FOREIGN KEY ({cols}) REFERENCES {ref_table} ({ref_cols})",
        q(&fk.name)
    );
    if !fk.on_delete.is_empty() && fk.on_delete != "NO ACTION" {
        clause.push_str(&format!(" ON DELETE {}", fk.on_delete));
    }
    if !fk.on_update.is_empty() && fk.on_update != "NO ACTION" {
        clause.push_str(&format!(" ON UPDATE {}", fk.on_update));
    }
    clause
}

fn drop_fk_ddl(
    fk: &ForeignKeyInfo,
    table: &str,
    target: Dialect,
    schema: Option<&str>,
) -> String {
    let q = |name: &str| target.quote(name);
    let tref = target.qualified(schema, table);
    match target {
        Dialect::Mysql => format!("ALTER TABLE {tref} DROP FOREIGN KEY {}", q(&fk.name)),
        Dialect::Postgres => format!("ALTER TABLE {tref} DROP CONSTRAINT {}", q(&fk.name)),
    }
}

fn create_table_ddl(
    structure: &TableStructure,
    target: Dialect,
    schema: Option<&str>,
    source_schema: &str,
    warnings: &mut Vec<String>,
) -> Vec<DdlStatement> {
    let q = |name: &str| target.quote(name);
    let tref = target.qualified(schema, &structure.name);
    let mut body: Vec<String> = Vec::new();
    for column in &structure.columns {
        let definition = column_definition(column, target);
        if let Some(warning) = definition.warning {
            warnings.push(format!("表 {}：{warning}", structure.name));
        }
        body.push("  ".to_string() + &definition.sql);
    }
    if !structure.primary_key.is_empty() {
        let cols = structure
            .primary_key
            .iter()
            .map(|c| q(c))
            .collect::<Vec<_>>()
            .join(", ");
        match target {
            Dialect::Mysql => body.push(format!("  PRIMARY KEY ({cols})")),
            Dialect::Postgres => body.push(format!(
                "  CONSTRAINT {} PRIMARY KEY ({cols})",
                q(&format!("{}_pkey", structure.name))
            )),
        }
    }
    if target == Dialect::Mysql {
        for index in &structure.indexes {
            let cols = index
                .columns
                .iter()
                .map(|c| q(c))
                .collect::<Vec<_>>()
                .join(", ");
            let unique = if index.is_unique { "UNIQUE KEY" } else { "KEY" };
            body.push(format!("  {unique} {} ({cols})", q(&index.name)));
        }
    }
    let mut sql = format!("CREATE TABLE {tref} (\n{}\n)", body.join(",\n"));
    if target == Dialect::Mysql {
        let options = Dialect::Mysql.table_options(
            structure.engine.as_deref(),
            structure.charset.as_deref(),
            structure.collation.as_deref(),
        );
        if !options.is_empty() {
            sql.push(' ');
            sql.push_str(&options);
        }
        if let Some(comment) = &structure.comment {
            sql.push_str(&format!(
                " COMMENT={}",
                Dialect::Mysql.string_literal(comment)
            ));
        }
    }
    sql.push(';');

    let mut statements = vec![DdlStatement {
        table: structure.name.clone(),
        category: "create_table".into(),
        object: structure.name.clone(),
        sql,
    }];

    // MySQL inlines indexes (and comments) into CREATE TABLE. PostgreSQL needs
    // them as separate statements, and `COMMENT ON` is PostgreSQL-only syntax.
    if target == Dialect::Postgres {
        for index in &structure.indexes {
            statements.push(DdlStatement {
                table: structure.name.clone(),
                category: "create_index".into(),
                object: index.name.clone(),
                sql: format!("{};", index_ddl(index, &structure.name, target, schema)),
            });
        }
        if let Some(comment) = &structure.comment {
            statements.push(DdlStatement {
                table: structure.name.clone(),
                category: "table_comment".into(),
                object: structure.name.clone(),
                sql: format!(
                    "COMMENT ON TABLE {tref} IS {};",
                    Dialect::Postgres.string_literal(comment)
                ),
            });
        }
        for column in &structure.columns {
            if let Some(comment) = &column.comment {
                statements.push(DdlStatement {
                    table: structure.name.clone(),
                    category: "column_comment".into(),
                    object: column.name.clone(),
                    sql: format!(
                        "COMMENT ON COLUMN {tref}.{} IS {};",
                        q(&column.name),
                        Dialect::Postgres.string_literal(comment)
                    ),
                });
            }
        }
    }

    // Foreign keys are always emitted separately so the planner can run them
    // once every table exists; an inline FK would require the referenced table
    // to have been created already, and tables are created in name order.
    for raw_fk in &structure.foreign_keys {
        let fk = remap_fk(raw_fk, source_schema, schema);
        statements.push(DdlStatement {
            table: structure.name.clone(),
            category: "add_foreign_key".into(),
            object: fk.name.clone(),
            sql: format!("{};", fk_ddl(&fk, &structure.name, target, schema)),
        });
    }

    statements
}

/// Name of the column that precedes `name` in the source table order, used to
/// reproduce MySQL column positioning with `AFTER`.
fn previous_column(columns: &[&ColumnInfo], name: &str) -> Option<String> {
    let mut previous = None;
    for column in columns {
        if column.name == name {
            return previous;
        }
        previous = Some(column.name.clone());
    }
    None
}

fn alter_column_statements(
    table: &TableDiff,
    target: Dialect,
    schema: Option<&str>,
    warnings: &mut Vec<String>,
) -> Vec<DdlStatement> {
    let q = |name: &str| target.quote(name);
    let tref = target.qualified(schema, &table.name);
    let mut out = Vec::new();

    let source_columns: Vec<&ColumnInfo> = table.columns.iter().filter_map(|c| c.source.as_ref()).collect();
    // Columns that already exist on the target. `MODIFY ... AFTER` may only
    // reference these, because additions are applied after modifications.
    let existing_columns: Vec<&ColumnInfo> = table
        .columns
        .iter()
        .filter(|c| c.kind != ChangeKind::New)
        .filter_map(|c| c.source.as_ref())
        .collect();

    // Drops first (a name may be reused by an add), then in-place
    // modifications, then additions in source order so that an `AFTER`
    // predecessor is guaranteed to exist already.
    let ordered: Vec<&crate::schema::diff::ColumnDiff> = [ChangeKind::Missing, ChangeKind::Changed, ChangeKind::New]
        .iter()
        .flat_map(|wanted| table.columns.iter().filter(move |c| c.kind == *wanted))
        .collect();

    for diff in ordered {
        // A dropped column has no source definition; handled separately below.
        if diff.kind == ChangeKind::Missing {
            let name = diff
                .target
                .as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_else(|| diff.name.clone());
            out.push(DdlStatement {
                table: table.name.clone(),
                category: "drop_column".into(),
                object: name.clone(),
                sql: format!("ALTER TABLE {tref} DROP COLUMN {};", q(&name)),
            });
            continue;
        }

        let Some(column) = diff.source.as_ref() else {
            continue;
        };
        match diff.kind {
            ChangeKind::New => {
                let definition = column_definition(column, target);
                if let Some(warning) = definition.warning {
                    warnings.push(format!("表 {}：{warning}", table.name));
                }
                let mut sql = format!("ALTER TABLE {tref} ADD COLUMN {}", definition.sql);
                if target == Dialect::Mysql {
                    let position = previous_column(&source_columns, &column.name);
                    match position {
                        Some(prev) => sql.push_str(&format!(" AFTER {}", q(&prev))),
                        None => sql.push_str(" FIRST"),
                    }
                }
                sql.push(';');
                out.push(DdlStatement {
                    table: table.name.clone(),
                    category: "add_column".into(),
                    object: column.name.clone(),
                    sql,
                });
                if target == Dialect::Postgres {
                    if let Some(comment) = &column.comment {
                        out.push(DdlStatement {
                            table: table.name.clone(),
                            category: "column_comment".into(),
                            object: column.name.clone(),
                            sql: format!(
                                "COMMENT ON COLUMN {tref}.{} IS {};",
                                q(&column.name),
                                Dialect::Postgres.string_literal(comment)
                            ),
                        });
                    }
                }
            }
            ChangeKind::Changed => {
                if diff.differences.iter().any(|d| d.field == "生成列") {
                    warnings.push(format!(
                        "表 {}：列 {} 的「生成列」属性发生变化，两个引擎都不支持用 ALTER 增删生成属性，该语句可能被服务端拒绝，需要重建这一列",
                        table.name, column.name
                    ));
                }
                if target == Dialect::Mysql {
                    let definition = column_definition(column, target);
                    if let Some(warning) = definition.warning {
                        warnings.push(format!("表 {}：{warning}", table.name));
                    }
                    let mut sql = format!(
                        "ALTER TABLE {tref} MODIFY COLUMN {}",
                        definition.sql
                    );
                    let position = previous_column(&existing_columns, &column.name);
                    if let Some(prev) = position {
                        sql.push_str(&format!(" AFTER {}", q(&prev)));
                    }
                    sql.push(';');
                    out.push(DdlStatement {
                        table: table.name.clone(),
                        category: "modify_column".into(),
                        object: column.name.clone(),
                        sql,
                    });
                } else {
                    let target_column = diff.target.as_ref();
                    let type_changed = diff
                        .differences
                        .iter()
                        .any(|d| d.field == "类型");
                    let new_type = column.full_type.clone();
                    if type_changed {
                        out.push(DdlStatement {
                            table: table.name.clone(),
                            category: "modify_column".into(),
                            object: column.name.clone(),
                            sql: format!(
                                "ALTER TABLE {tref} ALTER COLUMN {} TYPE {new_type} USING {}::{};",
                                q(&column.name),
                                q(&column.name),
                                new_type
                            ),
                        });
                    }
                    let nullable_changed = target_column
                        .map(|t| t.nullable != column.nullable)
                        .unwrap_or(true);
                    if nullable_changed {
                        out.push(DdlStatement {
                            table: table.name.clone(),
                            category: "modify_column".into(),
                            object: column.name.clone(),
                            sql: format!(
                                "ALTER TABLE {tref} ALTER COLUMN {} {};",
                                q(&column.name),
                                if column.nullable {
                                    "DROP NOT NULL"
                                } else {
                                    "SET NOT NULL"
                                }
                            ),
                        });
                    }
                    let default_changed = diff.differences.iter().any(|d| d.field == "默认值");
                    if default_changed {
                        let clause = match render_default(column, target) {
                            Some(value) => format!("SET DEFAULT {value}"),
                            None => "DROP DEFAULT".to_string(),
                        };
                        out.push(DdlStatement {
                            table: table.name.clone(),
                            category: "modify_column".into(),
                            object: column.name.clone(),
                            sql: format!(
                                "ALTER TABLE {tref} ALTER COLUMN {} {clause};",
                                q(&column.name)
                            ),
                        });
                    }
                    let auto_changed = diff.differences.iter().any(|d| d.field == "自增");
                    if auto_changed {
                        let clause = if column.is_auto_increment {
                            match column.auto_kind.as_deref() {
                                Some("identity_always") => {
                                    "ADD GENERATED ALWAYS AS IDENTITY".to_string()
                                }
                                _ => "ADD GENERATED BY DEFAULT AS IDENTITY".to_string(),
                            }
                        } else {
                            "DROP IDENTITY IF EXISTS".to_string()
                        };
                        out.push(DdlStatement {
                            table: table.name.clone(),
                            category: "modify_column".into(),
                            object: column.name.clone(),
                            sql: format!(
                                "ALTER TABLE {tref} ALTER COLUMN {} {clause};",
                                q(&column.name)
                            ),
                        });
                    }
                    if diff.differences.iter().any(|d| d.field == "注释") {
                        let value = match &column.comment {
                            Some(c) => Dialect::Postgres.string_literal(c),
                            None => "NULL".to_string(),
                        };
                        out.push(DdlStatement {
                            table: table.name.clone(),
                            category: "column_comment".into(),
                            object: column.name.clone(),
                            sql: format!(
                                "COMMENT ON COLUMN {tref}.{} IS {value};",
                                q(&column.name)
                            ),
                        });
                    }
                }
            }
            ChangeKind::Missing | ChangeKind::Identical => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Statement planning
// ---------------------------------------------------------------------------

/// Build the full ordered DDL plan that turns the target into the source.
pub fn generate(
    diff: &SchemaDiff,
    sources: &HashMap<String, TableStructure>,
    selection: Option<&SyncSelection>,
    target_schema: Option<&str>,
    warnings: &mut Vec<String>,
) -> Vec<DdlStatement> {
    let target = diff.target_dialect;
    // `None` means "no explicit selection was supplied, apply every change".
    // `Some(empty)` means the user unchecked everything and must apply nothing.
    let is_selected = |table: &str| match selection {
        Some(s) => s.table(table).is_some(),
        None => true,
    };

    let (drop_views, create_views) = plan_views(
        diff,
        selection,
        target,
        target_schema,
        &diff.source_namespace,
        warnings,
    );

    let mut drop_fks: Vec<DdlStatement> = Vec::new();
    let mut drop_tables: Vec<DdlStatement> = Vec::new();
    let mut create_tables: Vec<DdlStatement> = Vec::new();
    let mut body: Vec<DdlStatement> = Vec::new();
    let mut add_indexes: Vec<DdlStatement> = Vec::new();
    let mut add_fks: Vec<DdlStatement> = Vec::new();

    for table in &diff.tables {
        if !table.has_changes() || !is_selected(&table.name) {
            continue;
        }
        let sel = selection.and_then(|s| s.table(&table.name));

        match table.kind {
            ChangeKind::New => {
                if sel.map(|s| s.whole_table).unwrap_or(true) {
                    if let Some(structure) = sources.get(&table.name) {
                        for statement in create_table_ddl(
                            structure,
                            target,
                            target_schema,
                            &diff.source_namespace,
                            warnings,
                        ) {
                            // Only foreign keys must wait until every table
                            // exists; indexes and comments keep their order.
                            if statement.category == "add_foreign_key" {
                                add_fks.push(statement);
                            } else {
                                create_tables.push(statement);
                            }
                        }
                    }
                }
            }
            ChangeKind::Missing => {
                if sel.map(|s| s.whole_table).unwrap_or(true) {
                    drop_tables.push(DdlStatement {
                        table: table.name.clone(),
                        category: "drop_table".into(),
                        object: table.name.clone(),
                        sql: format!(
                            "DROP TABLE IF EXISTS {};",
                            target.qualified(
                                target_schema,
                                table.target_name.as_deref().unwrap_or(&table.name)
                            )
                        ),
                    });
                }
            }
            ChangeKind::Changed | ChangeKind::Identical => {
                let wants = |name: &str, selected: Option<&Vec<String>>| match selected {
                    Some(list) => list.iter().any(|n| n == name),
                    None => true,
                };

                // 1. FKs that must go (dropped or changed) and the ones to add.
                for diff_fk in table.foreign_keys.iter() {
                    let name = &diff_fk.name;
                    if !wants(name, sel.map(|s| &s.foreign_keys)) {
                        continue;
                    }
                    match diff_fk.kind {
                        ChangeKind::Missing | ChangeKind::Changed => {
                            if let Some(fk) = diff_fk.target.as_ref() {
                                drop_fks.push(DdlStatement {
                                    table: table.name.clone(),
                                    category: "drop_foreign_key".into(),
                                    object: name.clone(),
                                    sql: format!(
                                        "{};",
                                        drop_fk_ddl(fk, &table.name, target, target_schema)
                                    ),
                                });
                            }
                        }
                        _ => {}
                    }
                    if diff_fk.kind == ChangeKind::New {
                        if let Some(fk) = diff_fk.source.as_ref() {
                            let fk = remap_fk(fk, &diff.source_namespace, target_schema);
                            add_fks.push(DdlStatement {
                                table: table.name.clone(),
                                category: "add_foreign_key".into(),
                                object: name.clone(),
                                sql: format!("{};", fk_ddl(&fk, &table.name, target, target_schema)),
                            });
                        }
                    }
                }
                // Changed FKs are re-created after the structural work.
                for diff_fk in table.foreign_keys.iter() {
                    if diff_fk.kind == ChangeKind::Changed {
                        if !wants(&diff_fk.name, sel.map(|s| &s.foreign_keys)) {
                            continue;
                        }
                        if let Some(fk) = diff_fk.source.as_ref() {
                            let fk = remap_fk(fk, &diff.source_namespace, target_schema);
                            add_fks.push(DdlStatement {
                                table: table.name.clone(),
                                category: "add_foreign_key".into(),
                                object: diff_fk.name.clone(),
                                sql: format!("{};", fk_ddl(&fk, &table.name, target, target_schema)),
                            });
                        }
                    }
                }

                // 2. Indexes to drop (before columns they depend on).
                for index_diff in table.indexes.iter() {
                    if !wants(&index_diff.name, sel.map(|s| &s.indexes)) {
                        continue;
                    }
                    if index_diff.kind == ChangeKind::Missing || index_diff.kind == ChangeKind::Changed
                    {
                        if let Some(index) = index_diff.target.as_ref() {
                            body.push(DdlStatement {
                                table: table.name.clone(),
                                category: "drop_index".into(),
                                object: index_diff.name.clone(),
                                sql: format!(
                                    "{};",
                                    drop_index_ddl(index, &table.name, target, target_schema)
                                ),
                            });
                        }
                    }
                }

                // 3. Primary key changes.
                let tref = target.qualified(target_schema, &table.name);

                if table.primary_key.kind != ChangeKind::Identical
                    && sel.map(|s| s.primary_key).unwrap_or(true)
                {
                    let q = |name: &str| target.quote(name);
                    if !table.primary_key.target.is_empty() {
                        match target {
                            Dialect::Mysql => body.push(DdlStatement {
                                table: table.name.clone(),
                                category: "drop_primary_key".into(),
                                object: "PRIMARY".into(),
                                sql: format!("ALTER TABLE {tref} DROP PRIMARY KEY;"),
                            }),
                            Dialect::Postgres => body.push(DdlStatement {
                                table: table.name.clone(),
                                category: "drop_primary_key".into(),
                                object: "PRIMARY".into(),
                                sql: format!(
                                    "ALTER TABLE {tref} DROP CONSTRAINT IF EXISTS {};",
                                    q(&format!("{}_pkey", table.name))
                                ),
                            }),
                        }
                    }
                    if !table.primary_key.source.is_empty() {
                        let cols = table
                            .primary_key
                            .source
                            .iter()
                            .map(|c| q(c))
                            .collect::<Vec<_>>()
                            .join(", ");
                        body.push(DdlStatement {
                            table: table.name.clone(),
                            category: "add_primary_key".into(),
                            object: "PRIMARY".into(),
                            sql: format!("ALTER TABLE {tref} ADD PRIMARY KEY ({cols});"),
                        });
                    }
                }

                // 4. Column changes.
                let column_selected = sel.map(|s| &s.columns);
                let filtered = TableDiff {
                    columns: table
                        .columns
                        .iter()
                        .filter(|c| {
                            c.kind == ChangeKind::Identical
                                || wants(&c.name, column_selected)
                        })
                        .cloned()
                        .collect(),
                    ..table.clone()
                };
                body.extend(alter_column_statements(
                    &filtered,
                    target,
                    target_schema,
                    warnings,
                ));

                // 5. Indexes to add.
                for index_diff in table.indexes.iter() {
                    if !wants(&index_diff.name, sel.map(|s| &s.indexes)) {
                        continue;
                    }
                    if index_diff.kind == ChangeKind::New || index_diff.kind == ChangeKind::Changed {
                        if let Some(index) = index_diff.source.as_ref() {
                            add_indexes.push(DdlStatement {
                                table: table.name.clone(),
                                category: "create_index".into(),
                                object: index_diff.name.clone(),
                                sql: format!(
                                    "{};",
                                    index_ddl(index, &table.name, target, target_schema)
                                ),
                            });
                        }
                    }
                }

                // 6. Table / column comments.
                if !diff_has_comment_change(table) {
                    // nothing
                } else if target == Dialect::Mysql {
                    let comment = table
                        .source_comment
                        .clone()
                        .map(|c| Dialect::Mysql.string_literal(&c))
                        .unwrap_or_else(|| "''".into());
                    body.push(DdlStatement {
                        table: table.name.clone(),
                        category: "table_comment".into(),
                        object: table.name.clone(),
                        sql: format!("ALTER TABLE {tref} COMMENT={comment};"),
                    });
                } else {
                    let value = table
                        .source_comment
                        .clone()
                        .map(|c| Dialect::Postgres.string_literal(&c))
                        .unwrap_or_else(|| "NULL".into());
                    body.push(DdlStatement {
                        table: table.name.clone(),
                        category: "table_comment".into(),
                        object: table.name.clone(),
                        sql: format!("COMMENT ON TABLE {tref} IS {value};"),
                    });
                }
            }
        }
    }

    let mut plan = Vec::new();
    // Views are dropped before any table work (PostgreSQL refuses to alter a
    // table a view depends on) and recreated once every table exists.
    plan.extend(drop_views);
    plan.extend(drop_fks);
    plan.extend(drop_tables);
    plan.extend(create_tables);
    plan.extend(body);
    plan.extend(add_indexes);
    plan.extend(add_fks);
    plan.extend(create_views);
    plan
}

/// Work out which views must be dropped and recreated, and in what order.
///
/// A view is rebuilt when it is added, changed, or when it reads from a table
/// (or another rebuilt view) that is itself changing — otherwise PostgreSQL
/// would reject the column change underneath it.
fn plan_views(
    diff: &SchemaDiff,
    selection: Option<&SyncSelection>,
    target: Dialect,
    target_schema: Option<&str>,
    source_schema: &str,
    warnings: &mut Vec<String>,
) -> (Vec<DdlStatement>, Vec<DdlStatement>) {
    if diff.views.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let selected: Option<std::collections::HashSet<&str>> = selection.map(|s| {
        s.views
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<&str>>()
    });
    let is_selected = |name: &str| match &selected {
        Some(set) => set.contains(name),
        None => true,
    };

    let mut changed_relations: std::collections::HashSet<String> = diff
        .tables
        .iter()
        .filter(|t| t.kind != ChangeKind::Identical)
        .map(|t| t.name.clone())
        .collect();

    // Transitive closure over the view dependency graph.
    let mut rebuild: std::collections::HashSet<String> = diff
        .views
        .iter()
        .filter(|v| v.kind == ChangeKind::New || v.kind == ChangeKind::Changed)
        .filter(|v| is_selected(&v.name))
        .map(|v| v.name.clone())
        .collect();

    loop {
        let mut added = false;
        for view in &diff.views {
            if view.kind == ChangeKind::Missing || rebuild.contains(&view.name) {
                continue;
            }
            let Some(source) = view.source.as_ref() else {
                continue;
            };
            // An unchanged view that a user explicitly deselected is still
            // rebuilt when it sits on top of a change, because leaving it in
            // place would make the change fail.
            if source
                .dependencies
                .iter()
                .any(|dependency| changed_relations.contains(dependency))
            {
                if !is_selected(&view.name) {
                    warnings.push(format!(
                        "视图 {} 依赖于本次变更的表，已自动加入重建列表",
                        view.name
                    ));
                }
                rebuild.insert(view.name.clone());
                changed_relations.insert(view.name.clone());
                added = true;
            }
        }
        if !added {
            break;
        }
    }

    // Topological order: dependencies first, cycles fall back to name order.
    let mut ordered: Vec<&crate::schema::diff::ViewDiff> = Vec::new();
    let mut placed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pending: Vec<&crate::schema::diff::ViewDiff> = diff
        .views
        .iter()
        .filter(|v| rebuild.contains(&v.name))
        .collect();
    pending.sort_by(|a, b| a.name.cmp(&b.name));
    while !pending.is_empty() {
        let mut progressed = false;
        let mut index = 0;
        while index < pending.len() {
            let view = pending[index];
            let ready = view
                .source
                .as_ref()
                .map(|source| {
                    source.dependencies.iter().all(|dependency| {
                        !rebuild.contains(dependency) || placed.contains(dependency)
                    })
                })
                .unwrap_or(true);
            if ready {
                placed.insert(view.name.clone());
                ordered.push(view);
                pending.remove(index);
                progressed = true;
            } else {
                index += 1;
            }
        }
        if !progressed {
            // Circular view definitions: emit the rest in name order.
            for view in pending.drain(..) {
                placed.insert(view.name.clone());
                ordered.push(view);
            }
        }
    }

    let mut drops: Vec<DdlStatement> = Vec::new();
    // Views that disappear entirely are dropped but never recreated.
    for view in diff.views.iter().filter(|v| v.kind == ChangeKind::Missing) {
        if !is_selected(&view.name) {
            continue;
        }
        if let Some(existing) = view.target.as_ref() {
            drops.push(DdlStatement {
                table: view.name.clone(),
                category: "drop_view".into(),
                object: view.name.clone(),
                sql: drop_view_ddl(existing, target, target_schema),
            });
        }
    }
    // Rebuilt views are dropped in reverse dependency order. A view that does
    // not exist on the target yet needs no drop: CREATE OR REPLACE creates it.
    for view in ordered.iter().rev() {
        if view.target.is_none() {
            continue;
        }
        if let Some(source) = view.source.as_ref() {
            drops.push(DdlStatement {
                table: view.name.clone(),
                category: "drop_view".into(),
                object: view.name.clone(),
                sql: drop_view_ddl(source, target, target_schema),
            });
        }
    }

    let mut creates: Vec<DdlStatement> = Vec::new();
    for view in ordered {
        if let Some(source) = view.source.as_ref() {
            creates.push(DdlStatement {
                table: view.name.clone(),
                category: "create_view".into(),
                object: view.name.clone(),
                sql: create_view_ddl(source, target, target_schema, source_schema),
            });
        }
    }

    // A materialised view cannot be replaced in place, so the drop is always
    // emitted above; nothing further is needed here.
    let _ = needs_drop_before_create;
    (drops, creates)
}

fn diff_has_comment_change(table: &TableDiff) -> bool {
    if table.kind != ChangeKind::Changed {
        return false;
    }
    let source = table.source_comment.as_deref().unwrap_or("");
    let target = table.target_comment.as_deref().unwrap_or("");
    source != target
}
