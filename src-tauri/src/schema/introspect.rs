use std::collections::HashMap;

use crate::db::value::ColumnKind;
use crate::db::Pool;
use crate::error::AppResult;
use crate::schema::model::{
    ColumnInfo, CustomType, ForeignKeyInfo, IndexInfo, NamespaceInfo, TableStructure,
    TableSummary, ViewStructure,
};

const MYSQL_SYSTEM_SCHEMAS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

/// Introspection queries declare each result column's kind explicitly.
///
/// Relying on the driver's result-set type info is not safe here: MySQL reports
/// the `information_schema` text columns with the binary flag set on several
/// server builds, so an inferred kind would decode `COLUMN_TYPE` (and every
/// comment, default and charset) as a hex blob.
async fn query(
    pool: &Pool,
    sql: &str,
    params: &[&str],
    kinds: &[ColumnKind],
) -> AppResult<Vec<Vec<String>>> {
    let rows = pool.fetch(sql, params, kinds).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|value| value.display().unwrap_or_default())
                .collect()
        })
        .collect())
}

const S: ColumnKind = ColumnKind::Str;
const I: ColumnKind = ColumnKind::Int;
const B: ColumnKind = ColumnKind::Bool;

// ---------------------------------------------------------------------------
// Namespaces (MySQL database / PostgreSQL schema)
// ---------------------------------------------------------------------------

pub async fn list_namespaces(pool: &Pool) -> AppResult<Vec<NamespaceInfo>> {
    match pool {
        Pool::MySql(_) => {
            let sql = "SELECT SCHEMA_NAME, DEFAULT_CHARACTER_SET_NAME, DEFAULT_COLLATION_NAME \
                       FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME";
            let rows = query(pool, sql, &[], &[S, S, S]).await?;
            Ok(rows
                .into_iter()
                .map(|r| NamespaceInfo {
                    is_system: MYSQL_SYSTEM_SCHEMAS.contains(&r[0].as_str()),
                    name: r[0].clone(),
                    charset: r.get(1).cloned(),
                    collation: r.get(2).cloned(),
                })
                .collect())
        }
        Pool::Postgres(_) => {
            let sql = "SELECT n.nspname, pg_catalog.pg_get_userbyid(n.nspowner) \
                       FROM pg_catalog.pg_namespace n \
                       WHERE n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema' \
                       ORDER BY n.nspname";
            let rows = query(pool, sql, &[], &[S, S]).await?;
            Ok(rows
                .into_iter()
                .map(|r| NamespaceInfo {
                    name: r[0].clone(),
                    charset: None,
                    collation: None,
                    is_system: r[0].starts_with("pg_"),
                })
                .collect())
        }
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

pub async fn list_tables(pool: &Pool, namespace: &str) -> AppResult<Vec<TableSummary>> {
    match pool {
        Pool::MySql(_) => {
            let sql = "SELECT TABLE_NAME, TABLE_TYPE, TABLE_COMMENT, ENGINE, TABLE_ROWS \
                       FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME";
            let rows = query(pool, sql, &[namespace], &[S, S, S, S, I]).await?;
            Ok(rows
                .into_iter()
                .map(|r| TableSummary {
                    name: r[0].clone(),
                    table_type: if r[1].eq_ignore_ascii_case("VIEW") {
                        "VIEW".into()
                    } else {
                        "BASE TABLE".into()
                    },
                    comment: non_empty(r.get(2)),
                    rows: r.get(3).and_then(|v| v.parse::<i64>().ok()),
                })
                .collect())
        }
        Pool::Postgres(_) => {
            let sql = "SELECT c.relname, \
                              CASE c.relkind WHEN 'r' THEN 'BASE TABLE' \
                                             WHEN 'p' THEN 'BASE TABLE' \
                                             WHEN 'v' THEN 'VIEW' \
                                             WHEN 'm' THEN 'MATERIALIZED VIEW' \
                                             WHEN 'f' THEN 'FOREIGN TABLE' END, \
                              obj_description(c.oid, 'pg_class'), \
                              CASE WHEN c.reltuples < 0 THEN NULL ELSE c.reltuples::bigint END \
                       FROM pg_catalog.pg_class c \
                       JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                       WHERE n.nspname = $1 AND c.relkind IN ('r','p','v','m','f') \
                       ORDER BY c.relname";
            let rows = query(pool, sql, &[namespace], &[S, S, S, I]).await?;
            Ok(rows
                .into_iter()
                .map(|r| TableSummary {
                    name: r[0].clone(),
                    table_type: r.get(1).cloned().unwrap_or_else(|| "BASE TABLE".into()),
                    comment: non_empty(r.get(2)),
                    rows: r.get(3).and_then(|v| v.parse::<i64>().ok()),
                })
                .collect())
        }
    }
}

/// Views and foreign tables cannot be synced as tables.
pub fn is_syncable(table: &TableSummary) -> bool {
    let t = table.table_type.to_uppercase();
    t.contains("BASE TABLE") || t.contains("PARTITIONED")
}

/// Cell values render booleans as "1"/"0"; accept the other common spellings too.
fn as_bool(value: &str) -> bool {
    matches!(value, "1" | "t" | "true" | "TRUE" | "YES")
}

fn non_empty(value: Option<&String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty()).cloned()
}

// ---------------------------------------------------------------------------
// Full table structure
// ---------------------------------------------------------------------------

pub async fn load_table(pool: &Pool, namespace: &str, table: &str) -> AppResult<TableStructure> {
    match pool {
        Pool::MySql(_) => load_mysql_table(pool, namespace, table).await,
        Pool::Postgres(_) => load_pg_table(pool, namespace, table).await,
    }
}

async fn load_mysql_table(pool: &Pool, schema: &str, table: &str) -> AppResult<TableStructure> {
    let meta_sql = "SELECT TABLE_COMMENT, ENGINE, TABLE_COLLATION \
                    FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?";
    let meta = query(pool, meta_sql, &[schema, table], &[S, S, S]).await?;
    let (comment, engine, collation) = match meta.first() {
        Some(row) => (
            non_empty(row.first()),
            non_empty(row.get(1)),
            non_empty(row.get(2)),
        ),
        None => (None, None, None),
    };
    let charset = collation
        .as_deref()
        .and_then(|c| c.split('_').next())
        .map(|s| s.to_string());

    let col_sql = "SELECT COLUMN_NAME, ORDINAL_POSITION, COLUMN_DEFAULT, IS_NULLABLE, DATA_TYPE, \
                          COLUMN_TYPE, COLUMN_KEY, EXTRA, COLUMN_COMMENT, CHARACTER_SET_NAME, \
                          COLLATION_NAME, GENERATION_EXPRESSION \
                   FROM information_schema.COLUMNS \
                   WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION";
    let col_rows = query(
        pool,
        col_sql,
        &[schema, table],
        &[S, I, S, S, S, S, S, S, S, S, S, S],
    )
    .await?;
    let mut columns = Vec::with_capacity(col_rows.len());
    for row in col_rows {
        let name = row[0].clone();
        let column_type = row.get(5).cloned().unwrap_or_default();
        let data_type_raw = row.get(4).cloned().unwrap_or_default();
        let extra = row.get(7).cloned().unwrap_or_default().to_lowercase();
        let is_unsigned = column_type.to_lowercase().contains("unsigned");
        let family = mysql_family(&data_type_raw, &column_type, is_unsigned);
        let auto_kind = if extra.contains("auto_increment") {
            Some("auto_increment".to_string())
        } else {
            None
        };
        // MySQL reports generated columns through EXTRA plus a dedicated
        // GENERATION_EXPRESSION column. The flag must be matched precisely:
        // `DEFAULT_GENERATED` merely means "the default is an expression" (for
        // example `DEFAULT CURRENT_TIMESTAMP`) and is NOT a generated column.
        let generation_kind = if extra.contains("stored generated") {
            Some("stored".to_string())
        } else if extra.contains("virtual generated") {
            Some("virtual".to_string())
        } else {
            None
        };
        let is_generated = generation_kind.is_some();
        let generation_expression = if is_generated {
            non_empty(row.get(11)).map(|e| e.trim().to_string())
        } else {
            None
        };
        columns.push(ColumnInfo {
            name,
            ordinal: row
                .get(1)
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or_default(),
            data_type: family,
            full_type: column_type,
            nullable: row.get(3).map(|v| v.eq_ignore_ascii_case("YES")).unwrap_or(true),
            default: if is_generated {
                None
            } else {
                normalize_mysql_default(row.get(2), &extra)
            },
            is_auto_increment: auto_kind.is_some(),
            auto_kind,
            is_generated,
            generation_expression,
            generation_kind,
            is_unsigned,
            comment: non_empty(row.get(8)),
            charset: non_empty(row.get(9)),
            collation: non_empty(row.get(10)),
            udt_name: None,
        });
    }

    let idx_sql = "SELECT INDEX_NAME, NON_UNIQUE, SEQ_IN_INDEX, COLUMN_NAME, INDEX_TYPE \
                   FROM information_schema.STATISTICS \
                   WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
                   ORDER BY INDEX_NAME, SEQ_IN_INDEX";
    let idx_rows = query(pool, idx_sql, &[schema, table], &[S, I, I, S, S]).await?;
    let (primary_key, indexes) = group_mysql_indexes(idx_rows);

    let fk_sql = "SELECT k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, \
                         k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, k.ORDINAL_POSITION, \
                         r.UPDATE_RULE, r.DELETE_RULE \
                  FROM information_schema.KEY_COLUMN_USAGE k \
                  JOIN information_schema.REFERENTIAL_CONSTRAINTS r \
                    ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA \
                   AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
                  WHERE k.TABLE_SCHEMA = ? AND k.TABLE_NAME = ? \
                    AND k.REFERENCED_TABLE_NAME IS NOT NULL \
                  ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION";
    let fk_rows = query(pool, fk_sql, &[schema, table], &[S, S, S, S, S, I, S, S]).await?;
    let foreign_keys = group_foreign_keys(fk_rows, 0, 1, 2, 3, 4, 6, 7);

    Ok(TableStructure {
        name: table.to_string(),
        comment,
        engine,
        charset,
        collation,
        primary_key,
        columns,
        indexes,
        foreign_keys,
    })
}

fn mysql_family(data_type: &str, column_type: &str, unsigned: bool) -> String {
    let base = data_type.to_ascii_uppercase();
    let lower = column_type.to_lowercase();
    let family = if base == "TINYINT" && lower.starts_with("tinyint(1)") {
        "BOOLEAN".to_string()
    } else {
        match base.as_str() {
            "INT" | "INTEGER" => "INT".into(),
            "NUMERIC" | "DEC" | "FIXED" => "DECIMAL".into(),
            "DOUBLE" | "REAL" => "DOUBLE".into(),
            "BOOL" | "BOOLEAN" => "BOOLEAN".into(),
            other => other.to_string(),
        }
    };
    if unsigned && family != "BOOLEAN" {
        format!("{family} UNSIGNED")
    } else {
        family
    }
}

/// MySQL 8 reports expression defaults with an `DEFAULT_GENERATED` extra flag.
fn normalize_mysql_default(value: Option<&String>, extra: &str) -> Option<String> {
    let raw = value?;
    if raw.is_empty() {
        return None;
    }
    let is_expression = extra.contains("default_generated");
    if is_expression && !raw.starts_with('(') && !raw.eq_ignore_ascii_case("null") {
        // CURRENT_TIMESTAMP style keywords stay as-is; anything else is an
        // expression and must be parenthesised when replayed.
        let upper = raw.to_ascii_uppercase();
        let keywordish = upper.starts_with("CURRENT_TIMESTAMP")
            || upper.starts_with("NOW()")
            || upper.starts_with("CURRENT_DATE")
            || upper.starts_with("CURRENT_TIME")
            || upper.starts_with("LOCALTIME")
            || upper.starts_with("LOCALTIMESTAMP");
        if !keywordish {
            return Some(format!("({raw})"));
        }
    }
    Some(raw.clone())
}

fn group_mysql_indexes(rows: Vec<Vec<String>>) -> (Vec<String>, Vec<IndexInfo>) {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, IndexInfo> = HashMap::new();
    for row in rows {
        let name = row[0].clone();
        if !grouped.contains_key(&name) {
            order.push(name.clone());
            grouped.insert(
                name.clone(),
                IndexInfo {
                    name: name.clone(),
                    columns: Vec::new(),
                    is_unique: row.get(1).map(|v| v == "0").unwrap_or(false),
                    index_type: non_empty(row.get(4)),
                },
            );
        }
        if let Some(entry) = grouped.get_mut(&name) {
            if let Some(col) = row.get(3) {
                entry.columns.push(col.clone());
            }
        }
    }
    let mut primary_key = Vec::new();
    let mut indexes = Vec::new();
    for name in order {
        if let Some(entry) = grouped.remove(&name) {
            if entry.name == "PRIMARY" {
                primary_key = entry.columns;
            } else {
                indexes.push(entry);
            }
        }
    }
    (primary_key, indexes)
}

#[allow(clippy::too_many_arguments)]
fn group_foreign_keys(
    rows: Vec<Vec<String>>,
    name_idx: usize,
    col_idx: usize,
    ref_schema_idx: usize,
    ref_table_idx: usize,
    ref_col_idx: usize,
    update_idx: usize,
    delete_idx: usize,
) -> Vec<ForeignKeyInfo> {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, ForeignKeyInfo> = HashMap::new();
    for row in rows {
        let name = row[name_idx].clone();
        if !grouped.contains_key(&name) {
            order.push(name.clone());
            grouped.insert(
                name.clone(),
                ForeignKeyInfo {
                    name: name.clone(),
                    columns: Vec::new(),
                    ref_schema: non_empty(row.get(ref_schema_idx)),
                    ref_table: row.get(ref_table_idx).cloned().unwrap_or_default(),
                    ref_columns: Vec::new(),
                    on_update: row
                        .get(update_idx)
                        .cloned()
                        .unwrap_or_else(|| "NO ACTION".into())
                        .to_uppercase(),
                    on_delete: row
                        .get(delete_idx)
                        .cloned()
                        .unwrap_or_else(|| "NO ACTION".into())
                        .to_uppercase(),
                },
            );
        }
        if let Some(entry) = grouped.get_mut(&name) {
            if let Some(col) = row.get(col_idx) {
                entry.columns.push(col.clone());
            }
            if let Some(col) = row.get(ref_col_idx) {
                entry.ref_columns.push(col.clone());
            }
        }
    }
    order.into_iter().filter_map(|n| grouped.remove(&n)).collect()
}

// ---------------------------------------------------------------------------
// PostgreSQL
// ---------------------------------------------------------------------------

async fn load_pg_table(pool: &Pool, schema: &str, table: &str) -> AppResult<TableStructure> {
    let meta_sql = "SELECT obj_description(c.oid, 'pg_class') \
                    FROM pg_catalog.pg_class c \
                    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                    WHERE n.nspname = $1 AND c.relname = $2";
    let meta = query(pool, meta_sql, &[schema, table], &[S]).await?;
    let comment = meta.first().and_then(|r| non_empty(r.first()));

    let col_sql = "SELECT a.attname, a.attnum, t.typname, \
                          format_type(a.atttypid, a.atttypmod), \
                          NOT a.attnotnull, \
                          pg_get_expr(d.adbin, d.adrelid), \
                          col_description(a.attrelid, a.attnum), \
                          a.attidentity::text, a.attgenerated::text, \
                          coll.collname, \
                          CASE WHEN t.typtype IN ('e', 'd') THEN t.typname END \
                   FROM pg_catalog.pg_attribute a \
                   JOIN pg_catalog.pg_type t ON t.oid = a.atttypid \
                   LEFT JOIN pg_catalog.pg_attrdef d \
                     ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                   LEFT JOIN pg_catalog.pg_collation coll \
                     ON coll.oid = a.attcollation AND a.attcollation <> t.typcollation \
                   WHERE a.attrelid = (SELECT c.oid FROM pg_catalog.pg_class c \
                                       JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                                       WHERE n.nspname = $1 AND c.relname = $2) \
                     AND a.attnum > 0 AND NOT a.attisdropped \
                   ORDER BY a.attnum";
    let col_rows = query(
        pool,
        col_sql,
        &[schema, table],
        &[S, I, S, S, B, S, S, S, S, S, S],
    )
    .await?;

    let pk_sql = "SELECT a.attname FROM pg_catalog.pg_index i \
                  JOIN pg_catalog.pg_class c ON c.oid = i.indrelid \
                  JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                  JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord) ON true \
                  JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum = k.attnum \
                  WHERE n.nspname = $1 AND c.relname = $2 AND i.indisprimary \
                  ORDER BY k.ord";
    let primary_key: Vec<String> = query(pool, pk_sql, &[schema, table], &[S])
        .await?
        .into_iter()
        .filter_map(|mut row| row.drain(..).next())
        .collect();

    let mut columns = Vec::with_capacity(col_rows.len());
    for row in col_rows {
        let typname = row.get(2).cloned().unwrap_or_default();
        let full_type = row.get(3).cloned().unwrap_or_default();
        let raw_default = non_empty(row.get(5));
        let identity = row.get(7).cloned().unwrap_or_default();
        let generated_flag = row.get(8).cloned().unwrap_or_default();
        let is_generated = generated_flag == "s";
        let mut auto_kind = None;
        let mut default = raw_default.clone();
        if identity == "a" {
            auto_kind = Some("identity_always".to_string());
            default = None;
        } else if identity == "d" {
            auto_kind = Some("identity_default".to_string());
            default = None;
        } else if let Some(d) = raw_default.as_deref() {
            if d.starts_with("nextval(") {
                auto_kind = Some("serial".to_string());
                default = None;
            }
        }
        let name = row[0].clone();
        let collation = non_empty(row.get(9));
        // A generated column stores its expression in pg_attrdef too; it must
        // not be mistaken for a DEFAULT.
        let generation_expression = if is_generated {
            raw_default.clone()
        } else {
            None
        };
        if is_generated {
            default = None;
            auto_kind = None;
        }
        columns.push(ColumnInfo {
            name,
            ordinal: row
                .get(1)
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or_default(),
            data_type: pg_family(&typname),
            full_type,
            nullable: row.get(4).map(|v| as_bool(v)).unwrap_or(true),
            default,
            is_auto_increment: auto_kind.is_some(),
            auto_kind,
            is_generated,
            generation_expression,
            generation_kind: is_generated.then(|| "stored".to_string()),
            is_unsigned: false,
            comment: non_empty(row.get(6)),
            charset: None,
            collation,
            udt_name: non_empty(row.get(10)),
        });
    }

    let idx_sql = "SELECT ic.relname, i.indisunique, am.amname, \
                          COALESCE(a.attname, pg_get_indexdef(i.indexrelid, k.ord::int, true)), \
                          k.ord \
                   FROM pg_catalog.pg_index i \
                   JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid \
                   JOIN pg_catalog.pg_class tc ON tc.oid = i.indrelid \
                   JOIN pg_catalog.pg_namespace n ON n.oid = tc.relnamespace \
                   JOIN pg_catalog.pg_am am ON am.oid = ic.relam \
                   JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord) ON true \
                   LEFT JOIN pg_catalog.pg_attribute a \
                     ON a.attrelid = tc.oid AND a.attnum = k.attnum \
                   WHERE n.nspname = $1 AND tc.relname = $2 AND NOT i.indisprimary \
                   ORDER BY ic.relname, k.ord";
    let idx_rows = query(pool, idx_sql, &[schema, table], &[S, B, S, S, I]).await?;
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, IndexInfo> = HashMap::new();
    for row in idx_rows {
        let name = row[0].clone();
        if !grouped.contains_key(&name) {
            order.push(name.clone());
            grouped.insert(
                name.clone(),
                IndexInfo {
                    name: name.clone(),
                    columns: Vec::new(),
                    is_unique: row.get(1).map(|v| as_bool(v)).unwrap_or(false),
                    index_type: non_empty(row.get(2)),
                },
            );
        }
        if let Some(entry) = grouped.get_mut(&name) {
            if let Some(col) = row.get(3) {
                entry.columns.push(col.clone());
            }
        }
    }
    let indexes: Vec<IndexInfo> = order.into_iter().filter_map(|n| grouped.remove(&n)).collect();

    let fk_sql = "SELECT con.conname, att.attname, n2.nspname, cl2.relname, att2.attname, \
                         con.confupdtype::text, con.confdeltype::text, k.ord \
                  FROM pg_catalog.pg_constraint con \
                  JOIN pg_catalog.pg_class cl ON cl.oid = con.conrelid \
                  JOIN pg_catalog.pg_namespace n ON n.oid = cl.relnamespace \
                  JOIN pg_catalog.pg_class cl2 ON cl2.oid = con.confrelid \
                  JOIN pg_catalog.pg_namespace n2 ON n2.oid = cl2.relnamespace \
                  JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord) ON true \
                  JOIN pg_catalog.pg_attribute att \
                    ON att.attrelid = con.conrelid AND att.attnum = k.attnum \
                  JOIN LATERAL unnest(con.confkey) WITH ORDINALITY AS k2(attnum, ord) \
                    ON k2.ord = k.ord \
                  JOIN pg_catalog.pg_attribute att2 \
                    ON att2.attrelid = con.confrelid AND att2.attnum = k2.attnum \
                  WHERE con.contype = 'f' AND n.nspname = $1 AND cl.relname = $2 \
                  ORDER BY con.conname, k.ord";
    let fk_rows = query(pool, fk_sql, &[schema, table], &[S, S, S, S, S, S, S, I]).await?;
    let foreign_keys = fk_rows
        .into_iter()
        .fold(Vec::<ForeignKeyInfo>::new(), |mut acc, row| {
            let name = row[0].clone();
            let update = pg_action(row.get(5).map(String::as_str).unwrap_or("a"));
            let delete = pg_action(row.get(6).map(String::as_str).unwrap_or("a"));
            match acc.iter_mut().find(|f| f.name == name) {
                Some(existing) => {
                    existing.columns.push(row[1].clone());
                    existing.ref_columns.push(row[4].clone());
                }
                None => acc.push(ForeignKeyInfo {
                    name,
                    columns: vec![row[1].clone()],
                    ref_schema: non_empty(row.get(2)),
                    ref_table: row[3].clone(),
                    ref_columns: vec![row[4].clone()],
                    on_update: update,
                    on_delete: delete,
                }),
            }
            acc
        });

    Ok(TableStructure {
        name: table.to_string(),
        comment,
        engine: None,
        charset: None,
        collation: None,
        primary_key,
        columns,
        indexes,
        foreign_keys,
    })
}

fn pg_family(typname: &str) -> String {
    let lower = typname.to_ascii_lowercase();
    if lower.starts_with('_') {
        return format!("{}[]", lower[1..].to_ascii_uppercase());
    }
    match lower.as_str() {
        "int2" => "SMALLINT".into(),
        "int4" => "INT".into(),
        "int8" => "BIGINT".into(),
        "float4" => "REAL".into(),
        "float8" => "DOUBLE".into(),
        "numeric" => "DECIMAL".into(),
        "bpchar" => "CHAR".into(),
        "varchar" => "VARCHAR".into(),
        "bool" => "BOOLEAN".into(),
        "timestamptz" => "TIMESTAMPTZ".into(),
        "timestamp" => "TIMESTAMP".into(),
        "bytea" => "BYTEA".into(),
        "jsonb" => "JSONB".into(),
        other => other.to_ascii_uppercase(),
    }
}

fn pg_action(code: &str) -> String {
    match code {
        "r" => "RESTRICT",
        "c" => "CASCADE",
        "n" => "SET NULL",
        "d" => "SET DEFAULT",
        _ => "NO ACTION",
    }
    .to_string()
}

// ---------------------------------------------------------------------------
// Views and materialised views
// ---------------------------------------------------------------------------

pub async fn list_views(pool: &Pool, namespace: &str) -> AppResult<Vec<ViewStructure>> {
    let mut views = match pool {
        Pool::MySql(_) => {
            let sql = "SELECT TABLE_NAME, VIEW_DEFINITION FROM information_schema.VIEWS \
                       WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME";
            let rows = query(pool, sql, &[namespace], &[S, S]).await?;
            rows.into_iter()
                .map(|row| ViewStructure {
                    name: row[0].clone(),
                    kind: "view".into(),
                    definition: row.get(1).cloned().unwrap_or_default(),
                    comment: None,
                    dependencies: Vec::new(),
                })
                .collect::<Vec<_>>()
        }
        Pool::Postgres(_) => {
            // `pg_get_viewdef` returns the SELECT without a trailing semicolon.
            // `relkind` is PostgreSQL's internal `"char"` type, which the driver
            // cannot decode; cast it explicitly.
            let sql = "SELECT c.relname, c.relkind::text, pg_get_viewdef(c.oid, true), \
                              obj_description(c.oid, 'pg_class') \
                       FROM pg_catalog.pg_class c \
                       JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                       WHERE n.nspname = $1 AND c.relkind IN ('v', 'm') \
                       ORDER BY c.relname";
            let rows = query(pool, sql, &[namespace], &[S, S, S, S]).await?;
            rows.into_iter()
                .map(|row| ViewStructure {
                    name: row[0].clone(),
                    kind: if row.get(1).map(String::as_str) == Some("m") {
                        "materialized".into()
                    } else {
                        "view".into()
                    },
                    definition: row.get(2).cloned().unwrap_or_default(),
                    comment: non_empty(row.get(3)),
                    dependencies: Vec::new(),
                })
                .collect::<Vec<_>>()
        }
    };

    // Dependencies are derived from the bodies rather than from a catalog
    // query: it works identically on both engines and on older MySQL versions.
    let mut known: Vec<String> = list_tables(pool, namespace)
        .await?
        .iter()
        .map(|table| table.name.clone())
        .collect();
    known.extend(views.iter().map(|view| view.name.clone()));

    for view in &mut views {
        view.dependencies = relations_referenced(&view.definition, &known, &view.name);
    }
    Ok(views)
}

/// Relation names that appear as whole identifiers in `definition`.
fn relations_referenced(definition: &str, known: &[String], self_name: &str) -> Vec<String> {
    let mut found: Vec<String> = known
        .iter()
        .filter(|name| !name.eq_ignore_ascii_case(self_name))
        .filter(|name| contains_identifier(definition, name))
        .cloned()
        .collect();
    found.sort();
    found.dedup();
    found
}

fn contains_identifier(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hay = haystack.to_lowercase();
    let target = needle.to_lowercase();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let bytes: Vec<char> = hay.chars().collect();
    let target_chars: Vec<char> = target.chars().collect();
    if target_chars.len() > bytes.len() {
        return false;
    }
    for start in 0..=(bytes.len() - target_chars.len()) {
        if bytes[start..start + target_chars.len()] != target_chars[..] {
            continue;
        }
        let before_ok = start == 0 || !is_word(bytes[start - 1]);
        let after = start + target_chars.len();
        let after_ok = after >= bytes.len() || !is_word(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// User-defined types (PostgreSQL only)
// ---------------------------------------------------------------------------

pub async fn list_custom_types(pool: &Pool, namespace: &str) -> AppResult<Vec<CustomType>> {
    if matches!(pool, Pool::MySql(_)) {
        // MySQL declares ENUM/SET inline on the column; there is no separate type.
        return Ok(Vec::new());
    }

    let enum_sql = "SELECT t.typname, e.enumlabel, obj_description(t.oid, 'pg_type') \
                    FROM pg_catalog.pg_type t \
                    JOIN pg_catalog.pg_enum e ON e.enumtypid = t.oid \
                    JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
                    WHERE n.nspname = $1 \
                    ORDER BY t.typname, e.enumsortorder";
    let enum_rows = query(pool, enum_sql, &[namespace], &[S, S, S]).await?;
    let mut enums: Vec<CustomType> = Vec::new();
    for row in enum_rows {
        let name = row[0].clone();
        match enums.iter_mut().find(|t| t.name == name) {
            Some(existing) => existing.labels.push(row[1].clone()),
            None => enums.push(CustomType {
                name,
                kind: "enum".into(),
                labels: vec![row[1].clone()],
                base_type: None,
                domain_nullable: false,
                domain_default: None,
                constraints: Vec::new(),
                comment: non_empty(row.get(2)),
            }),
        }
    }

    let domain_sql = "SELECT t.typname, format_type(t.typbasetype, t.typtypmod), \
                             t.typnotnull, t.typdefault, obj_description(t.oid, 'pg_type') \
                      FROM pg_catalog.pg_type t \
                      JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
                      WHERE n.nspname = $1 AND t.typtype = 'd' \
                      ORDER BY t.typname";
    let domain_rows = query(pool, domain_sql, &[namespace], &[S, S, B, S, S]).await?;
    let mut domains: Vec<CustomType> = Vec::new();
    for row in domain_rows {
        domains.push(CustomType {
            name: row[0].clone(),
            kind: "domain".into(),
            labels: Vec::new(),
            base_type: non_empty(row.get(1)),
            domain_nullable: !as_bool(row.get(2).map(String::as_str).unwrap_or("f")),
            domain_default: non_empty(row.get(3)),
            constraints: Vec::new(),
            comment: non_empty(row.get(4)),
        });
    }

    if !domains.is_empty() {
        let constraint_sql = "SELECT t.typname, c.conname, pg_get_constraintdef(c.oid) \
                              FROM pg_catalog.pg_constraint c \
                              JOIN pg_catalog.pg_type t ON t.oid = c.contypid \
                              JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
                              WHERE n.nspname = $1 AND c.contype = 'c' \
                              ORDER BY t.typname, c.conname";
        let rows = query(pool, constraint_sql, &[namespace], &[S, S, S]).await?;
        for row in rows {
            if let Some(domain) = domains.iter_mut().find(|d| d.name == row[0]) {
                domain.constraints.push(row[2].clone());
            }
        }
    }

    let mut all = enums;
    all.extend(domains);
    all.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(all)
}

/// Every `(child, parent)` foreign-key edge in a namespace.
///
/// Used before dropping tables: a table can only be dropped once the
/// constraints pointing at it are gone, and those live on other tables.
pub async fn list_foreign_key_edges(
    pool: &Pool,
    namespace: &str,
) -> AppResult<Vec<(String, String)>> {
    let rows = match pool {
        Pool::MySql(_) => {
            let sql = "SELECT TABLE_NAME, REFERENCED_TABLE_NAME \
                       FROM information_schema.KEY_COLUMN_USAGE \
                       WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL \
                       GROUP BY TABLE_NAME, REFERENCED_TABLE_NAME";
            query(pool, sql, &[namespace], &[S, S]).await?
        }
        Pool::Postgres(_) => {
            let sql = "SELECT DISTINCT cl.relname, ref.relname \
                       FROM pg_catalog.pg_constraint con \
                       JOIN pg_catalog.pg_class cl ON cl.oid = con.conrelid \
                       JOIN pg_catalog.pg_namespace n ON n.oid = cl.relnamespace \
                       JOIN pg_catalog.pg_class ref ON ref.oid = con.confrelid \
                       WHERE con.contype = 'f' AND n.nspname = $1";
            query(pool, sql, &[namespace], &[S, S]).await?
        }
    };
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let child = row.first()?.clone();
            let parent = row.get(1)?.clone();
            (!child.is_empty() && !parent.is_empty()).then_some((child, parent))
        })
        .collect())
}
