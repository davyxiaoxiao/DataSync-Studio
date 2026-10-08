use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    Mysql,
    Postgres,
}

impl Dialect {
    pub fn label(self) -> &'static str {
        match self {
            Dialect::Mysql => "MySQL",
            Dialect::Postgres => "PostgreSQL",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Dialect::Mysql => 3306,
            Dialect::Postgres => 5432,
        }
    }

    /// Identifier quoting. MySQL uses backticks, PostgreSQL double quotes.
    pub fn quote(self, ident: &str) -> String {
        match self {
            Dialect::Mysql => format!("`{}`", ident.replace('`', "``")),
            Dialect::Postgres => format!("\"{}\"", ident.replace('"', "\"\"")),
        }
    }

    pub fn qualified(self, namespace: Option<&str>, table: &str) -> String {
        match namespace {
            Some(ns) if !ns.is_empty() => format!("{}.{}", self.quote(ns), self.quote(table)),
            _ => self.quote(table),
        }
    }

    /// Render a Rust string as a SQL string literal for this dialect.
    pub fn string_literal(self, value: &str) -> String {
        match self {
            // MySQL treats backslash as an escape character by default.
            Dialect::Mysql => {
                let mut out = String::with_capacity(value.len() + 2);
                out.push('\'');
                for ch in value.chars() {
                    match ch {
                        '\'' => out.push_str("''"),
                        '\\' => out.push_str("\\\\"),
                        '\0' => out.push_str("\\0"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\u{1a}' => out.push_str("\\Z"),
                        _ => out.push(ch),
                    }
                }
                out.push('\'');
                out
            }
            // PostgreSQL: standard_conforming_strings is on by default, so a
            // doubled single quote is the only escape required. NUL bytes are
            // not representable in a text literal, so use the bytea form.
            Dialect::Postgres => {
                if value.contains('\0') {
                    return format!("convert_from(decode('{}', 'hex'), 'UTF8')", hex::encode(value));
                }
                format!("'{}'", value.replace('\'', "''"))
            }
        }
    }

    pub fn bytes_literal(self, bytes: &[u8]) -> String {
        match self {
            Dialect::Mysql => format!("X'{}'", hex::encode(bytes)),
            Dialect::Postgres => format!("'\\x{}'::bytea", hex::encode(bytes)),
        }
    }

    /// NULL-safe equality: `a <=> b` on MySQL, `a IS NOT DISTINCT FROM b` on
    /// PostgreSQL. Used to match a row without a plain `= NULL` silently
    /// matching nothing.
    pub fn not_distinct_from(self) -> &'static str {
        match self {
            Dialect::Mysql => "<=>",
            Dialect::Postgres => "IS NOT DISTINCT FROM",
        }
    }

    pub fn bool_literal(self, value: bool) -> &'static str {
        match self {
            Dialect::Mysql => {
                if value {
                    "1"
                } else {
                    "0"
                }
            }
            Dialect::Postgres => {
                if value {
                    "TRUE"
                } else {
                    "FALSE"
                }
            }
        }
    }

    /// `CREATE TABLE ... ` tail for the table-level options.
    pub fn table_options(self, engine: Option<&str>, charset: Option<&str>, collation: Option<&str>) -> String {
        match self {
            Dialect::Mysql => {
                let mut parts = Vec::new();
                parts.push(format!("ENGINE={}", engine.unwrap_or("InnoDB")));
                if let Some(cs) = charset {
                    parts.push(format!("DEFAULT CHARSET={cs}"));
                }
                if let Some(coll) = collation {
                    parts.push(format!("COLLATE={coll}"));
                }
                parts.join(" ")
            }
            Dialect::Postgres => String::new(),
        }
    }

}
