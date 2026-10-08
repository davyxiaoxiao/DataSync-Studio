//! Turning spreadsheet text into SQL literals for a target column.
//!
//! Spreadsheets carry everything as text once parsed, so every value has to be
//! validated against the column it is destined for. A cell that does not fit is
//! reported with a reason instead of being smuggled in as a quoted string, which
//! would either fail deep inside the server or, worse, store nonsense.

use crate::db::value::ColumnKind;
use crate::dialect::Dialect;
use crate::schema::model::ColumnInfo;

/// Result of preparing one cell: a SQL literal, or why it cannot be used.
pub type CellResult = Result<String, String>;

/// Format an integer, tolerating the `1.0` that spreadsheets produce for whole
/// numbers.
pub fn parse_int(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = trimmed.parse::<i64>() {
        return Some(value);
    }
    let as_float: f64 = trimmed.parse().ok()?;
    (as_float.fract() == 0.0 && as_float.abs() < 9.007_199_254_740_992e15)
        .then_some(as_float as i64)
}

/// Whether the text is a plain SQL numeric literal.
///
/// Used for `DECIMAL`/`NUMERIC`, where the value must be emitted unquoted to
/// keep full precision, so the shape has to be checked rather than trusted.
pub fn is_numeric_literal(text: &str) -> bool {
    let trimmed = text.trim();
    let mut chars = trimmed.chars().peekable();
    if matches!(chars.peek(), Some('+') | Some('-')) {
        chars.next();
    }
    let mut digits = 0usize;
    let mut dot = false;
    let mut exponent = false;
    let mut exponent_digits = 0usize;
    // Set right after the exponent marker, so `-2.5E-3` is accepted while a
    // stray sign anywhere else is not.
    let mut sign_allowed = false;
    for ch in chars {
        match ch {
            '0'..='9' => {
                if exponent {
                    exponent_digits += 1;
                } else {
                    digits += 1;
                }
                sign_allowed = false;
            }
            '.' if !dot && !exponent => {
                dot = true;
                sign_allowed = false;
            }
            'e' | 'E' if !exponent && digits > 0 => {
                exponent = true;
                sign_allowed = true;
            }
            '+' | '-' if sign_allowed => sign_allowed = false,
            _ => return false,
        }
    }
    digits > 0 && (!exponent || exponent_digits > 0)
}

/// Recognise the spellings people actually use for booleans.
pub fn parse_bool(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "1" | "yes" | "y" | "是" | "真" | "on" => Some(true),
        "false" | "f" | "0" | "no" | "n" | "否" | "假" | "off" => Some(false),
        _ => None,
    }
}

const DATE_FORMATS: &[&str] = &["%Y-%m-%d", "%Y/%m/%d", "%Y.%m.%d", "%Y%m%d"];

const DATETIME_FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S",
    "%Y/%m/%d %H:%M:%S",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%d %H:%M",
    "%Y/%m/%d %H:%M",
    "%Y-%m-%dT%H:%M",
];

const TIME_FORMATS: &[&str] = &["%H:%M:%S", "%H:%M"];

/// Normalise a date to `YYYY-MM-DD`.
pub fn normalize_date(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if let Ok(stamp) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(stamp.date().format("%Y-%m-%d").to_string());
    }
    if let Ok(stamp) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Some(stamp.date_naive().format("%Y-%m-%d").to_string());
    }
    for format in DATE_FORMATS {
        if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, format) {
            return Some(date.format("%Y-%m-%d").to_string());
        }
    }
    for format in DATETIME_FORMATS {
        if let Ok(stamp) = chrono::NaiveDateTime::parse_from_str(trimmed, format) {
            return Some(stamp.date().format("%Y-%m-%d").to_string());
        }
    }
    None
}

/// Normalise a timestamp to `YYYY-MM-DD HH:MM:SS`.
pub fn normalize_datetime(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if let Ok(stamp) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Some(
            stamp
                .naive_local()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        );
    }
    for format in DATETIME_FORMATS {
        if let Ok(stamp) = chrono::NaiveDateTime::parse_from_str(trimmed, format) {
            return Some(stamp.format("%Y-%m-%d %H:%M:%S").to_string());
        }
    }
    // A bare date is a legitimate timestamp at midnight.
    for format in DATE_FORMATS {
        if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, format) {
            return Some(date.format("%Y-%m-%d 00:00:00").to_string());
        }
    }
    None
}

/// Normalise a time to `HH:MM:SS`.
pub fn normalize_time(text: &str) -> Option<String> {
    let trimmed = text.trim();
    for format in TIME_FORMATS {
        if let Ok(time) = chrono::NaiveTime::parse_from_str(trimmed, format) {
            return Some(time.format("%H:%M:%S").to_string());
        }
    }
    // A full timestamp is also acceptable for a TIME column.
    for format in DATETIME_FORMATS {
        if let Ok(stamp) = chrono::NaiveDateTime::parse_from_str(trimmed, format) {
            return Some(stamp.time().format("%H:%M:%S").to_string());
        }
    }
    None
}

/// Turn one cell into a literal for `column`.
///
/// `raw` is the cell text; an empty cell becomes `NULL`.
pub fn cell_literal(
    raw: &str,
    column: &ColumnInfo,
    kind: ColumnKind,
    dialect: Dialect,
) -> CellResult {
    let text = raw.trim();
    if text.is_empty() {
        if column.nullable {
            return Ok("NULL".to_string());
        }
        return Err(format!(
            "列 {} 不允许为空；不希望导入该列时请取消它的映射，让数据库使用默认值",
            column.name
        ));
    }

    match kind {
        ColumnKind::Int | ColumnKind::Uint => parse_int(text)
            .map(|value| value.to_string())
            .ok_or_else(|| format!("{text:?} 不是整数")),
        ColumnKind::Float | ColumnKind::Decimal => {
            if is_numeric_literal(text) {
                Ok(text.to_string())
            } else {
                Err(format!("{text:?} 不是有效数字"))
            }
        }
        ColumnKind::Bool => parse_bool(text)
            .map(|value| dialect.bool_literal(value).to_string())
            .ok_or_else(|| format!("{text:?} 无法识别为布尔值")),
        ColumnKind::Date => normalize_date(text)
            .map(|value| dialect.string_literal(&value))
            .ok_or_else(|| format!("{text:?} 无法识别为日期")),
        ColumnKind::DateTime => normalize_datetime(text)
            .map(|value| dialect.string_literal(&value))
            .ok_or_else(|| format!("{text:?} 无法识别为日期时间")),
        ColumnKind::Time => normalize_time(text)
            .map(|value| dialect.string_literal(&value))
            .ok_or_else(|| format!("{text:?} 无法识别为时间")),
        ColumnKind::Json => {
            // Validate so a malformed cell is reported instead of rejected by
            // the server mid-batch.
            serde_json::from_str::<serde_json::Value>(text)
                .map(|_| dialect.string_literal(text))
                .map_err(|error| format!("{text:?} 不是有效 JSON：{error}"))
        }
        ColumnKind::Bytes => Err(format!(
            "列 {} 是二进制类型，暂不支持从 Excel 导入",
            column.name
        )),
        ColumnKind::Str | ColumnKind::Text => Ok(dialect.string_literal(text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::value::ColumnKind;
    use crate::dialect::Dialect;

    fn column(name: &str, data_type: &str, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            ordinal: 1,
            data_type: data_type.into(),
            full_type: data_type.into(),
            nullable,
            default: None,
            is_auto_increment: false,
            auto_kind: None,
            is_generated: false,
            generation_expression: None,
            generation_kind: None,
            is_unsigned: false,
            comment: None,
            charset: None,
            collation: None,
            udt_name: None,
        }
    }

    #[test]
    fn integers_accept_the_float_shape_spreadsheets_produce() {
        assert_eq!(parse_int("42"), Some(42));
        assert_eq!(parse_int(" 42 "), Some(42));
        assert_eq!(parse_int("-7"), Some(-7));
        // A whole number stored as a float arrives as "42.0".
        assert_eq!(parse_int("42.0"), Some(42));
        assert_eq!(parse_int("42.5"), None);
        assert_eq!(parse_int("abc"), None);
        assert_eq!(parse_int(""), None);
    }

    #[test]
    fn numeric_literals_reject_anything_that_is_not_a_number() {
        for good in ["1", "-1", "+1", "1.5", ".5", "1.", "1e10", "-2.5E-3", "0"] {
            assert!(is_numeric_literal(good), "{good} 应被接受");
        }
        for bad in ["", "abc", "1; DROP TABLE t", "1,5", "1.2.3", "e10", "1e", "--1", "1 2"] {
            assert!(!is_numeric_literal(bad), "{bad} 应被拒绝");
        }
    }

    #[test]
    fn booleans_cover_the_common_spellings() {
        for good in ["true", "TRUE", "1", "yes", "是", "Y"] {
            assert_eq!(parse_bool(good), Some(true), "{good}");
        }
        for bad in ["false", "0", "no", "否", "N"] {
            assert_eq!(parse_bool(bad), Some(false), "{bad}");
        }
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn dates_are_normalised_from_the_formats_people_use() {
        assert_eq!(normalize_date("2024-03-05"), Some("2024-03-05".into()));
        assert_eq!(normalize_date("2024/3/5"), Some("2024-03-05".into()));
        assert_eq!(normalize_date("2024-03-05 10:00:00"), Some("2024-03-05".into()));
        assert_eq!(normalize_date("nope"), None);

        assert_eq!(
            normalize_datetime("2024/03/05 10:20:30"),
            Some("2024-03-05 10:20:30".into())
        );
        assert_eq!(
            normalize_datetime("2024-03-05"),
            Some("2024-03-05 00:00:00".into())
        );
        assert_eq!(normalize_datetime("nope"), None);

        assert_eq!(normalize_time("10:20:30"), Some("10:20:30".into()));
        assert_eq!(normalize_time("10:20"), Some("10:20:00".into()));
        assert_eq!(normalize_time("nope"), None);
    }

    #[test]
    fn empty_cells_respect_nullability() {
        let optional = column("note", "varchar", true);
        assert_eq!(
            cell_literal("  ", &optional, ColumnKind::Str, Dialect::Mysql),
            Ok("NULL".into())
        );
        let required = column("code", "varchar", false);
        let error = cell_literal("", &required, ColumnKind::Str, Dialect::Mysql).unwrap_err();
        assert!(error.contains("不允许为空"), "{error}");
        // The message must point at the way out.
        assert!(error.contains("取消它的映射"), "{error}");
    }

    #[test]
    fn values_are_quoted_and_escaped_per_dialect() {
        let text = column("name", "varchar", true);
        // MySQL escapes by doubling the quote as well, which also works under
        // NO_BACKSLASH_ESCAPES.
        assert_eq!(
            cell_literal("O'Brien", &text, ColumnKind::Str, Dialect::Mysql),
            Ok("'O''Brien'".into())
        );
        assert_eq!(
            cell_literal("O'Brien", &text, ColumnKind::Str, Dialect::Postgres),
            Ok("'O''Brien'".into())
        );
        // Numbers are emitted bare so decimal precision survives.
        let amount = column("amount", "decimal", true);
        assert_eq!(
            cell_literal("0.1000000000000000055511151231257827", &amount, ColumnKind::Decimal, Dialect::Mysql),
            Ok("0.1000000000000000055511151231257827".into())
        );
        // Booleans use the engine's own spelling.
        let flag = column("ok", "boolean", true);
        assert_eq!(
            cell_literal("是", &flag, ColumnKind::Bool, Dialect::Postgres),
            Ok("TRUE".into())
        );
        assert_eq!(
            cell_literal("否", &flag, ColumnKind::Bool, Dialect::Mysql),
            Ok("0".into())
        );
    }

    #[test]
    fn malformed_values_are_reported_with_their_column() {
        let number = column("age", "int", true);
        let error = cell_literal("abc", &number, ColumnKind::Int, Dialect::Mysql).unwrap_err();
        assert!(error.contains("不是整数"), "{error}");

        let payload = column("payload", "json", true);
        let error =
            cell_literal("{oops", &payload, ColumnKind::Json, Dialect::Mysql).unwrap_err();
        assert!(error.contains("不是有效 JSON"), "{error}");

        let blob = column("blob", "blob", true);
        let error = cell_literal("x", &blob, ColumnKind::Bytes, Dialect::Mysql).unwrap_err();
        assert!(error.contains("二进制类型"), "{error}");
    }
}
