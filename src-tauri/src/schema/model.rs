use serde::{Deserialize, Serialize};

use crate::db::value::ColumnKind;

/// A column, normalised across MySQL and PostgreSQL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    pub ordinal: i64,
    /// Family name in upper case, e.g. `VARCHAR`, `INT`, `TIMESTAMPTZ`.
    pub data_type: String,
    /// The declared type exactly as the server reports it.
    pub full_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub is_auto_increment: bool,
    /// `auto_increment` | `identity_always` | `identity_default` | `serial`
    pub auto_kind: Option<String>,
    /// A computed column. Its value is derived and can never be written.
    #[serde(default)]
    pub is_generated: bool,
    /// The generation expression exactly as the server reports it.
    #[serde(default)]
    pub generation_expression: Option<String>,
    /// `stored` (materialised on write) | `virtual` (computed on read).
    #[serde(default)]
    pub generation_kind: Option<String>,
    pub is_unsigned: bool,
    pub comment: Option<String>,
    pub charset: Option<String>,
    pub collation: Option<String>,
}

impl ColumnInfo {
    /// Type text used for change detection: lower case, collapsed whitespace and
    /// with MySQL integer display widths removed (`int(11)` -> `int`).
    pub fn canonical_type(&self) -> String {
        let collapsed = self
            .full_type
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        strip_int_display_width(&collapsed)
    }

    pub fn kind(&self) -> ColumnKind {
        match self.data_type.to_ascii_uppercase().as_str() {
            "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT" | "YEAR" => {
                if self.is_unsigned {
                    ColumnKind::Uint
                } else {
                    ColumnKind::Int
                }
            }
            "BOOLEAN" | "BOOL" => ColumnKind::Bool,
            "FLOAT" | "DOUBLE" | "REAL" | "DOUBLE PRECISION" => ColumnKind::Float,
            "DECIMAL" | "NUMERIC" => ColumnKind::Decimal,
            "DATE" => ColumnKind::Date,
            "TIME" => ColumnKind::Time,
            "DATETIME" | "TIMESTAMP" | "TIMESTAMPTZ" => ColumnKind::DateTime,
            "JSON" | "JSONB" => ColumnKind::Json,
            "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BIT"
            | "BYTEA" | "GEOMETRY" => ColumnKind::Bytes,
            "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM"
            | "SET" | "UUID" | "BPCHAR" | "NAME" | "CITEXT" => ColumnKind::Str,
            _ => ColumnKind::Text,
        }
    }

    /// Whether the column can appear in INSERT / UPDATE statements.
    pub fn is_writable(&self) -> bool {
        !self.is_generated
    }

    /// Whether the value must be selected as text because the driver cannot
    /// decode the type natively.
    ///
    /// These family names are only ever produced by PostgreSQL (`pg_family`),
    /// so probing them is safe for MySQL columns.
    pub fn needs_text_cast(&self) -> bool {
        if self.kind() == ColumnKind::Text {
            return true;
        }
        matches!(
            self.data_type.to_ascii_uppercase().as_str(),
            "UUID"
                | "BIT"
                | "VARBIT"
                | "MONEY"
                | "INTERVAL"
                | "INET"
                | "CIDR"
                | "MACADDR"
                | "MACADDR8"
                | "TSVECTOR"
                | "XML"
                | "CITEXT"
                | "NAME"
                | "POINT"
                | "LINE"
                | "LSEG"
                | "BOX"
                | "PATH"
                | "POLYGON"
                | "CIRCLE"
        )
    }
}

fn strip_int_display_width(input: &str) -> String {
    let families = [
        "tinyint", "smallint", "mediumint", "int", "integer", "bigint",
    ];
    for family in families {
        let prefix = format!("{family}(");
        if let Some(rest) = input.strip_prefix(&prefix) {
            if let Some(close) = rest.find(')') {
                let inner = &rest[..close];
                // Only strip pure display widths; `decimal(10,2)` style args stay.
                if inner.chars().all(|c| c.is_ascii_digit()) {
                    return format!("{family}{}", &rest[close + 1..]);
                }
            }
        }
    }
    input.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
    pub index_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableStructure {
    pub name: String,
    pub comment: Option<String>,
    pub engine: Option<String>,
    pub charset: Option<String>,
    pub collation: Option<String>,
    pub primary_key: Vec<String>,
    pub columns: Vec<ColumnInfo>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
}

impl TableStructure {
    pub fn column(&self, name: &str) -> Option<&ColumnInfo> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn index(&self, name: &str) -> Option<&IndexInfo> {
        self.indexes.iter().find(|i| i.name == name)
    }

    pub fn foreign_key(&self, name: &str) -> Option<&ForeignKeyInfo> {
        self.foreign_keys.iter().find(|f| f.name == name)
    }

}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableSummary {
    pub name: String,
    pub table_type: String,
    pub comment: Option<String>,
    pub rows: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamespaceInfo {
    pub name: String,
    pub charset: Option<String>,
    pub collation: Option<String>,
    pub is_system: bool,
}
