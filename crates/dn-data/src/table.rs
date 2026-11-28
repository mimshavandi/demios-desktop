//! A table read from a file: a header and rows of cells, before any typing.

use crate::value::Date;

/// A cell as the file gives it: CSV only has text; spreadsheets have typed cells.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Empty,
    Text(String),
    Number(f64),
    Bool(bool),
    Date(Date),
    /// A spreadsheet error such as `#DIV/0!`.
    Error(String),
}

impl Cell {
    /// The text a person would see (for counting values and top values).
    pub fn display(&self) -> String {
        match self {
            Cell::Empty => String::new(),
            Cell::Text(t) => t.trim().to_string(),
            Cell::Number(v) => crate::value::Decimal::from_f64(*v)
                .map(|d| d.text())
                .unwrap_or_else(|| v.to_string()),
            Cell::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
            Cell::Date(d) => d.iso(),
            Cell::Error(e) => e.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    /// The sheet's name, for a spreadsheet.
    pub sheet: Option<String>,
    /// Column names; made up (`Column 1`, …) when the file has no header row.
    pub header: Vec<String>,
    /// Whether `header` came from the file.
    pub has_header: bool,
    /// The rows read (at most the row limit), each as long as `header`.
    pub rows: Vec<Vec<Cell>>,
    /// Data rows in the file, including those past the limit.
    pub total_rows: u64,
    /// Columns past the column limit, which weren't read.
    pub columns_skipped: usize,
    /// How the file was read, for the fact sheet (e.g. "semicolon-separated, Windows-1252").
    pub read_notes: Vec<String>,
}

/// Limits for reading.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: u64,
    pub max_rows: usize,
    pub max_columns: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 50 * 1024 * 1024,
            max_rows: 200_000,
            max_columns: 200,
        }
    }
}

/// Whether the first row looks like a header: every cell is non-empty text that isn't a
/// number or a date, and either a later row has a number or date in some column, or the
/// first row has no repeated names and no later row equals it.
pub(crate) fn looks_like_header(first: &[Cell], rest: &[Vec<Cell>]) -> bool {
    let is_value = |c: &Cell| match c {
        Cell::Number(_) | Cell::Date(_) | Cell::Bool(_) => true,
        Cell::Text(t) => {
            let t = t.trim();
            crate::value::parse_number(t, crate::value::DecimalMark::Dot).is_some()
                || crate::value::parse_number(t, crate::value::DecimalMark::Comma).is_some()
                || crate::value::date_order(std::iter::once(t)).is_some()
        }
        _ => false,
    };
    if first.is_empty()
        || first
            .iter()
            .any(|c| matches!(c, Cell::Empty) || is_value(c))
    {
        return false;
    }
    let mut seen = std::collections::HashSet::new();
    let unique = first
        .iter()
        .all(|c| seen.insert(c.display().to_lowercase()));
    if rest.is_empty() {
        return unique;
    }
    let typed_below =
        (0..first.len()).any(|i| rest.iter().take(50).any(|r| r.get(i).is_some_and(is_value)));
    typed_below || (unique && !rest.iter().any(|r| r.as_slice() == first))
}

/// Builds a table from raw rows: header detection, padding to one width, the limits.
pub(crate) fn build(
    sheet: Option<String>,
    mut raw: Vec<Vec<Cell>>,
    total_rows_hint: Option<u64>,
    limits: &Limits,
    mut read_notes: Vec<String>,
) -> Table {
    // Drop fully empty rows at the end and the start.
    while raw
        .last()
        .is_some_and(|r| r.iter().all(|c| matches!(c, Cell::Empty)))
    {
        raw.pop();
    }
    let lead = raw
        .iter()
        .take_while(|r| r.iter().all(|c| matches!(c, Cell::Empty)))
        .count();
    raw.drain(..lead);
    let width = raw.iter().map(Vec::len).max().unwrap_or(0);
    let used = width.min(limits.max_columns);
    let columns_skipped = width - used;
    for r in &mut raw {
        r.resize(width, Cell::Empty);
        r.truncate(used);
    }
    let has_header = raw
        .split_first()
        .is_some_and(|(first, rest)| looks_like_header(first, rest));
    let header: Vec<String> = if has_header {
        let first = raw.remove(0);
        dedupe(first.iter().map(Cell::display).collect())
    } else {
        if !raw.is_empty() {
            read_notes.push("no header row: columns are numbered".into());
        }
        (1..=used).map(|i| format!("Column {i}")).collect()
    };
    let total_rows = total_rows_hint
        .map(|t| t.saturating_sub(u64::from(has_header)))
        .unwrap_or(raw.len() as u64)
        .max(raw.len() as u64);
    raw.truncate(limits.max_rows);
    Table {
        sheet,
        header,
        has_header,
        rows: raw,
        total_rows,
        columns_skipped,
        read_notes,
    }
}

/// Unique column names (ignoring case): blanks become `Column N`, and a repeat gets the
/// first free ` (2)`, ` (3)`, … that no other column has, including names written that way
/// in the file.
fn dedupe(names: Vec<String>) -> Vec<String> {
    let names: Vec<String> = names
        .into_iter()
        .enumerate()
        .map(|(i, n)| {
            if n.trim().is_empty() {
                format!("Column {}", i + 1)
            } else {
                n.trim().to_string()
            }
        })
        .collect();
    // Names as written are kept first; only later repeats are renamed.
    let mut taken = std::collections::HashSet::new();
    let mut first_seen = std::collections::HashSet::new();
    let keep: Vec<bool> = names
        .iter()
        .map(|n| first_seen.insert(n.to_lowercase()))
        .collect();
    for (n, k) in names.iter().zip(&keep) {
        if *k {
            taken.insert(n.to_lowercase());
        }
    }
    names
        .into_iter()
        .zip(keep)
        .map(|(n, keep)| {
            if keep {
                return n;
            }
            let mut k = 2;
            loop {
                let candidate = format!("{n} ({k})");
                if taken.insert(candidate.to_lowercase()) {
                    return candidate;
                }
                k += 1;
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Cell {
        Cell::Text(s.into())
    }

    #[test]
    fn header_detection_and_names() {
        let rows = vec![
            vec![t("Region"), t("Revenue"), t("Revenue")],
            vec![t("North"), t("1,200.50"), t("3")],
        ];
        let tb = build(None, rows, None, &Limits::default(), vec![]);
        assert!(tb.has_header);
        assert_eq!(tb.header, ["Region", "Revenue", "Revenue (2)"]);
        assert_eq!(tb.total_rows, 1);
        let no_header = vec![vec![t("1"), t("2")], vec![t("3"), t("4")]];
        let tb = build(None, no_header, None, &Limits::default(), vec![]);
        assert!(!tb.has_header);
        assert_eq!(tb.header, ["Column 1", "Column 2"]);
        assert_eq!(tb.rows.len(), 2);
    }

    #[test]
    fn renamed_repeats_never_collide_with_other_names() {
        let names = |v: &[&str]| dedupe(v.iter().map(|s| s.to_string()).collect());
        assert_eq!(
            names(&["Revenue", "Revenue", "Revenue (2)"]),
            ["Revenue", "Revenue (3)", "Revenue (2)"]
        );
        assert_eq!(
            names(&["a", "A", "a", "", "a (2)"]),
            ["a", "A (3)", "a (4)", "Column 4", "a (2)"]
        );
        let all = names(&["x", "x", "x (2)", "x (3)", "x"]);
        let unique: std::collections::HashSet<String> =
            all.iter().map(|s| s.to_lowercase()).collect();
        assert_eq!(unique.len(), all.len(), "{all:?}");
    }

    #[test]
    fn limits_cut_rows_and_columns_but_count_them() {
        let rows: Vec<Vec<Cell>> = std::iter::once(vec![t("a"), t("b"), t("c")])
            .chain((0..10).map(|i| vec![t(&i.to_string()), t("x"), t("y")]))
            .collect();
        let lim = Limits {
            max_rows: 4,
            max_columns: 2,
            ..Limits::default()
        };
        let tb = build(None, rows, None, &lim, vec![]);
        assert_eq!(
            (tb.rows.len(), tb.total_rows, tb.columns_skipped),
            (4, 10, 1)
        );
        assert_eq!(tb.header, ["a", "b"]);
    }
}
