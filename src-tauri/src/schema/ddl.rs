use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::dialect::Dialect;
use crate::error::{AppError, AppResult};
use crate::schema::diff::{ChangeKind, SchemaDiff, TableDiff, TypeDiff};
use crate::schema::model::{
    ColumnInfo, CustomType, ForeignKeyInfo, IndexInfo, TableStructure, ViewStructure,
};

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
    /// Every field is optional on the wire: a partial payload must not make the
    /// whole request fail to deserialise.
    #[serde(default)]
    pub tables: Vec<TableSelection>,
    /// Names of the views/ materialised views to apply.
    #[serde(default)]
    pub views: Vec<String>,
    /// Names of the user-defined types (PostgreSQL enum/domain) to apply.
    #[serde(default)]
    pub types: Vec<String>,
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
fn render_default(
    column: &ColumnInfo,
    dialect: Dialect,
    source_schema: &str,
    target_schema: Option<&str>,
) -> Option<String> {
    let raw = column.default.as_ref()?.trim().to_string();
    if raw.is_empty() || raw.eq_ignore_ascii_case("NULL") {
        return None;
    }
    match dialect {
        // `pg_get_expr` already yields a complete, correctly typed expression
        // such as `'abc'::character varying`, `0`, `true` or `now()`. Any
        // namespace it carries must be rewritten, otherwise a default such as
        // `'new'::src.order_state` would not match the target's own enum type.
        Dialect::Postgres => Some(remap_schema_references(
            &raw,
            source_schema,
            target_schema.unwrap_or(""),
        )),
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

fn column_definition(
    column: &ColumnInfo,
    target: Dialect,
    schema: Option<&str>,
    source_schema: &str,
) -> ColumnDdl {
    let q = |name: &str| target.quote(name);
    // Both sides are the same engine, so the declared type is reused verbatim —
    // except for user-defined types, which must be schema-qualified because the
    // target schema is not necessarily on the connection's search_path.
    let type_name = match (&column.udt_name, target) {
        (Some(udt), Dialect::Postgres) => target.qualified(schema, udt),
        _ => column.full_type.clone(),
    };
    let mut parts = vec![format!("{} {}", q(&column.name), type_name)];

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
        if let Some(default) = render_default(column, target, source_schema, schema) {
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

fn create_type_ddl(entry: &CustomType, target: Dialect, schema: Option<&str>) -> String {
    let qualified = target.qualified(schema, &entry.name);
    if entry.kind == "enum" {
        let labels: Vec<String> = entry
            .labels
            .iter()
            .map(|label| target.string_literal(label))
            .collect();
        format!("CREATE TYPE {qualified} AS ENUM ({});", labels.join(", "))
    } else {
        let base = entry.base_type.clone().unwrap_or_else(|| "text".into());
        let mut sql = format!("CREATE DOMAIN {qualified} AS {base}");
        if !entry.domain_nullable {
            sql.push_str(" NOT NULL");
        }
        if let Some(default) = entry.domain_default.as_deref().filter(|d| !d.is_empty()) {
            sql.push_str(&format!(" DEFAULT {default}"));
        }
        for (index, constraint) in entry.constraints.iter().enumerate() {
            // `pg_get_constraintdef` already returns e.g. `CHECK ((VALUE > 0))`.
            sql.push_str(&format!(
                " CONSTRAINT {} {constraint}",
                target.quote(&format!("{}_check_{}", entry.name, index + 1))
            ));
        }
        sql.push(';');
        return sql;
    }
}

fn drop_type_ddl(entry: &CustomType, target: Dialect, schema: Option<&str>) -> String {
    let keyword = if entry.kind == "enum" { "TYPE" } else { "DOMAIN" };
    format!(
        "DROP {keyword} IF EXISTS {};",
        target.qualified(schema, &entry.name)
    )
}

/// Statements that reconcile an existing enum/domain with its source.
fn alter_type_ddl(
    entry: &TypeDiff,
    target: Dialect,
    schema: Option<&str>,
    warnings: &mut Vec<String>,
) -> Vec<DdlStatement> {
    let (Some(source), Some(existing)) = (entry.source.as_ref(), entry.target.as_ref()) else {
        return Vec::new();
    };
    let qualified = target.qualified(schema, &entry.name);
    let mut out = Vec::new();

    if source.kind != existing.kind || source.kind == "domain" {
        if source.kind == "domain" {
            if source.base_type != existing.base_type {
                warnings.push(format!(
                    "域 {} 的基础类型由 {} 变为 {}，PostgreSQL 不支持修改域的基础类型，需要手工重建",
                    entry.name,
                    existing.base_type.clone().unwrap_or_default(),
                    source.base_type.clone().unwrap_or_default()
                ));
            }
            if source.domain_nullable != existing.domain_nullable {
                out.push(DdlStatement {
                    table: entry.name.clone(),
                    category: "alter_type".into(),
                    object: entry.name.clone(),
                    sql: format!(
                        "ALTER DOMAIN {qualified} {};",
                        if source.domain_nullable {
                            "DROP NOT NULL"
                        } else {
                            "SET NOT NULL"
                        }
                    ),
                });
            }
            if source.domain_default != existing.domain_default {
                let clause = match source.domain_default.as_deref().filter(|d| !d.is_empty()) {
                    Some(default) => format!("SET DEFAULT {default}"),
                    None => "DROP DEFAULT".to_string(),
                };
                out.push(DdlStatement {
                    table: entry.name.clone(),
                    category: "alter_type".into(),
                    object: entry.name.clone(),
                    sql: format!("ALTER DOMAIN {qualified} {clause};"),
                });
            }
            let added: Vec<&String> = source
                .constraints
                .iter()
                .filter(|c| !existing.constraints.contains(c))
                .collect();
            for (index, constraint) in added.iter().enumerate() {
                out.push(DdlStatement {
                    table: entry.name.clone(),
                    category: "alter_type".into(),
                    object: entry.name.clone(),
                    sql: format!(
                        "ALTER DOMAIN {qualified} ADD CONSTRAINT {} {constraint};",
                        target.quote(&format!("{}_added_{}", entry.name, index + 1))
                    ),
                });
            }
            if existing
                .constraints
                .iter()
                .any(|c| !source.constraints.contains(c))
            {
                warnings.push(format!(
                    "域 {} 存在被删除的 CHECK 约束，PostgreSQL 需要显式的约束名，请手工删除",
                    entry.name
                ));
            }
        }
        return out;
    }

    // Enum: PostgreSQL can only add labels, optionally positioned.
    let removed: Vec<&String> = existing
        .labels
        .iter()
        .filter(|label| !source.labels.contains(label))
        .collect();
    let added: Vec<&String> = source
        .labels
        .iter()
        .filter(|label| !existing.labels.contains(label))
        .collect();
    if !removed.is_empty() {
        warnings.push(format!(
            "枚举 {} 存在被删除的值（{}），PostgreSQL 不支持删除枚举值，需要手工重建类型及其依赖列",
            entry.name,
            removed
                .iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        return out;
    }
    for label in added {
        let position = source.labels.iter().position(|l| l == label);
        let anchor = position
            .and_then(|index| index.checked_sub(1))
            .and_then(|previous| source.labels.get(previous))
            .map(|previous| format!(" AFTER {}", target.string_literal(previous)))
            .unwrap_or_default();
        out.push(DdlStatement {
            table: entry.name.clone(),
            category: "alter_type".into(),
            object: entry.name.clone(),
            sql: format!(
                "ALTER TYPE {qualified} ADD VALUE {}{anchor};",
                target.string_literal(label)
            ),
        });
    }
    out
}

fn view_keyword(view: &ViewStructure, target: Dialect) -> &'static str {
    if view.is_materialized() && target == Dialect::Postgres {
        "MATERIALIZED VIEW"
    } else {
        "VIEW"
    }
}

/// Rewrite references to the source namespace so they point at the target.
///
/// Everything a server hands back is qualified with the namespace it came from:
/// PostgreSQL renders a view as `FROM ns.table` and a default as
/// `'x'::ns.myenum`, MySQL renders ``from `ns`.`table` ``. Copying that text
/// verbatim would leave the target reading the *source* database, which is both
/// wrong and invisible until the two databases diverge.
fn remap_schema_references(definition: &str, source_schema: &str, target_schema: &str) -> String {
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
    let body = remap_schema_references(
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
        let definition = column_definition(column, target, schema, source_schema);
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
    source_schema: &str,
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
                // A NOT NULL column with no default cannot be added to a table
                // that already holds rows on PostgreSQL. Structure sync does not
                // invent values for existing rows — filling data is the data
                // sync's job — so this is reported as advice and the statement is
                // left to succeed or fail on the server's own terms.
                if !column.nullable
                    && render_default(column, target, source_schema, schema).is_none()
                {
                    warnings.push(format!(
                        "表 {}：列 {} 为非空且无默认值。若目标表已有数据，PostgreSQL 会拒绝这条 ADD COLUMN \
                         （结构同步不会代为填充数据）；可先同步为可空列，用数据同步写入真实值后再同步一次加上 NOT NULL",
                        table.name, column.name
                    ));
                }

                let definition = column_definition(column, target, schema, source_schema);
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
                    let definition = column_definition(column, target, schema, source_schema);
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
                        let clause = match render_default(column, target, source_schema, schema) {
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

    let (create_types, alter_types, drop_types) =
        plan_types(diff, selection, target, target_schema, warnings);

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
    // (table, constraint) pairs already scheduled for removal, and the tables
    // that are going away entirely.
    let mut planned_fk_drops: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    let mut dropped_tables: std::collections::HashSet<String> = std::collections::HashSet::new();
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
                    // Release the table's own constraints first: MySQL refuses to
                    // drop a table that participates in a foreign key, including
                    // one of its own.
                    for fk in &table.foreign_keys {
                        if let Some(existing) = fk.target.as_ref() {
                            planned_fk_drops.insert((table.name.clone(), fk.name.clone()));
                            drop_fks.push(DdlStatement {
                                table: table.name.clone(),
                                category: "drop_foreign_key".into(),
                                object: fk.name.clone(),
                                sql: format!(
                                    "{};",
                                    drop_fk_ddl(existing, &table.name, target, target_schema)
                                ),
                            });
                        }
                    }
                    dropped_tables.insert(table.name.clone());
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
                                planned_fk_drops
                                    .insert((table.name.clone(), diff_fk.name.clone()));
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
                    &diff.source_namespace,
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

    // A table cannot be dropped while any foreign key still points at it. That
    // includes constraints on tables whose own changes the user did not select —
    // for example a target-only table referenced by an otherwise unchanged
    // table. Those references are released here regardless of the selection,
    // because the alternative is a plan that cannot execute.
    if !dropped_tables.is_empty() {
        for table in &diff.tables {
            for fk in &table.foreign_keys {
                let Some(existing) = fk.target.as_ref() else {
                    continue;
                };
                if existing.ref_table == table.name {
                    // A self-reference disappears with the table itself.
                    continue;
                }
                if !dropped_tables.contains(&existing.ref_table) {
                    continue;
                }
                if !planned_fk_drops.insert((table.name.clone(), fk.name.clone())) {
                    continue;
                }
                warnings.push(format!(
                    "表 {} 的外键 {} 指向即将删除的表 {}，已自动加入删除列表",
                    table.name, fk.name, existing.ref_table
                ));
                drop_fks.push(DdlStatement {
                    table: table.name.clone(),
                    category: "drop_foreign_key".into(),
                    object: fk.name.clone(),
                    sql: format!(
                        "{};",
                        drop_fk_ddl(existing, &table.name, target, target_schema)
                    ),
                });
            }
        }
    }

    let mut plan = Vec::new();
    // Views are dropped before any table work (PostgreSQL refuses to alter a
    // table a view depends on) and recreated once every table exists.
    plan.extend(drop_views);
    plan.extend(drop_fks);
    plan.extend(drop_tables);
    // A type can only be dropped once nothing references it, so the drops wait
    // until the very end while the creates come before the tables that use them.
    plan.extend(create_types);
    plan.extend(create_tables);
    plan.extend(body);
    plan.extend(add_indexes);
    plan.extend(add_fks);
    plan.extend(create_views);
    plan.extend(alter_types);
    plan.extend(drop_types);
    plan
}

/// Plan user-defined types. A type's own changes come before the tables that
/// use it, and its removal after everything that referenced it.
fn plan_types(
    diff: &SchemaDiff,
    selection: Option<&SyncSelection>,
    target: Dialect,
    schema: Option<&str>,
    warnings: &mut Vec<String>,
) -> (Vec<DdlStatement>, Vec<DdlStatement>, Vec<DdlStatement>) {
    if diff.types.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let selected: Option<std::collections::HashSet<&str>> = selection.map(|s| {
        s.types
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<&str>>()
    });
    let is_selected = |name: &str| match &selected {
        Some(set) => set.contains(name),
        None => true,
    };

    let mut creates = Vec::new();
    let mut alters = Vec::new();
    let mut drops = Vec::new();
    for entry in &diff.types {
        if !is_selected(&entry.name) {
            continue;
        }
        match entry.kind {
            ChangeKind::New => {
                if let Some(source) = entry.source.as_ref() {
                    creates.push(DdlStatement {
                        table: entry.name.clone(),
                        category: "create_type".into(),
                        object: entry.name.clone(),
                        sql: create_type_ddl(source, target, schema),
                    });
                }
            }
            ChangeKind::Changed => {
                alters.extend(alter_type_ddl(entry, target, schema, warnings));
            }
            ChangeKind::Missing => {
                if let Some(existing) = entry.target.as_ref() {
                    drops.push(DdlStatement {
                        table: entry.name.clone(),
                        category: "drop_type".into(),
                        object: entry.name.clone(),
                        sql: drop_type_ddl(existing, target, schema),
                    });
                }
            }
            ChangeKind::Identical => {}
        }
    }
    (creates, alters, drops)
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

// ---------------------------------------------------------------------------
// Ad-hoc table operations (drop / rename / edit columns)
// ---------------------------------------------------------------------------

/// Derive the type family from a declared type, so that incoming column
/// definitions from the UI behave exactly like introspected ones (the family
/// drives default rendering and value decoding).
pub fn family_from_declared_type(dialect: Dialect, declared: &str) -> String {
    let lower = declared.trim().to_lowercase();
    // PostgreSQL spells several types with spaces, so they are collapsed before
    // the leading word is taken. Order matters: the longer forms come first.
    let collapsed = lower
        .replace("character varying", "varchar")
        .replace("timestamp with time zone", "timestamptz")
        .replace("timestamp without time zone", "timestamp")
        .replace("time with time zone", "timetz")
        .replace("time without time zone", "time")
        .replace("double precision", "double")
        .replace("bit varying", "varbit")
        .replace("character", "char");
    let head = collapsed.split(['(', ' ']).next().unwrap_or("").to_string();

    let family = match head.as_str() {
        "varchar" => "VARCHAR",
        "char" => "CHAR",
        "int" | "int4" | "integer" | "mediumint" => "INT",
        "int2" | "smallint" => "SMALLINT",
        "int8" | "bigint" => "BIGINT",
        "bool" => "BOOLEAN",
        "numeric" | "decimal" | "dec" | "fixed" => "DECIMAL",
        "float4" | "real" => "FLOAT",
        "float8" | "double" => "DOUBLE",
        other => return other.to_ascii_uppercase(),
    };
    // MySQL carries signedness as a modifier on the declared type.
    if dialect == Dialect::Mysql && lower.contains("unsigned") && family != "BOOLEAN" {
        return format!("{family} UNSIGNED");
    }
    family.to_string()
}

/// Normalise columns submitted by the table designer: position, trimmed text
/// and a derived type family. The declared type is what the DDL uses.
pub fn normalize_designed_columns(
    dialect: Dialect,
    columns: &[ColumnInfo],
) -> AppResult<Vec<ColumnInfo>> {
    let mut out = Vec::with_capacity(columns.len());
    for (index, column) in columns.iter().enumerate() {
        let name = column.name.trim().to_string();
        if name.is_empty() {
            return Err(AppError::msg(format!(
                "第 {} 个字段缺少名称",
                index + 1
            )));
        }
        if out.iter().any(|c: &ColumnInfo| c.name.eq_ignore_ascii_case(&name)) {
            return Err(AppError::msg(format!("字段名 {name} 重复")));
        }
        let full_type = column.full_type.trim().to_string();
        if full_type.is_empty() {
            return Err(AppError::msg(format!("字段 {name} 缺少类型")));
        }
        let mut normalized = column.clone();
        normalized.name = name;
        normalized.data_type = family_from_declared_type(dialect, &full_type);
        normalized.full_type = full_type;
        normalized.ordinal = index as i64 + 1;
        normalized.default = normalized
            .default
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        normalized.comment = normalized
            .comment
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        out.push(normalized);
    }
    Ok(out)
}

/// DDL for dropping tables, in an order that releases every reference first.
///
/// A synthetic comparison is used so the ordinary planner does the work: the
/// tables to keep form the "source" (and therefore look identical), the tables
/// to drop are absent from it (and therefore look like removals). That way the
/// same foreign-key handling as a full structure sync applies — including
/// constraints living on tables the user did not touch.
pub fn plan_drop_tables(
    all: &[TableStructure],
    to_drop: &[String],
    target: Dialect,
    schema: Option<&str>,
    warnings: &mut Vec<String>,
) -> Vec<DdlStatement> {
    let kept: Vec<TableStructure> = all
        .iter()
        .filter(|table| !to_drop.iter().any(|name| name == &table.name))
        .cloned()
        .collect();
    let diff = crate::schema::diff::compare_everything(
        &kept,
        all,
        &[],
        &[],
        &[],
        &[],
        target,
        target,
        schema.unwrap_or(""),
        schema.unwrap_or(""),
        crate::schema::diff::CompareOptions::default(),
    );
    let selection = SyncSelection {
        tables: to_drop
            .iter()
            .map(|name| TableSelection {
                name: name.clone(),
                whole_table: true,
                ..TableSelection::default()
            })
            .collect(),
        views: Vec::new(),
        types: Vec::new(),
    };
    let sources: HashMap<String, TableStructure> = HashMap::new();
    generate(&diff, &sources, Some(&selection), schema, warnings)
}

/// DDL for renaming a column, preserving its data.
///
/// MySQL's `RENAME COLUMN` only exists from 8.0, so `CHANGE` is used there —
/// it is accepted by every supported version and keeps the values.
pub fn rename_column_ddl(
    table: &str,
    from: &str,
    to: &str,
    definition: &ColumnInfo,
    target: Dialect,
    schema: Option<&str>,
    source_schema: &str,
) -> String {
    let tref = target.qualified(schema, table);
    match target {
        Dialect::Mysql => {
            let rendered = column_definition(definition, target, schema, source_schema);
            format!(
                "ALTER TABLE {tref} CHANGE {} {};",
                target.quote(from),
                rendered.sql
            )
        }
        Dialect::Postgres => format!(
            "ALTER TABLE {tref} RENAME COLUMN {} TO {};",
            target.quote(from),
            target.quote(to)
        ),
    }
}

/// DDL for renaming a table.
pub fn rename_table_ddl(table: &str, new_name: &str, target: Dialect, schema: Option<&str>) -> String {
    let from = target.qualified(schema, table);
    match target {
        // MySQL takes a table reference on both sides; an unqualified target
        // would be resolved against the session's default database, which the
        // app does not set.
        Dialect::Mysql => format!(
            "RENAME TABLE {from} TO {};",
            target.qualified(schema, new_name)
        ),
        // PostgreSQL renames within the same schema and rejects a qualified
        // new name.
        Dialect::Postgres => format!("ALTER TABLE {from} RENAME TO {};", target.quote(new_name)),
    }
}

/// Validate index definitions submitted by the table editor.
///
/// Catches the mistakes that would otherwise surface as a server error halfway
/// through a batch: a missing name, two indexes sharing a name, an index with no
/// columns, or a column that no longer exists.
pub fn validate_indexes(
    indexes: &[IndexInfo],
    columns: &[ColumnInfo],
) -> AppResult<()> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for index in indexes {
        let name = index.name.trim();
        if name.is_empty() {
            return Err(AppError::msg("索引名不能为空"));
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(AppError::msg(format!("索引名 {name} 重复")));
        }
        if index.columns.is_empty() {
            return Err(AppError::msg(format!("索引 {name} 没有指定任何列")));
        }
        for entry in &index.columns {
            // A functional index keeps its expression verbatim and is checked by
            // the server; only plain column names are validated here.
            if entry.contains('(') {
                continue;
            }
            let column = entry.trim();
            if column.is_empty() {
                return Err(AppError::msg(format!("索引 {name} 有空的列名")));
            }
            if !columns
                .iter()
                .any(|candidate| candidate.name.eq_ignore_ascii_case(column))
            {
                return Err(AppError::msg(format!(
                    "索引 {name} 引用了不存在的字段 {column}"
                )));
            }
        }
    }
    Ok(())
}

/// Combine index changes into a single `ALTER TABLE` on MySQL.
///
/// MySQL validates foreign keys at the *end* of a statement, so replacing the
/// index that supports a key with a different one only works when the drop and
/// the add travel together: as two statements the drop fails with
/// "Cannot drop index … needed in a foreign key constraint".
///
/// The merged form also keeps a same-named "modify" (drop then add) valid, which
/// is why the drops come first inside the statement.
pub fn merge_mysql_index_statements(
    table: &str,
    schema: Option<&str>,
    statements: Vec<DdlStatement>,
) -> Vec<DdlStatement> {
    fn is_index(statement: &DdlStatement) -> bool {
        statement.category == "create_index" || statement.category == "drop_index"
    }

    if statements.iter().filter(|s| is_index(s)).count() < 2 {
        return statements;
    }
    let prefix = format!("ALTER TABLE {} ", Dialect::Mysql.qualified(schema, table));
    let Some(sample) = statements.iter().find(|s| is_index(s)) else {
        return statements;
    };
    // Only statements this planner produced can be rewritten safely.
    if !sample.sql.starts_with(&prefix) {
        return statements;
    }

    let mut drops = Vec::new();
    let mut adds = Vec::new();
    for statement in &statements {
        if !is_index(statement) {
            continue;
        }
        let Some(spec) = statement.sql.strip_prefix(&prefix) else {
            return statements;
        };
        let spec = spec.trim().trim_end_matches(';').to_string();
        if statement.category == "drop_index" {
            drops.push(spec);
        } else {
            adds.push(spec);
        }
    }
    drops.extend(adds);

    let mut out = Vec::with_capacity(statements.len());
    let mut merged = false;
    for statement in statements {
        if is_index(&statement) {
            if !merged {
                merged = true;
                out.push(DdlStatement {
                    table: statement.table.clone(),
                    category: "alter_indexes".into(),
                    object: table.to_string(),
                    sql: format!("{prefix}{};", drops.join(", ")),
                });
            }
            continue;
        }
        out.push(statement);
    }
    out
}

/// Validate foreign keys submitted by the table editor.
///
/// The mistakes worth catching before the server sees them: a missing name, two
/// keys sharing a name, a key with no columns, a column that is not on the table,
/// and a column count that does not match the referenced key.
pub fn validate_foreign_keys(
    foreign_keys: &[ForeignKeyInfo],
    columns: &[ColumnInfo],
) -> AppResult<()> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for foreign_key in foreign_keys {
        let name = foreign_key.name.trim();
        if name.is_empty() {
            return Err(AppError::msg("外键名不能为空"));
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(AppError::msg(format!("外键名 {name} 重复")));
        }
        if foreign_key.columns.is_empty() {
            return Err(AppError::msg(format!("外键 {name} 没有指定任何字段")));
        }
        if foreign_key.columns.len() != foreign_key.ref_columns.len() {
            return Err(AppError::msg(format!(
                "外键 {name} 的两端字段数量不一致：{} 个引用 {} 个",
                foreign_key.columns.len(),
                foreign_key.ref_columns.len()
            )));
        }
        if foreign_key.ref_table.trim().is_empty() {
            return Err(AppError::msg(format!("外键 {name} 没有指定引用表")));
        }
        for entry in &foreign_key.columns {
            let column = entry.trim();
            if column.is_empty() {
                return Err(AppError::msg(format!("外键 {name} 有空字段名")));
            }
            if !columns
                .iter()
                .any(|candidate| candidate.name.eq_ignore_ascii_case(column))
            {
                return Err(AppError::msg(format!(
                    "外键 {name} 引用了本表不存在的字段 {column}"
                )));
            }
        }
        for entry in &foreign_key.ref_columns {
            if entry.trim().is_empty() {
                return Err(AppError::msg(format!("外键 {name} 有空的目标字段名")));
            }
        }
    }
    Ok(())
}
