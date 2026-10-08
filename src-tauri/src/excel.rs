//! Spreadsheet reading for the Excel import flow.
//!
//! Parsing lives on this side rather than in the webview: `calamine` is a pure
//! Rust reader for xlsx/xls/xlsb/ods, so nothing large is shipped to the
//! frontend and the npm `xlsx` package (unmaintained, with open advisories) is
//! not needed.

use calamine::{open_workbook_auto, Data, Reader};
use serde::Serialize;

use crate::error::{AppError, AppResult};

/// How many raw rows are returned so the UI can offer them as header choices.
const HEAD_CHOICES: usize = 12;
/// Cap on values kept from a single cell; spreadsheets occasionally carry
/// enormous blobs and there is no reason to move them around.
const MAX_CELL_CHARS: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcelSheetPreview {
    pub sheets: Vec<String>,
    pub sheet: String,
    /// 0-based index of the row used as the header, or `None` for no header.
    pub header_row: Option<usize>,
    /// Column names, either from the header row or generated as 列1, 列2 …
    pub headers: Vec<String>,
    /// Data rows after the header, limited by the caller.
    pub rows: Vec<Vec<String>>,
    /// Number of data rows below the header.
    pub total_rows: usize,
    /// True when `rows` holds fewer entries than `total_rows`.
    pub truncated: bool,
    /// The first few raw rows, so the user can pick a different header row
    /// without another round trip.
    pub head: Vec<Vec<String>>,
}

/// A sheet read into strings, with `None` for empty cells.
#[derive(Debug, Clone, Default)]
pub struct ExcelSheet {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub total_rows: usize,
}

fn format_number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.007_199_254_740_992e15 {
        format!("{}", value as i64)
    } else {
        // Rust's shortest round-trip formatting avoids 0.30000000000000004.
        format!("{value}")
    }
}

/// One cell as text. `None` means the cell is empty and therefore NULL.
pub fn cell_to_text(cell: &Data) -> Option<String> {
    let text = match cell {
        Data::Empty => return None,
        Data::String(value) => value.trim().to_string(),
        Data::Float(value) => format_number(*value),
        Data::Int(value) => value.to_string(),
        Data::Bool(value) => {
            if *value {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Data::DateTime(value) => match value.as_datetime() {
            Some(stamp) => {
                // A pure date reads better without a midnight time part.
                if stamp.time() == chrono::NaiveTime::MIN {
                    stamp.format("%Y-%m-%d").to_string()
                } else {
                    stamp.format("%Y-%m-%d %H:%M:%S").to_string()
                }
            }
            None => return None,
        },
        Data::DateTimeIso(value) => value.trim().to_string(),
        Data::DurationIso(value) => value.trim().to_string(),
        // A broken formula should reach the user as a rejected value rather
        // than as a silently empty cell.
        Data::Error(error) => format!("#ERROR({error:?})"),
    };
    if text.is_empty() {
        return None;
    }
    Some(if text.chars().count() > MAX_CELL_CHARS {
        text.chars().take(MAX_CELL_CHARS).collect()
    } else {
        text
    })
}

/// Column name for a header cell, falling back to a positional name.
fn header_name(cell: Option<&Data>, index: usize) -> String {
    let text = cell.and_then(cell_to_text).unwrap_or_default();
    let text = text.trim();
    if text.is_empty() {
        format!("列{}", index + 1)
    } else {
        text.to_string()
    }
}

/// Read a sheet into strings.
///
/// `header_row` is the 0-based row used for column names; `None` means the sheet
/// has no header and the columns are named 列1, 列2 ….
pub fn read_sheet(
    path: &str,
    sheet: Option<&str>,
    header_row: Option<usize>,
    limit: Option<usize>,
) -> AppResult<(ExcelSheet, Vec<String>, String)> {
    let mut workbook = open_workbook_auto(path)
        .map_err(|error| AppError::msg(format!("无法打开 Excel 文件：{error}")))?;
    let sheets: Vec<String> = workbook.sheet_names().to_vec();
    if sheets.is_empty() {
        return Err(AppError::msg("该文件不包含任何工作表"));
    }
    let chosen = match sheet {
        Some(name) if sheets.iter().any(|item| item == name) => name.to_string(),
        Some(name) => {
            return Err(AppError::msg(format!(
                "工作表 {name} 不存在，可用：{}",
                sheets.join("、")
            )))
        }
        None => sheets[0].clone(),
    };

    let range = workbook
        .worksheet_range(&chosen)
        .map_err(|error| AppError::msg(format!("无法读取工作表 {chosen}：{error}")))?;

    let mut raw: Vec<Vec<Option<String>>> = range
        .rows()
        .map(|row| row.iter().map(cell_to_text).collect())
        .collect();
    // Trailing blank rows are extremely common in hand-edited files.
    while raw
        .last()
        .is_some_and(|row| row.iter().all(|cell| cell.is_none()))
    {
        raw.pop();
    }

    let (headers, data) = match header_row {
        Some(index) => {
            if index >= raw.len() {
                return Err(AppError::msg(format!(
                    "表头行 {} 超出范围（该工作表共 {} 行）",
                    index + 1,
                    raw.len()
                )));
            }
            let header_cells = &range
                .rows()
                .nth(index)
                .map(|row| row.to_vec())
                .unwrap_or_default();
            let width = raw
                .iter()
                .map(|row| row.len())
                .max()
                .unwrap_or(0)
                .max(header_cells.len());
            let headers = (0..width)
                .map(|column| header_name(header_cells.get(column), column))
                .collect::<Vec<_>>();
            (headers, raw[index + 1..].to_vec())
        }
        None => {
            let width = raw.iter().map(|row| row.len()).max().unwrap_or(0);
            let headers = (0..width).map(|column| format!("列{}", column + 1)).collect();
            (headers, raw)
        }
    };

    let total_rows = data.len();
    let rows = match limit {
        Some(max) => data.into_iter().take(max).collect(),
        None => data,
    };

    Ok((
        ExcelSheet {
            headers,
            rows,
            total_rows,
        },
        sheets,
        chosen,
    ))
}

/// Preview a sheet: column names plus a sample of rows.
pub fn preview(
    path: &str,
    sheet: Option<&str>,
    header_row: Option<usize>,
    sample: usize,
) -> AppResult<ExcelSheetPreview> {
    let (parsed, sheets, chosen) = read_sheet(path, sheet, header_row, Some(sample.max(1)))?;
    // The raw head is read separately so it is unaffected by the header choice.
    let head = read_head(path, &chosen, HEAD_CHOICES).unwrap_or_default();
    Ok(ExcelSheetPreview {
        sheets,
        sheet: chosen,
        header_row,
        headers: parsed.headers,
        truncated: parsed.rows.len() < parsed.total_rows,
        total_rows: parsed.total_rows,
        rows: parsed
            .rows
            .into_iter()
            .map(|row| row.into_iter().map(|cell| cell.unwrap_or_default()).collect())
            .collect(),
        head,
    })
}

/// The first `count` rows of a sheet, verbatim.
fn read_head(path: &str, sheet: &str, count: usize) -> AppResult<Vec<Vec<String>>> {
    let mut workbook = open_workbook_auto(path)
        .map_err(|error| AppError::msg(format!("无法打开 Excel 文件：{error}")))?;
    let range = workbook
        .worksheet_range(sheet)
        .map_err(|error| AppError::msg(format!("无法读取工作表 {sheet}：{error}")))?;
    Ok(range
        .rows()
        .take(count)
        .map(|row| {
            row.iter()
                .map(|cell| cell_to_text(cell).unwrap_or_default())
                .collect()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/import.xlsx")
            .to_string_lossy()
            .to_string()
    }

    #[test]
    fn a_workbook_reports_its_sheets() {
        let (_, sheets, chosen) = read_sheet(&fixture(), None, Some(0), Some(5)).unwrap();
        assert_eq!(sheets, vec!["数据", "第二表"]);
        assert_eq!(chosen, "数据", "未指定时使用第一张表");
    }

    #[test]
    fn headers_and_rows_are_read_in_order() {
        let (sheet, _, _) = read_sheet(&fixture(), Some("数据"), Some(0), None).unwrap();
        assert_eq!(
            sheet.headers,
            vec!["id", "name", "amount", "ratio", "active", "created_at", "payload", "列8"],
            "空表头应回退为位置名"
        );
        assert_eq!(sheet.total_rows, 4, "表头之外应有 4 行数据");
        assert_eq!(sheet.rows.len(), 4);
        // Row width follows the header, so a short row pads with None.
        assert_eq!(sheet.rows[0].len(), 8);
    }

    #[test]
    fn cell_types_are_normalised_to_the_text_a_user_expects() {
        let (sheet, _, _) = read_sheet(&fixture(), Some("数据"), Some(0), None).unwrap();
        let first = &sheet.rows[0];
        assert_eq!(first[0].as_deref(), Some("1"), "整数不应变成 1.0");
        assert_eq!(first[2].as_deref(), Some("10.5"));
        assert_eq!(first[3].as_deref(), Some("0.25"), "小数不应有浮点噪声");
        assert_eq!(
            first[4].as_deref(),
            Some("true"),
            "布尔值统一为 true/false"
        );
        // A real Excel date cell carries a serial number; returning 45356 would
        // silently corrupt every date import.
        assert_eq!(
            first[5].as_deref(),
            Some("2024-03-05"),
            "日期序列号必须还原成日期"
        );
        assert_eq!(first[6].as_deref(), Some("{\"a\": 1}"));
        assert_eq!(first[7].as_deref(), Some("第一行"));
    }

    #[test]
    fn whole_floats_lose_their_trailing_zero() {
        let (sheet, _, _) = read_sheet(&fixture(), Some("数据"), Some(0), None).unwrap();
        // 20 is stored as a float cell but must read as "20".
        assert_eq!(sheet.rows[1][2].as_deref(), Some("20"));
        // "1.50" keeps its meaning without the padding zero.
        assert_eq!(sheet.rows[1][3].as_deref(), Some("1.5"));
    }

    #[test]
    fn empty_cells_become_none_rather_than_empty_strings() {
        let (sheet, _, _) = read_sheet(&fixture(), Some("数据"), Some(0), None).unwrap();
        let third = &sheet.rows[2];
        assert_eq!(third[2], None);
        assert_eq!(third[3], None);
        assert_eq!(third[4], None);
        assert_eq!(third[5], None);
        // A short trailing row is padded, not truncated.
        assert_eq!(sheet.rows[1][7], None);
    }

    #[test]
    fn a_sheet_without_a_header_gets_positional_names() {
        let (sheet, _, _) = read_sheet(&fixture(), Some("数据"), None, None).unwrap();
        assert_eq!(sheet.headers[0], "列1");
        assert_eq!(sheet.headers[7], "列8");
        assert_eq!(sheet.total_rows, 5, "没有表头时第一行也是数据");
    }

    #[test]
    fn a_different_header_row_is_honoured() {
        // Row 2 holds the real names in this fixture, so choosing it must drop
        // the first data row from the output.
        let (sheet, _, _) = read_sheet(&fixture(), Some("数据"), Some(1), None).unwrap();
        assert_eq!(sheet.headers[1], "苹果");
        assert_eq!(sheet.total_rows, 3);
    }

    #[test]
    fn trailing_blank_rows_are_dropped() {
        // Sheet 2 has only two meaningful rows; the reader must not report the
        // padding that spreadsheet editors leave behind.
        let (sheet, _, _) = read_sheet(&fixture(), Some("第二表"), Some(0), None).unwrap();
        assert_eq!(sheet.total_rows, 2);
    }

    #[test]
    fn limits_and_unknown_sheets_are_handled() {
        let (sheet, _, _) = read_sheet(&fixture(), Some("数据"), Some(0), Some(2)).unwrap();
        assert_eq!(sheet.rows.len(), 2);
        assert_eq!(sheet.total_rows, 4, "total_rows 反映整张表");

        let error = read_sheet(&fixture(), Some("不存在"), Some(0), None).unwrap_err();
        assert!(error.to_string().contains("不存在"), "{error}");

        let error = read_sheet(&fixture(), Some("数据"), Some(99), None).unwrap_err();
        assert!(error.to_string().contains("超出范围"), "{error}");

        let error = read_sheet("/definitely/missing.xlsx", None, None, None).unwrap_err();
        assert!(error.to_string().contains("无法打开"), "{error}");
    }

    #[test]
    fn preview_reports_the_head_so_a_header_row_can_be_chosen() {
        let preview = preview(&fixture(), Some("数据"), Some(0), 30).unwrap();
        assert_eq!(preview.sheets.len(), 2);
        assert_eq!(preview.headers[1], "name");
        assert_eq!(preview.rows.len(), 4);
        assert!(!preview.truncated);
        // Raw rows let the UI offer header choices without another round trip.
        assert_eq!(preview.head.len(), 5);
        assert_eq!(preview.head[1][1], "苹果");
    }
}
