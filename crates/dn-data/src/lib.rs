//! dn-data: tabular files to a computed fact sheet (#366, ADR-0020 R4).
//!
//! The engine computes every number; the model only quotes them. A CSV or spreadsheet is
//! read into [`Table`]s ([`read`]), each is profiled into a [`FactSheet`] ([`profile()`]) whose
//! facts carry ids (`F1`, `F2`, …), and [`trace::untraced_numbers`] finds numbers in an
//! answer that no fact has.
//!
//! Profiling follows the methods of K-Dense's exploratory-data-analysis skill
//! (<https://github.com/K-Dense-AI/scientific-agent-skills>, MIT, Copyright (c) 2025 K-Dense
//! Inc.), reimplemented in Rust; no code was copied. See [`profile`] for what was taken and
//! what was added.
//!
//! Pure: bytes in, facts out. Nothing here reads files by path, writes anything or uses the
//! network. Cell text is data from the file; whoever shows a fact sheet to a model renders
//! it as untrusted content.

pub mod aggregate;
pub mod csv;
pub mod profile;
pub mod table;
pub mod tool;
pub mod trace;
pub mod value;
pub mod workbook;

pub use profile::{ColumnProfile, ColumnType, Fact, FactSheet, FactValue, Options, profile};
pub use table::{Cell, Limits, Table};
pub use trace::{Untraced, mentions, untraced_numbers};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DataError {
    #[error("the file is larger than {0} bytes")]
    TooLarge(u64),
    #[error("the file has no data")]
    Empty,
    #[error("the file can't be read as a spreadsheet: {0}")]
    Unreadable(String),
    #[error("`{0}` isn't a file type this reads (CSV, TSV, TXT, XLSX, XLSM, XLSB, XLS, ODS)")]
    Unsupported(String),
}

/// What a file is, by its name's extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Delimited,
    Workbook,
}

pub fn kind_of(name: &str) -> Option<Kind> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "csv" | "tsv" | "txt" => Some(Kind::Delimited),
        "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => Some(Kind::Workbook),
        _ => None,
    }
}

/// Reads a file into tables: one for delimited text, one per non-empty sheet for a
/// workbook.
pub fn read(name: &str, bytes: &[u8], limits: &Limits) -> Result<Vec<Table>, DataError> {
    match kind_of(name) {
        Some(Kind::Delimited) => Ok(vec![csv::read(bytes, limits)?]),
        Some(Kind::Workbook) => workbook::read(bytes, limits),
        None => Err(DataError::Unsupported(name.to_string())),
    }
}
