use std::collections::BTreeMap;

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::mysql::MySqlRow;
use sqlx::postgres::PgRow;
use sqlx::{Column, Row, TypeInfo, ValueRef};

use crate::dialect::Dialect;
use crate::error::{AppError, AppResult};

/// How a column's value should be decoded. Derived from the *introspected*
/// declared type rather than the result-set type info, which keeps decoding
/// predictable across the two drivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnKind {
    Int,
    Uint,
    Float,
    Decimal,
    Bool,
    Str,
    Bytes,
    Json,
    Date,
    Time,
    DateTime,
    /// Fallback: select `col::text` and decode as a string.
    Text,
}

impl ColumnKind {

}

/// A single database cell, kept in a form that can produce both a faithful SQL
/// literal and a display string.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Decimal(String),
    Str(String),
    Bytes(Vec<u8>),
    Json(serde_json::Value),
    Date(String),
    Time(String),
    DateTime(String),
}

impl Value {
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Uint(_) => "uint",
            Value::Float(_) => "float",
            Value::Decimal(_) => "decimal",
            Value::Str(_) => "str",
            Value::Bytes(_) => "bytes",
            Value::Json(_) => "json",
            Value::Date(_) => "date",
            Value::Time(_) => "time",
            Value::DateTime(_) => "datetime",
        }
    }

    /// Human-readable form shipped to the frontend grid.
    pub fn display(&self) -> Option<String> {
        Some(match self {
            Value::Null => return None,
            Value::Bool(v) => if *v { "1" } else { "0" }.to_string(),
            Value::Int(v) => v.to_string(),
            Value::Uint(v) => v.to_string(),
            Value::Float(v) => format_float(*v),
            Value::Decimal(v) => v.clone(),
            Value::Str(v) => v.clone(),
            Value::Bytes(v) => format_bytes(v),
            Value::Json(v) => serde_json::to_string(v).unwrap_or_else(|_| "null".into()),
            Value::Date(v) | Value::Time(v) | Value::DateTime(v) => v.clone(),
        })
    }

    pub fn to_cell(&self) -> Cell {
        Cell {
            kind: self.kind().to_string(),
            text: self.display(),
        }
    }

    /// A dialect-correct SQL literal. `target_is_json` lets a text value be
    /// cast when the destination column is a JSON column.
    pub fn literal(&self, dialect: Dialect, json_column: bool) -> String {
        match self {
            Value::Null => "NULL".to_string(),
            Value::Bool(v) => dialect.bool_literal(*v).to_string(),
            Value::Int(v) => v.to_string(),
            Value::Uint(v) => v.to_string(),
            Value::Float(v) => format_float(*v),
            Value::Decimal(v) => v.clone(),
            Value::Date(v) | Value::Time(v) | Value::DateTime(v) => dialect.string_literal(v),
            Value::Str(v) => {
                let lit = dialect.string_literal(v);
                if json_column {
                    match dialect {
                        Dialect::Mysql => format!("CAST({lit} AS JSON)"),
                        Dialect::Postgres => format!("{lit}::jsonb"),
                    }
                } else {
                    lit
                }
            }
            Value::Bytes(v) => dialect.bytes_literal(v),
            Value::Json(v) => {
                let text = serde_json::to_string(v).unwrap_or_else(|_| "null".into());
                let lit = dialect.string_literal(&text);
                match dialect {
                    Dialect::Mysql => format!("CAST({lit} AS JSON)"),
                    Dialect::Postgres => format!("{lit}::jsonb"),
                }
            }
        }
    }

    /// Stable identity used for row matching and change detection.
    ///
    /// Numeric values of different concrete kinds collapse onto one another so
    /// that e.g. MySQL `INT 1` and PostgreSQL `NUMERIC 1.0` compare equal.
    pub fn canonical(&self) -> String {
        match self {
            Value::Null => "N".to_string(),
            Value::Bool(v) => format!("B{}", if *v { 1 } else { 0 }),
            Value::Int(v) => format!("D{}", normalize_numeric(&v.to_string())),
            Value::Uint(v) => format!("D{}", normalize_numeric(&v.to_string())),
            Value::Float(v) => {
                if v.fract() == 0.0 && v.abs() < 9.0e15 {
                    format!("D{}", normalize_numeric(&format!("{v:.0}")))
                } else {
                    format!("D{}", normalize_numeric(&format!("{v}")))
                }
            }
            Value::Decimal(v) => format!("D{}", normalize_numeric(v)),
            Value::Str(v) => format!("S{v}"),
            Value::Date(v) | Value::Time(v) | Value::DateTime(v) => format!("S{v}"),
            Value::Bytes(v) => format!("X{}", hex::encode(v)),
            Value::Json(v) => format!("J{}", canonical_json(v)),
        }
    }
}

fn format_float(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if v.fract() == 0.0 && v.abs() < 9.0e15 {
        format!("{v:.1}")
    } else {
        let mut s = format!("{v}");
        if s.contains('e') || s.contains('E') {
            s = format!("{v:.17}");
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        s
    }
}

fn format_bytes(bytes: &[u8]) -> String {
    const PREVIEW: usize = 64;
    let hexed: String = hex::encode(&bytes[..bytes.len().min(PREVIEW)]);
    if bytes.len() > PREVIEW {
        format!("0x{hexed}… ({} bytes)", bytes.len())
    } else {
        format!("0x{hexed}")
    }
}

/// `1.500` -> `1.5`, `007` -> `7`, `1.0` -> `1`.
pub fn normalize_numeric(raw: &str) -> String {
    let s = raw.trim();
    if let Ok(dec) = s.parse::<BigDecimal>() {
        return dec.normalized().to_string();
    }
    s.to_string()
}

fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted: BTreeMap<&String, String> = BTreeMap::new();
            for (k, v) in map {
                sorted.insert(k, canonical_json(v));
            }
            let inner: Vec<String> = sorted.iter().map(|(k, v)| format!("{k:?}:{v}")).collect();
            format!("{{{}}}", inner.join(","))
        }
        serde_json::Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

/// Serialisable cell handed to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
    pub kind: String,
    pub text: Option<String>,
}

impl Cell {
    pub fn null() -> Cell {
        Cell {
            kind: "null".into(),
            text: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMeta {
    pub name: String,
    pub data_type: String,
    pub kind: ColumnKind,
    pub nullable: bool,
    pub primary_key: bool,
    /// Generated columns are computed by the server and cannot be edited.
    pub generated: bool,
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

pub fn decode_mysql_row(row: &MySqlRow, kinds: &[ColumnKind]) -> AppResult<Vec<Value>> {
    let mut out = Vec::with_capacity(row.len());
    for idx in 0..row.len() {
        // Callers always supply an explicit kind per selected column; a short
        // slice means "unknown", which decodes as text rather than as hex bytes.
        let kind = kinds.get(idx).copied().unwrap_or(ColumnKind::Text);
        let raw = row.try_get_raw(idx).map_err(AppError::from)?;
        if raw.is_null() {
            out.push(Value::Null);
            continue;
        }
        out.push(decode_mysql_cell(row, idx, kind).map_err(|e| {
            AppError::msg(format!(
                "解析列 `{}` ({}) 失败: {e}",
                row.column(idx).name(),
                row.column(idx).type_info().name()
            ))
        })?);
    }
    Ok(out)
}

/// Decode a MySQL value as text.
///
/// MySQL flags a number of `information_schema` text columns as `BINARY`, and
/// sqlx refuses a `String` decode for those (`str::compatible` rejects the
/// binary flag). Since the caller has already declared the column textual, the
/// bytes are validated as UTF-8 here instead of trusting the charset flag.
fn mysql_text(row: &MySqlRow, idx: usize) -> Result<Value, sqlx::Error> {
    if let Ok(value) = row.try_get::<String, _>(idx) {
        return Ok(Value::Str(value));
    }
    let bytes = row.try_get::<Vec<u8>, _>(idx)?;
    Ok(match String::from_utf8(bytes) {
        Ok(text) => Value::Str(text),
        Err(err) => Value::Bytes(err.into_bytes()),
    })
}

fn pg_text(row: &PgRow, idx: usize) -> Result<Value, sqlx::Error> {
    if let Ok(value) = row.try_get::<String, _>(idx) {
        return Ok(Value::Str(value));
    }
    let bytes = row.try_get::<Vec<u8>, _>(idx)?;
    Ok(match String::from_utf8(bytes) {
        Ok(text) => Value::Str(text),
        Err(err) => Value::Bytes(err.into_bytes()),
    })
}

fn decode_mysql_cell(row: &MySqlRow, idx: usize, kind: ColumnKind) -> AppResult<Value> {
    let value = match kind {
        ColumnKind::Int => row
            .try_get::<i64, _>(idx)
            .map(Value::Int)
            .or_else(|_| row.try_get::<u64, _>(idx).map(Value::Uint))
            .or_else(|_| row.try_get::<f64, _>(idx).map(Value::Float))
            .or_else(|_| mysql_text(row, idx))?,
        ColumnKind::Uint => row
            .try_get::<u64, _>(idx)
            .map(Value::Uint)
            .or_else(|_| row.try_get::<i64, _>(idx).map(Value::Int))
            .or_else(|_| mysql_text(row, idx))?,
        ColumnKind::Float => row
            .try_get::<f64, _>(idx)
            .map(Value::Float)
            .or_else(|_| row.try_get::<f32, _>(idx).map(|v| Value::Float(v as f64)))
            .or_else(|_| mysql_text(row, idx))?,
        ColumnKind::Decimal => row
            .try_get::<BigDecimal, _>(idx)
            .map(|v| Value::Decimal(v.normalized().to_string()))
            .or_else(|_| mysql_text(row, idx))?,
        ColumnKind::Bool => row
            .try_get::<bool, _>(idx)
            .map(Value::Bool)
            .or_else(|_| row.try_get::<i64, _>(idx).map(|v| Value::Bool(v != 0)))?,
        ColumnKind::Date => match row.try_get::<NaiveDate, _>(idx) {
            Ok(v) => Value::Date(v.format("%Y-%m-%d").to_string()),
            Err(_) => match mysql_text(row, idx)? {
                Value::Str(text) => Value::Date(text),
                other => other,
            },
        },
        ColumnKind::Time => match row.try_get::<NaiveTime, _>(idx) {
            Ok(v) => Value::Time(v.format("%H:%M:%S%.f").to_string()),
            Err(_) => match mysql_text(row, idx)? {
                Value::Str(text) => Value::Time(text),
                other => other,
            },
        },
        ColumnKind::DateTime => match row.try_get::<NaiveDateTime, _>(idx) {
            Ok(v) => Value::DateTime(v.format("%Y-%m-%d %H:%M:%S%.f").to_string()),
            Err(_) => match row.try_get::<DateTime<Utc>, _>(idx) {
                Ok(v) => Value::DateTime(v.format("%Y-%m-%d %H:%M:%S%.f").to_string()),
                Err(_) => match mysql_text(row, idx)? {
                    Value::Str(text) => Value::DateTime(text),
                    other => other,
                },
            },
        },
        ColumnKind::Json => row
            .try_get::<serde_json::Value, _>(idx)
            .map(Value::Json)
            .or_else(|_| mysql_text(row, idx))?,
        ColumnKind::Bytes => row
            .try_get::<Vec<u8>, _>(idx)
            .map(Value::Bytes)
            .or_else(|_| row.try_get::<u64, _>(idx).map(Value::Uint))
            .or_else(|_| row.try_get::<i64, _>(idx).map(Value::Int))
            .or_else(|_| mysql_text(row, idx))?,
        ColumnKind::Str | ColumnKind::Text => mysql_text(row, idx)?,
    };
    Ok(value)
}

pub fn decode_pg_row(row: &PgRow, kinds: &[ColumnKind]) -> AppResult<Vec<Value>> {
    let mut out = Vec::with_capacity(row.len());
    for idx in 0..row.len() {
        // Callers always supply an explicit kind per selected column; a short
        // slice means "unknown", which decodes as text rather than as hex bytes.
        let kind = kinds.get(idx).copied().unwrap_or(ColumnKind::Text);
        let raw = row.try_get_raw(idx).map_err(AppError::from)?;
        if raw.is_null() {
            out.push(Value::Null);
            continue;
        }
        out.push(decode_pg_cell(row, idx, kind).map_err(|e| {
            AppError::msg(format!(
                "解析列 \"{}\" ({}) 失败: {e}",
                row.column(idx).name(),
                row.column(idx).type_info().name()
            ))
        })?);
    }
    Ok(out)
}

fn decode_pg_cell(row: &PgRow, idx: usize, kind: ColumnKind) -> AppResult<Value> {
    let value = match kind {
        ColumnKind::Int => row
            .try_get::<i64, _>(idx)
            .map(Value::Int)
            .or_else(|_| row.try_get::<i32, _>(idx).map(|v| Value::Int(v as i64)))
            .or_else(|_| row.try_get::<i16, _>(idx).map(|v| Value::Int(v as i64)))
            .or_else(|_| {
                row.try_get::<BigDecimal, _>(idx)
                    .map(|v| Value::Decimal(v.normalized().to_string()))
            })
            .or_else(|_| pg_text(row, idx))?,
        ColumnKind::Uint => row
            .try_get::<i64, _>(idx)
            .map(Value::Int)
            .or_else(|_| {
                row.try_get::<BigDecimal, _>(idx)
                    .map(|v| Value::Decimal(v.normalized().to_string()))
            })
            .or_else(|_| pg_text(row, idx))?,
        ColumnKind::Float => row
            .try_get::<f64, _>(idx)
            .map(Value::Float)
            .or_else(|_| row.try_get::<f32, _>(idx).map(|v| Value::Float(v as f64)))
            .or_else(|_| pg_text(row, idx))?,
        ColumnKind::Decimal => row
            .try_get::<BigDecimal, _>(idx)
            .map(|v| Value::Decimal(v.normalized().to_string()))
            .or_else(|_| pg_text(row, idx))?,
        ColumnKind::Bool => match row.try_get::<bool, _>(idx) {
            Ok(v) => Value::Bool(v),
            Err(_) => match pg_text(row, idx)? {
                Value::Str(text) => Value::Bool(matches!(text.as_str(), "1" | "t" | "true")),
                other => other,
            },
        },
        ColumnKind::Date => match row.try_get::<NaiveDate, _>(idx) {
            Ok(v) => Value::Date(v.format("%Y-%m-%d").to_string()),
            Err(_) => match pg_text(row, idx)? {
                Value::Str(text) => Value::Date(text),
                other => other,
            },
        },
        ColumnKind::Time => match row.try_get::<NaiveTime, _>(idx) {
            Ok(v) => Value::Time(v.format("%H:%M:%S%.f").to_string()),
            Err(_) => match pg_text(row, idx)? {
                Value::Str(text) => Value::Time(text),
                other => other,
            },
        },
        ColumnKind::DateTime => match row.try_get::<NaiveDateTime, _>(idx) {
            Ok(v) => Value::DateTime(v.format("%Y-%m-%d %H:%M:%S%.f").to_string()),
            Err(_) => match row.try_get::<DateTime<Utc>, _>(idx) {
                Ok(v) => Value::DateTime(v.format("%Y-%m-%d %H:%M:%S%.f").to_string()),
                Err(_) => match pg_text(row, idx)? {
                    Value::Str(text) => Value::DateTime(text),
                    other => other,
                },
            },
        },
        ColumnKind::Json => row
            .try_get::<serde_json::Value, _>(idx)
            .map(Value::Json)
            .or_else(|_| pg_text(row, idx))?,
        // PostgreSQL has no unsigned 64-bit integer, hence no `u64` step here.
        ColumnKind::Bytes => row
            .try_get::<Vec<u8>, _>(idx)
            .map(Value::Bytes)
            .or_else(|_| row.try_get::<i64, _>(idx).map(Value::Int))
            .or_else(|_| pg_text(row, idx))?,
        ColumnKind::Str | ColumnKind::Text => pg_text(row, idx)?,
    };
    Ok(value)
}
