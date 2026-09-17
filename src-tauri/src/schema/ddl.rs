use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::dialect::Dialect;
use crate::schema::diff::{ChangeKind, SchemaDiff, TableDiff};
use crate::schema::model::{ColumnInfo, ForeignKeyInfo, IndexInfo, TableStructure};

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
}

impl SyncSelection {
    fn table(&self, name: &str) -> Option<&TableSelection> {
        self.tables.iter().find(|t| t.name == name)
    }
}

// ---------------------------------------------------------------------------
// Type mapping
// ---------------------------------------------------------------------------

/// Lower-case canonical base name plus its argument list.
fn parse_type(full_type: &str) -> (String, Option<String>) {
    let lower = full_type.trim().to_lowercase();
    let (base, args, tail) = match lower.find('(') {
        Some(open) => {
            let end = lower.rfind(')').unwrap_or(lower.len());
            let base = lower[..open].trim().to_string();
            let args = lower[open + 1..end].trim().to_string();
            let tail = if end < lower.len() {
                lower[end + 1..].trim().to_string()
            } else {
                String::new()
            };
            (base, args, tail)
        }
        None => (lower.trim().to_string(), String::new(), String::new()),
    };
    let combined = if tail.is_empty() {
        base
    } else {
        format!("{base} {tail}")
    };
    let base = normalize_base(&combined);
    (base, if args.is_empty() { None } else { Some(args) })
}

fn normalize_base(base: &str) -> String {
    let collapsed = base.split_whitespace().collect::<Vec<_>>().join(" ");
    match collapsed.as_str() {
        "character varying" | "varchar" => "varchar".into(),
        "character" | "bpchar" | "char" => "char".into(),
        "timestamp with time zone" | "timestamptz" => "timestamptz".into(),
        "timestamp without time zone" => "timestamp".into(),
        "time with time zone" | "timetz" => "timetz".into(),
        "time without time zone" | "time" => "time".into(),
        "double precision" | "float8" | "double" => "double".into(),
        "real" | "float4" => "real".into(),
        "integer" | "int4" | "int" | "mediumint" => "integer".into(),
        "int2" => "smallint".into(),
        "int8" => "bigint".into(),
        "numeric" | "decimal" | "dec" | "fixed" => "decimal".into(),
        "bool" => "boolean".into(),
        "bit varying" => "varbit".into(),
        other => {
            let stripped = other
                .replace(" unsigned", "")
                .replace(" zerofill", "")
                .replace("[]", "");
            stripped.trim().to_string()
        }
    }
}

fn has_unsigned(full_type: &str) -> bool {
    full_type.to_lowercase().contains("unsigned")
}

fn first_arg(args: &Option<String>) -> Option<String> {
    args.as_ref()
        .and_then(|a| a.split(',').next())
        .map(|s| s.trim().to_string())
}

/// The declared type to use on the `target` engine.
pub fn map_type(source: Dialect, target: Dialect, column: &ColumnInfo) -> String {
    if source == target {
        return column.full_type.clone();
    }
    let (base, args) = parse_type(&column.full_type);
    let unsigned = column.is_unsigned || has_unsigned(&column.full_type);
    let arg_text = args.clone().unwrap_or_default();
    let length = first_arg(&args).and_then(|v| v.parse::<i64>().ok());

    match target {
        Dialect::Postgres => match base.as_str() {
            "boolean" => "boolean".into(),
            "tinyint" => {
                if column.data_type.eq_ignore_ascii_case("BOOLEAN") {
                    "boolean".into()
                } else {
                    "smallint".into()
                }
            }
            "smallint" => "smallint".into(),
            "integer" => "integer".into(),
            "bigint" => {
                if unsigned {
                    "numeric(20)".into()
                } else {
                    "bigint".into()
                }
            }
            "decimal" => {
                if arg_text.is_empty() {
                    "numeric".into()
                } else {
                    format!("numeric({arg_text})")
                }
            }
            "real" => "real".into(),
            "double" => "double precision".into(),
            "char" => {
                if arg_text.is_empty() {
                    "char(1)".into()
                } else {
                    format!("char({arg_text})")
                }
            }
            "varchar" => match length {
                Some(n) if n > 10_485_760 => "text".into(),
                Some(n) => format!("varchar({n})"),
                None => "varchar".into(),
            },
            "tinytext" | "text" | "mediumtext" | "longtext" | "enum" | "set" => "text".into(),
            "binary" | "varbinary" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "bytea" => {
                "bytea".into()
            }
            "date" => "date".into(),
            "time" => {
                if arg_text.is_empty() {
                    "time".into()
                } else {
                    format!("time({arg_text})")
                }
            }
            "datetime" => {
                if arg_text.is_empty() {
                    "timestamp".into()
                } else {
                    format!("timestamp({arg_text})")
                }
            }
            "timestamp" => {
                if arg_text.is_empty() {
                    "timestamptz".into()
                } else {
                    format!("timestamptz({arg_text})")
                }
            }
            "year" => "smallint".into(),
            "json" => "jsonb".into(),
            "bit" => match length {
                Some(1) | None => "boolean".into(),
                Some(n) => format!("bit({n})"),
            },
            "geometry" => "bytea".into(),
            other => {
                if other.starts_with("enum") || other.starts_with("set") {
                    "text".into()
                } else {
                    "text".into()
                }
            }
        },
        Dialect::Mysql => match base.as_str() {
            "boolean" => "tinyint(1)".into(),
            "smallint" => "smallint".into(),
            "integer" => "int".into(),
            "bigint" => "bigint".into(),
            "decimal" => {
                if arg_text.is_empty() {
                    "decimal(65,30)".into()
                } else {
                    format!("decimal({arg_text})")
                }
            }
            "real" => "float".into(),
            "double" => "double".into(),
            "char" => {
                if arg_text.is_empty() {
                    "char(1)".into()
                } else {
                    format!("char({arg_text})")
                }
            }
            "varchar" => match length {
                Some(n) if n > 65_535 => "longtext".into(),
                Some(n) => format!("varchar({n})"),
                None => "varchar(255)".into(),
            },
            "text" | "xml" | "tsvector" => "longtext".into(),
            "bytea" => "longblob".into(),
            "date" => "date".into(),
            "time" | "timetz" => {
                if arg_text.is_empty() {
                    "time".into()
                } else {
                    format!("time({arg_text})")
                }
            }
            "timestamp" | "timestamptz" => {
                if arg_text.is_empty() {
                    "datetime".into()
                } else {
                    format!("datetime({arg_text})")
                }
            }
            "json" | "jsonb" => "json".into(),
            "uuid" => "char(36)".into(),
            "inet" | "cidr" => "varchar(64)".into(),
            "macaddr" | "macaddr8" => "varchar(32)".into(),
            "interval" => "varchar(64)".into(),
            "varbit" => "varchar(64)".into(),
            "money" => "decimal(19,4)".into(),
            other if other.starts_with('_') => "json".into(),
            _ => "longtext".into(),
        },
    }
}

/// Type text used when deciding whether a column changed.
pub fn map_type_for_compare(source: Dialect, target: Dialect, column: &ColumnInfo) -> String {
    if source == target {
        column.canonical_type()
    } else {
        map_type(source, target, column).to_lowercase()
    }
}

// ---------------------------------------------------------------------------
// Default value handling
// ---------------------------------------------------------------------------

fn strip_pg_cast(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut in_quote = false;
    let mut i = 0;
    while i + 1 < bytes.len() {
        match bytes[i] {
            b'\'' => in_quote = !in_quote,
            b':' if !in_quote && bytes[i + 1] == b':' => {
                return value[..i].trim().to_string();
            }
            _ => {}
        }
        i += 1;
    }
    value.trim().to_string()
}

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

/// Render the DEFAULT clause for a column on the target engine, or `None` when
/// there is no usable default.
fn render_default(
    column: &ColumnInfo,
    source: Dialect,
    target: Dialect,
) -> Option<String> {
    let raw = column.default.as_ref()?.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let upper = raw.to_ascii_uppercase();
    if upper == "NULL" {
        return None;
    }

    // Expressions that are valid on both engines.
    if is_time_keyword(&upper) {
        return Some(match (source, target) {
            (Dialect::Postgres, Dialect::Mysql) => "CURRENT_TIMESTAMP".to_string(),
            _ => raw.clone(),
        });
    }

    // A default produced by a generator expression keeps its parentheses.
    let looks_like_expression = raw.starts_with('(')
        || upper.contains("NEXTVAL(")
        || upper.ends_with(')')
        || upper.starts_with("ARRAY[");

    if source == Dialect::Postgres {
        let stripped = strip_pg_cast(&raw);
        if target == Dialect::Postgres {
            return Some(stripped);
        }
        // PostgreSQL -> MySQL
        if stripped.eq_ignore_ascii_case("true") {
            return Some("1".into());
        }
        if stripped.eq_ignore_ascii_case("false") {
            return Some("0".into());
        }
        if stripped.starts_with('\'') {
            return Some(stripped);
        }
        if is_numeric_family(column) || stripped.parse::<f64>().is_ok() {
            return Some(stripped);
        }
        return Some(Dialect::Mysql.string_literal(&stripped));
    }

    // MySQL -> ...
    if looks_like_expression {
        return Some(raw.clone());
    }
    if is_boolean_family(column) {
        let truthy = matches!(upper.as_str(), "1" | "TRUE" | "B'1'");
        return Some(match target {
            Dialect::Mysql => {
                if truthy {
                    "1".into()
                } else {
                    "0".into()
                }
            }
            Dialect::Postgres => {
                if truthy {
                    "true".into()
                } else {
                    "false".into()
                }
            }
        });
    }
    if is_numeric_family(column) && raw.parse::<f64>().is_ok() {
        return Some(raw);
    }
    match target {
        Dialect::Mysql => Some(Dialect::Mysql.string_literal(&raw)),
        Dialect::Postgres => Some(Dialect::Postgres.string_literal(&raw)),
    }
}

// ---------------------------------------------------------------------------
// Column definitions
// ---------------------------------------------------------------------------

/// Constructs that exist in one engine's expression language but not the
/// other. When any of these survive translation the generated column is
/// created as a plain column instead of guessing at a wrong expression.
const NON_PORTABLE: [&str; 14] = [
    "::", "jsonb_", "json_", "->>", "->", "regexp_", "array_", "string_agg", "unnest",
    "substring_index", "group_concat", "date_format", "str_to_date", "convert_tz",
];

/// Mechanically translate a generation expression between dialects, reporting
/// whether the result is trustworthy. Cross-dialect expressions are always
/// surfaced as a warning so the user can review them.
fn port_generation_expression(
    expression: &str,
    source: Dialect,
    target: Dialect,
) -> (String, bool) {
    let trimmed = expression.trim();
    if source == target {
        return (trimmed.to_string(), true);
    }
    let mut ported = trimmed.to_string();
    match (source, target) {
        // MySQL quotes identifiers with backticks; PostgreSQL needs quotes.
        (Dialect::Mysql, Dialect::Postgres) => {
            ported = ported
                .replace('`', "\"")
                .replace("ifnull(", "coalesce(")
                .replace("IFNULL(", "coalesce(");
        }
        // PostgreSQL double-quotes identifiers and annotates casts; MySQL uses
        // backticks and has no `::`.
        (Dialect::Postgres, Dialect::Mysql) => {
            ported = ported.replace('"', "`");
            ported = strip_pg_casts(&ported);
        }
        // Same engine: handled by the early return above.
        _ => {}
    }
    let lowered = ported.to_lowercase();
    let trustworthy =
        !NON_PORTABLE.iter().any(|needle| lowered.contains(needle)) && !ported.contains("select ");
    (ported, trustworthy)
}

/// Remove `expr::type` annotations, which MySQL does not understand.
pub(crate) fn strip_pg_casts(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut in_quote = false;
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if ch == '\'' {
            in_quote = !in_quote;
            out.push(ch);
            i += 1;
            continue;
        }
        if !in_quote && ch == ':' && i + 1 < bytes.len() && bytes[i + 1] == b':' {
            i += 2;
            // Skip the type name (identifiers, spaces, digits, parentheses).
            while i < bytes.len() {
                let c = bytes[i] as char;
                if c.is_alphanumeric() || c == '_' || c == ' ' || c == '(' || c == ')' || c == ',' {
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// A rendered column definition plus anything the user should know about it.
struct ColumnDdl {
    sql: String,
    warning: Option<String>,
}

fn column_definition(column: &ColumnInfo, source: Dialect, target: Dialect) -> ColumnDdl {
    let q = |name: &str| target.quote(name);
    let ty = map_type(source, target, column);
    let mut parts = vec![format!("{} {}", q(&column.name), ty)];

    if column.is_generated {
        if let Some(expression) = column
            .generation_expression
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            let (ported, trustworthy) = port_generation_expression(expression, source, target);
            if trustworthy {
                // PostgreSQL only supports STORED generated columns.
                let kind = match target {
                    Dialect::Postgres => "STORED",
                    Dialect::Mysql => match column.generation_kind.as_deref() {
                        Some("virtual") => "VIRTUAL",
                        _ => "STORED",
                    },
                };
                parts.push(format!("GENERATED ALWAYS AS ({ported}) {kind}"));
                let warning = (source != target).then(|| {
                    format!(
                        "列 {} 的生成表达式已按 {} 语法机械转换为「{ported}」，请核对结果是否符合预期",
                        column.name,
                        target.label()
                    )
                });
                return ColumnDdl {
                    sql: parts.join(" "),
                    warning,
                };
            }
        }
        // No usable expression: fall back to an ordinary column so the table can
        // still be created, and warn rather than silently producing a column
        // whose values will never be computed.
        let reason = match column.generation_expression.as_deref().map(str::trim) {
            Some(expression) if !expression.is_empty() => format!(
                "生成表达式无法可靠转换为 {} 语法（原表达式：{expression}），已改为普通列，请手动补回",
                target.label()
            ),
            _ => "服务端未返回生成表达式，已改为普通列，请手动补回".to_string(),
        };
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
            warning: Some(format!("列 {}：{reason}", column.name)),
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
        if let Some(default) = render_default(column, source, target) {
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
    source: Dialect,
    target: Dialect,
    schema: Option<&str>,
    source_schema: &str,
    warnings: &mut Vec<String>,
) -> Vec<DdlStatement> {
    let q = |name: &str| target.quote(name);
    let tref = target.qualified(schema, &structure.name);
    let mut body: Vec<String> = Vec::new();
    for column in &structure.columns {
        let definition = column_definition(column, source, target);
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
    source: &Dialect,
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
                let definition = column_definition(column, *source, target);
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
                    let definition = column_definition(column, *source, target);
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
                    let new_type = map_type(*source, target, column);
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
                        let clause = match render_default(column, *source, target) {
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
    let source_dialect = diff.source_dialect;
    let target = diff.target_dialect;
    // `None` means "no explicit selection was supplied, apply every change".
    // `Some(empty)` means the user unchecked everything and must apply nothing.
    let is_selected = |table: &str| match selection {
        Some(s) => s.table(table).is_some(),
        None => true,
    };

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
                            source_dialect,
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
                    &source_dialect,
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
    plan.extend(drop_fks);
    plan.extend(drop_tables);
    plan.extend(create_tables);
    plan.extend(body);
    plan.extend(add_indexes);
    plan.extend(add_fks);
    plan
}

fn diff_has_comment_change(table: &TableDiff) -> bool {
    if table.kind != ChangeKind::Changed {
        return false;
    }
    let source = table.source_comment.as_deref().unwrap_or("");
    let target = table.target_comment.as_deref().unwrap_or("");
    source != target
}
