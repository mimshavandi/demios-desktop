//! Spreadsheets (XLSX, XLSM, XLSB, XLS, ODS) through calamine: one table per sheet, with
//! typed cells. Dates come from the cells' number formats, in either date system.

use crate::DataError;
use crate::table::{Cell, Limits, Table, build};
use crate::value::{Date, DateOrder, parse_date};
use calamine::{Data, Reader};

fn cell(d: &Data) -> Cell {
    match d {
        Data::Empty => Cell::Empty,
        Data::Int(i) => Cell::Number(*i as f64),
        // Infinity or NaN (`1e400`, `nan`) isn't a value to compute with: it's the
        // spreadsheet's number error, counted with the other error cells.
        Data::Float(f) if !f.is_finite() => Cell::Error("#NUM!".into()),
        Data::Float(f) => Cell::Number(*f),
        Data::String(s) if s.trim().is_empty() => Cell::Empty,
        Data::String(s) => Cell::Text(s.clone()),
        Data::Bool(b) => Cell::Bool(*b),
        Data::DateTime(dt) if dt.is_datetime() => {
            let (y, m, day, ..) = dt.to_ymd_hms_milli();
            Date::new(i32::from(y), u32::from(m), u32::from(day))
                .map(Cell::Date)
                .unwrap_or_else(|| Cell::Number(dt.as_f64()))
        }
        // A duration (e.g. hours worked): its value in days.
        Data::DateTime(dt) => Cell::Number(dt.as_f64()),
        Data::DateTimeIso(s) => parse_date(s, DateOrder::Iso)
            .map(Cell::Date)
            .unwrap_or_else(|| Cell::Text(s.clone())),
        Data::DurationIso(s) => Cell::Text(s.clone()),
        Data::Error(e) => Cell::Error(format!("{e}")),
    }
}

/// Reads every sheet of a workbook. Empty sheets are left out; a workbook with none is
/// [`DataError::Empty`].
pub fn read(bytes: &[u8], limits: &Limits) -> Result<Vec<Table>, DataError> {
    if bytes.len() as u64 > limits.max_bytes {
        return Err(DataError::TooLarge(limits.max_bytes));
    }
    let mut wb = calamine::open_workbook_auto_from_rs(std::io::Cursor::new(bytes))
        .map_err(|e| DataError::Unreadable(e.to_string()))?;
    let mut tables = Vec::new();
    for name in wb.sheet_names() {
        let range = wb
            .worksheet_range(&name)
            .map_err(|e| DataError::Unreadable(format!("sheet `{name}`: {e}")))?;
        let total = range.height() as u64;
        let raw: Vec<Vec<Cell>> = range
            .rows()
            .take(limits.max_rows + 1)
            .map(|r| r.iter().map(cell).collect())
            .collect();
        if raw
            .iter()
            .all(|r| r.iter().all(|c| matches!(c, Cell::Empty)))
        {
            continue;
        }
        tables.push(build(Some(name), raw, Some(total), limits, vec![]));
    }
    if tables.is_empty() {
        return Err(DataError::Empty);
    }
    Ok(tables)
}
