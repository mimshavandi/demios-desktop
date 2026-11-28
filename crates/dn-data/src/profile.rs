//! The fact sheet: every number a model may quote about a table, computed here.
//!
//! Profiling follows the methods of K-Dense's exploratory-data-analysis skill (MIT,
//! © 2025 K-Dense Inc.; reimplemented, not copied): column types from the non-missing
//! values (integer, number, boolean, date, text, mixed), missing values kept apart from
//! zeros, quartiles by linear interpolation, IQR fences at 1.5× as a flag (not a verdict),
//! exact duplicate rows, and no correlations, tests or causal claims. Added here: exact
//! decimal sums, dates, per-group totals for columns with few values, and totals per month
//! or year.

use crate::table::{Cell, Table};
use crate::value::{
    Date, DateOrder, Decimal, DecimalMark, MAX_SCALE, date_order, parse_date, parse_number,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// What counts as missing besides an empty cell (case-insensitive, after trimming).
pub const MISSING_TOKENS: &[&str] = &["na", "n/a", "null", "none", "#n/a", "-", "—", "nan"];

#[derive(Clone, Debug)]
pub struct Options {
    /// Text columns with at most this many distinct values get per-value totals.
    pub max_group_values: usize,
    /// At most this many columns are used for groups, and this many number columns.
    pub max_group_columns: usize,
    pub max_measure_columns: usize,
    /// The fact sheet stops here (and says so).
    pub max_facts: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_group_values: 20,
            max_group_columns: 3,
            max_measure_columns: 8,
            max_facts: 800,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
    Integer,
    Number,
    Boolean,
    Date,
    Text,
    /// Some values are numbers and some aren't: no number statistics.
    Mixed,
    /// Every value is missing.
    Empty,
}

/// One quotable fact.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Fact {
    /// `F1`, `F2`, … in order.
    pub id: String,
    /// What it is, e.g. `"Revenue" sum` or `"Revenue" sum where "Region" is "North"`.
    pub label: String,
    #[serde(flatten)]
    pub value: FactValue,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FactValue {
    /// A number as it should be quoted (`text`), and its value.
    Number {
        text: String,
        value: f64,
    },
    /// A percentage, e.g. a share of missing values: `text` is `12.5` for 12.5 %.
    Percent {
        text: String,
        value: f64,
    },
    Date {
        text: String,
    },
    Text {
        text: String,
    },
}

impl FactValue {
    pub(crate) fn number(d: Decimal) -> Self {
        FactValue::Number {
            text: d.text(),
            value: d.to_f64(),
        }
    }

    pub(crate) fn count(n: u64) -> Self {
        FactValue::Number {
            text: n.to_string(),
            value: n as f64,
        }
    }

    fn percent(part: u64, whole: u64) -> Self {
        // A u64 times 1000 always fits in an i128.
        let d =
            ratio(i128::from(part) * 100, 0, whole, 1).unwrap_or(Decimal { units: 0, scale: 1 });
        FactValue::Percent {
            text: d.text(),
            value: d.to_f64(),
        }
    }

    /// The text a fact sheet line shows.
    pub fn text(&self) -> String {
        match self {
            FactValue::Number { text, .. }
            | FactValue::Date { text }
            | FactValue::Text { text } => text.clone(),
            FactValue::Percent { text, .. } => format!("{text}%"),
        }
    }
}

/// `units / 10^scale / n`, rounded half away from zero to `out_scale` decimals; `None`
/// if it doesn't fit (then the fact is left out, and the sheet says so).
pub(crate) fn ratio(units: i128, scale: u32, n: u64, out_scale: u32) -> Option<Decimal> {
    let n = i128::from(n.max(1));
    let shift = 10i128.checked_pow(out_scale)?;
    // units * 10^out / 10^scale / n, done as one rounding division.
    let num = units.checked_mul(shift)?;
    let den = n.checked_mul(10i128.checked_pow(scale)?)?;
    let q = num / den;
    let r = num % den;
    let q = if r.abs().checked_mul(2)? >= den {
        q.checked_add(r.signum())?
    } else {
        q
    };
    Some(Decimal {
        units: q,
        scale: out_scale,
    })
}

/// Facts about one column, for the structured part of the sheet.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ColumnProfile {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ColumnType,
    pub values: u64,
    pub missing: u64,
    /// Spreadsheet error cells (`#DIV/0!`), counted apart.
    pub errors: u64,
    pub distinct: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decimal_mark: Option<DecimalMark>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_order: Option<DateOrder>,
    /// Day/month order was assumed: month/day would also fit every value.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub date_order_assumed: bool,
    /// Values with more digits than can be computed with exactly (an id, a code): they
    /// aren't treated as numbers.
    #[serde(skip_serializing_if = "is_zero")]
    pub too_long: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// A number written with more significant digits than [`crate::value::MAX_DIGITS`].
fn is_too_long(c: &Cell) -> bool {
    match c {
        Cell::Number(v) => v.is_finite() && Decimal::from_f64(*v).is_none(),
        Cell::Text(t) => {
            let t = t.trim().trim_start_matches(['-', '+']);
            let digits = t.bytes().filter(u8::is_ascii_digit).count();
            !t.is_empty()
                && t.bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b',' | b' '))
                && digits > crate::value::MAX_DIGITS
        }
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FactSheet {
    /// The file's name as the user sees it.
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
    pub rows_scanned: u64,
    pub rows_total: u64,
    pub columns: Vec<ColumnProfile>,
    pub facts: Vec<Fact>,
    /// How the file was read and what was left out (scan limits, assumptions).
    pub notes: Vec<String>,
}

/// A column's values, typed.
pub(crate) enum Typed {
    Numbers(Vec<Option<Decimal>>),
    Dates(Vec<Option<Date>>),
    Other,
}

pub(crate) struct Col<'a> {
    pub(crate) name: &'a str,
    pub(crate) profile: ColumnProfile,
    pub(crate) typed: Typed,
    /// Display text per row (`None` when missing or an error).
    pub(crate) texts: Vec<Option<String>>,
}

/// An error cell, or a number that isn't finite (whatever produced it): counted apart,
/// never computed with and never dropped silently.
pub(crate) fn is_error(c: &Cell) -> bool {
    match c {
        Cell::Error(_) => true,
        Cell::Number(v) => !v.is_finite(),
        _ => false,
    }
}

pub(crate) fn is_missing(c: &Cell) -> bool {
    match c {
        Cell::Empty => true,
        Cell::Text(t) => {
            let t = t.trim();
            t.is_empty() || MISSING_TOKENS.iter().any(|m| t.eq_ignore_ascii_case(m))
        }
        _ => false,
    }
}

pub(crate) fn column<'a>(table: &'a Table, i: usize, prefer_comma: bool) -> Col<'a> {
    let cells: Vec<&Cell> = table.rows.iter().map(|r| &r[i]).collect();
    let present: Vec<&Cell> = cells
        .iter()
        .copied()
        .filter(|c| !is_missing(c) && !is_error(c))
        .collect();
    let missing = cells.iter().filter(|c| is_missing(c)).count() as u64;
    let errors = cells.iter().filter(|c| is_error(c)).count() as u64;
    let texts: Vec<Option<String>> = cells
        .iter()
        .map(|c| (!is_missing(c) && !is_error(c)).then(|| c.display()))
        .collect();
    let distinct = texts.iter().flatten().collect::<HashSet<_>>().len() as u64;
    let mut profile = ColumnProfile {
        name: table.header[i].clone(),
        ty: ColumnType::Empty,
        values: present.len() as u64,
        missing,
        errors,
        distinct,
        decimal_mark: None,
        date_order: None,
        date_order_assumed: false,
        too_long: present.iter().filter(|c| is_too_long(c)).count() as u64,
    };
    if present.is_empty() {
        return Col {
            name: &table.header[i],
            profile,
            typed: Typed::Other,
            texts,
        };
    }
    let text_cells: Vec<&str> = present
        .iter()
        .filter_map(|c| match c {
            Cell::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    let only = |f: fn(&Cell) -> bool| present.iter().all(|c| f(c) || matches!(c, Cell::Text(_)));

    // Numbers: spreadsheet numbers plus texts that parse with one decimal mark.
    // A number too long to compute with makes the column not a number column (no silent
    // dropping of values); the sheet says why.
    if only(|c| matches!(c, Cell::Number(_))) && profile.too_long == 0 {
        let marks: &[DecimalMark] = if prefer_comma {
            &[DecimalMark::Comma, DecimalMark::Dot]
        } else {
            &[DecimalMark::Dot, DecimalMark::Comma]
        };
        let mark = marks
            .iter()
            .copied()
            .find(|m| text_cells.iter().all(|t| parse_number(t, *m).is_some()));
        if let Some(mark) = mark {
            let values: Vec<Option<Decimal>> = cells
                .iter()
                .map(|c| match c {
                    Cell::Number(v) => Decimal::from_f64(*v),
                    Cell::Text(t) if !is_missing(c) => parse_number(t, mark),
                    _ => None,
                })
                .collect();
            let integer = values.iter().flatten().all(|d| d.scale == 0);
            profile.ty = if integer {
                ColumnType::Integer
            } else {
                ColumnType::Number
            };
            if !text_cells.is_empty() {
                profile.decimal_mark = Some(mark);
            }
            return Col {
                name: &table.header[i],
                profile,
                typed: Typed::Numbers(values),
                texts,
            };
        }
    }
    // Dates: spreadsheet dates plus texts in one order.
    if only(|c| matches!(c, Cell::Date(_)))
        && let Some((order, assumed)) = if text_cells.is_empty() {
            Some((DateOrder::Iso, false))
        } else {
            date_order(text_cells.iter().copied())
        }
    {
        let values: Vec<Option<Date>> = cells
            .iter()
            .map(|c| match c {
                Cell::Date(d) => Some(*d),
                Cell::Text(t) if !is_missing(c) => parse_date(t, order),
                _ => None,
            })
            .collect();
        profile.ty = ColumnType::Date;
        if !text_cells.is_empty() {
            profile.date_order = Some(order);
            profile.date_order_assumed = assumed;
        }
        return Col {
            name: &table.header[i],
            profile,
            typed: Typed::Dates(values),
            texts,
        };
    }
    let is_bool = |c: &&Cell| match c {
        Cell::Bool(_) => true,
        Cell::Text(t) => {
            let t = t.trim();
            t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false")
        }
        _ => false,
    };
    let any_number = present.iter().any(|c| {
        matches!(c, Cell::Number(_))
            || matches!(c, Cell::Text(t) if parse_number(t, DecimalMark::Dot).is_some() || parse_number(t, DecimalMark::Comma).is_some())
    });
    profile.ty = if present.iter().all(is_bool) {
        ColumnType::Boolean
    } else if any_number {
        ColumnType::Mixed
    } else {
        ColumnType::Text
    };
    Col {
        name: &table.header[i],
        profile,
        typed: Typed::Other,
        texts,
    }
}

/// Builds facts in order, numbering them, up to a limit.
struct Facts {
    out: Vec<Fact>,
    max: usize,
    cut: bool,
    /// Facts left out because the exact result didn't fit.
    too_large: Vec<String>,
}

impl Facts {
    /// A fact whose computation may not fit: left out (and listed in the notes) if not.
    fn push_or_skip(&mut self, label: String, value: Option<FactValue>) {
        match value {
            Some(v) => self.push(label, v),
            None => self.too_large.push(label),
        }
    }

    fn push(&mut self, label: String, value: FactValue) {
        if self.out.len() >= self.max {
            self.cut = true;
            return;
        }
        let id = format!("F{}", self.out.len() + 1);
        self.out.push(Fact { id, label, value });
    }
}

/// A name or value from the file, quoted for a fact line: one line (every line or
/// paragraph separator and control character becomes a space), with `"`, `\`, `[` and `]`
/// escaped, so cell text can't end its quotes, start a line that looks like a fact or hold
/// a fact id.
pub(crate) fn q(name: &str) -> String {
    let flat: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{85}') {
                ' '
            } else {
                c
            }
        })
        .collect();
    // `[` and `]` too, so cell text can't hold something that reads as a fact id.
    let escaped = flat
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('[', "\\[")
        .replace(']', "\\]");
    // And no run of `<` or `>` that could pass for an untrusted-content delimiter, since
    // these names also appear in messages outside a block.
    format!("\"{}\"", dn_core::untrusted::neutralise(&escaped))
}

/// Linear-interpolation quantile of sorted values.
pub(crate) fn quantile(sorted: &[f64], p: f64) -> f64 {
    let pos = p * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

/// `v` to `scale` decimals; `None` if it can't be shown exactly (then the fact is left out
/// and listed, never shown as 0).
pub(crate) fn rounded(v: f64, scale: u32) -> Option<Decimal> {
    Decimal::from_f64(v).and_then(|d| d.rescale(scale))
}

/// Names from the file (columns, sheets) listed in a message for the model, each quoted
/// like a fact line ([`q`] neutralises delimiters too).
pub(crate) fn listed<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    names.into_iter().map(q).collect::<Vec<_>>().join(", ")
}

/// The decimals a number column is shown with: the most any of its values has.
pub(crate) fn column_scale(values: &[Option<Decimal>]) -> u32 {
    values
        .iter()
        .flatten()
        .map(|d| d.scale)
        .max()
        .unwrap_or(0)
        .min(MAX_SCALE)
}

/// Exact sum of decimals at the largest scale among them; `None` if it doesn't fit.
pub(crate) fn exact_sum(values: &[Decimal]) -> (Option<Decimal>, u32) {
    let scale = values
        .iter()
        .map(|d| d.scale)
        .max()
        .unwrap_or(0)
        .min(MAX_SCALE);
    let units = values
        .iter()
        .map(|d| d.rescale(scale))
        .try_fold(0i128, |a, d| a.checked_add(d?.units));
    (units.map(|units| Decimal { units, scale }), scale)
}

fn number_facts(f: &mut Facts, name: &str, values: &[Decimal]) {
    let n = values.len() as u64;
    if n == 0 {
        return;
    }
    let (sum, scale) = exact_sum(values);
    let shown = (scale + 2).min(MAX_SCALE);
    let col_name = q(name);
    f.push_or_skip(format!("{col_name} sum"), sum.map(FactValue::number));
    f.push_or_skip(
        format!("{col_name} mean"),
        sum.and_then(|s| ratio(s.units, scale, n, shown))
            .map(FactValue::number),
    );
    let mut sorted: Vec<f64> = values.iter().map(|d| d.to_f64()).collect();
    sorted.sort_by(f64::total_cmp);
    let min = values
        .iter()
        .min_by(|a, b| a.to_f64().total_cmp(&b.to_f64()))
        .copied();
    let max = values
        .iter()
        .max_by(|a, b| a.to_f64().total_cmp(&b.to_f64()))
        .copied();
    let col = col_name;
    if let (Some(lo), Some(hi)) = (min, max) {
        f.push(
            format!("{col} minimum"),
            FactValue::number(lo.rescale(scale).unwrap_or(lo)),
        );
        f.push(
            format!("{col} maximum"),
            FactValue::number(hi.rescale(scale).unwrap_or(hi)),
        );
    }
    let (q1, med, q3) = (
        quantile(&sorted, 0.25),
        quantile(&sorted, 0.5),
        quantile(&sorted, 0.75),
    );
    f.push_or_skip(
        format!("{col} median"),
        rounded(med, shown).map(FactValue::number),
    );
    f.push_or_skip(
        format!("{col} lower quartile (Q1)"),
        rounded(q1, shown).map(FactValue::number),
    );
    f.push_or_skip(
        format!("{col} upper quartile (Q3)"),
        rounded(q3, shown).map(FactValue::number),
    );
    if n > 1 {
        let mean = sorted.iter().sum::<f64>() / n as f64;
        let var = sorted.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
        f.push_or_skip(
            format!("{col} standard deviation (sample)"),
            rounded(var.sqrt(), shown).map(FactValue::number),
        );
    }
    let iqr = q3 - q1;
    let (lo, hi) = (q1 - 1.5 * iqr, q3 + 1.5 * iqr);
    let outside = sorted.iter().filter(|x| **x < lo || **x > hi).count() as u64;
    f.push(
        format!("{col} values outside the IQR fences (1.5 × IQR; a flag, not an error)"),
        FactValue::count(outside),
    );
}

/// Month buckets (`2026-03`) when the dates span at most three years, else years.
pub(crate) fn period_of(d: Date, monthly: bool) -> String {
    if monthly {
        format!("{:04}-{:02}", d.year, d.month)
    } else {
        format!("{:04}", d.year)
    }
}

/// Profiles one table. `source` is the file's name as the user sees it.
pub fn profile(table: &Table, source: &str, opts: &Options) -> FactSheet {
    let prefer_comma = table.read_notes.iter().any(|n| n.starts_with("semicolon"));
    let cols: Vec<Col> = (0..table.header.len())
        .map(|i| column(table, i, prefer_comma))
        .collect();
    let mut f = Facts {
        out: Vec::new(),
        max: opts.max_facts,
        cut: false,
        too_large: Vec::new(),
    };
    let scanned = table.rows.len() as u64;
    f.push("rows scanned".into(), FactValue::count(scanned));
    f.push(
        "rows in the file".into(),
        FactValue::count(table.total_rows),
    );
    f.push(
        "columns".into(),
        FactValue::count(table.header.len() as u64),
    );
    let mut seen = HashSet::new();
    let dupes = table
        .rows
        .iter()
        .filter(|r| !seen.insert(r.iter().map(Cell::display).collect::<Vec<_>>()))
        .count() as u64;
    f.push(
        "exact duplicate rows (repeats of an earlier row)".into(),
        FactValue::count(dupes),
    );

    for c in &cols {
        let p = &c.profile;
        let col = q(c.name);
        let ty = serde_json::to_value(p.ty)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        f.push(format!("{col} type"), FactValue::Text { text: ty });
        f.push(format!("{col} values present"), FactValue::count(p.values));
        f.push(format!("{col} missing values"), FactValue::count(p.missing));
        f.push(
            format!("{col} missing share"),
            FactValue::percent(p.missing, scanned),
        );
        if p.errors > 0 {
            f.push(
                format!("{col} spreadsheet error cells"),
                FactValue::count(p.errors),
            );
        }
        f.push(
            format!("{col} distinct values"),
            FactValue::count(p.distinct),
        );
        match &c.typed {
            Typed::Numbers(v) => {
                let values: Vec<Decimal> = v.iter().flatten().copied().collect();
                number_facts(&mut f, c.name, &values);
            }
            Typed::Dates(v) => {
                let dates: Vec<Date> = v.iter().flatten().copied().collect();
                if let (Some(a), Some(b)) = (dates.iter().min(), dates.iter().max()) {
                    f.push(format!("{col} earliest"), FactValue::Date { text: a.iso() });
                    f.push(format!("{col} latest"), FactValue::Date { text: b.iso() });
                    f.push(
                        format!("{col} span in days"),
                        FactValue::count((b.days() - a.days()) as u64),
                    );
                }
            }
            Typed::Other => {
                let mut counts: HashMap<&str, u64> = HashMap::new();
                for t in c.texts.iter().flatten() {
                    *counts.entry(t.as_str()).or_default() += 1;
                }
                let mut top: Vec<(&str, u64)> = counts.into_iter().collect();
                top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
                for (v, n) in top.into_iter().take(5) {
                    f.push(
                        format!("{col} rows with value {}", q(v)),
                        FactValue::count(n),
                    );
                }
            }
        }
    }

    let measures: Vec<(&str, &Vec<Option<Decimal>>)> = cols
        .iter()
        .filter_map(|c| match &c.typed {
            Typed::Numbers(v) => Some((c.name, v)),
            _ => None,
        })
        .take(opts.max_measure_columns)
        .collect();

    // Groups: text columns with few distinct values.
    let groups: Vec<&Col> = cols
        .iter()
        .filter(|c| {
            matches!(c.profile.ty, ColumnType::Text | ColumnType::Boolean)
                && (2..=opts.max_group_values as u64).contains(&c.profile.distinct)
        })
        .take(opts.max_group_columns)
        .collect();
    for g in &groups {
        let mut keys: Vec<&String> = g
            .texts
            .iter()
            .flatten()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        keys.sort();
        for key in keys {
            let rows: Vec<usize> = (0..g.texts.len())
                .filter(|r| g.texts[*r].as_ref() == Some(key))
                .collect();
            for (m, values) in &measures {
                let vals: Vec<Decimal> = rows.iter().filter_map(|r| values[*r]).collect();
                if vals.is_empty() {
                    continue;
                }
                // Shown with the whole column's decimals, like its other facts.
                let scale = column_scale(values);
                let sum = exact_sum(&vals).0.and_then(|s| s.rescale(scale));
                let label = format!("{} where {} is {}", q(m), q(g.name), q(key));
                f.push_or_skip(format!("{label}: sum"), sum.map(FactValue::number));
                f.push_or_skip(
                    format!("{label}: mean"),
                    sum.and_then(|s| {
                        ratio(
                            s.units,
                            scale,
                            vals.len() as u64,
                            (scale + 2).min(MAX_SCALE),
                        )
                    })
                    .map(FactValue::number),
                );
                f.push(
                    format!("{label}: values counted"),
                    FactValue::count(vals.len() as u64),
                );
            }
        }
    }

    // Trends: totals per month (or year) along the first date column.
    let mut notes = table.read_notes.clone();
    if let Some(dc) = cols.iter().find(|c| matches!(c.typed, Typed::Dates(_))) {
        let Typed::Dates(dates) = &dc.typed else {
            unreachable!()
        };
        let present: Vec<Date> = dates.iter().flatten().copied().collect();
        if let (Some(a), Some(b)) = (present.iter().min(), present.iter().max()) {
            let months = (b.year - a.year) * 12 + b.month as i32 - a.month as i32;
            let monthly = months <= 36;
            for (m, values) in &measures {
                let mut buckets: BTreeMap<String, Vec<Decimal>> = BTreeMap::new();
                for (d, v) in dates.iter().zip(values.iter()) {
                    if let (Some(d), Some(v)) = (d, v) {
                        buckets.entry(period_of(*d, monthly)).or_default().push(*v);
                    }
                }
                if buckets.len() < 2 {
                    continue;
                }
                let along = format!(
                    "{} by {} of {}",
                    q(m),
                    if monthly { "month" } else { "year" },
                    q(dc.name)
                );
                let scale = column_scale(values);
                let sums: Option<Vec<(String, Decimal)>> = buckets
                    .iter()
                    .map(|(p, v)| Some((p.clone(), exact_sum(v).0?.rescale(scale)?)))
                    .collect();
                let Some(sums) = sums else {
                    f.too_large.push(format!("{along}: sums"));
                    continue;
                };
                for (p, s) in &sums {
                    f.push(format!("{along}: sum in {p}"), FactValue::number(*s));
                }
                let (fp, first) = &sums[0];
                let (lp, last) = &sums[sums.len() - 1];
                let change = last
                    .units
                    .checked_sub(first.units)
                    .map(|units| Decimal { units, scale });
                f.push_or_skip(
                    format!("{along}: change from {fp} to {lp}"),
                    change.map(FactValue::number),
                );
                if first.units != 0 {
                    // change / |first| × 100, to one decimal (same scale cancels out).
                    let rel = change.and_then(|c| {
                        let base = u64::try_from(first.units.unsigned_abs()).ok()?;
                        ratio(c.units.checked_mul(100)?, 0, base, 1)
                    });
                    f.push_or_skip(
                        format!("{along}: change from {fp} to {lp} in percent"),
                        rel.map(|r| FactValue::Percent {
                            text: r.text(),
                            value: r.to_f64(),
                        }),
                    );
                }
                let mut best_up: Option<(i128, &str, &str)> = None;
                let mut best_down: Option<(i128, &str, &str)> = None;
                for w in sums.windows(2) {
                    let Some(d) = w[1].1.units.checked_sub(w[0].1.units) else {
                        continue;
                    };
                    if d > 0 && best_up.is_none_or(|b| d > b.0) {
                        best_up = Some((d, &w[0].0, &w[1].0));
                    }
                    if d < 0 && best_down.is_none_or(|b| d < b.0) {
                        best_down = Some((d, &w[0].0, &w[1].0));
                    }
                }
                if let Some((d, a, b)) = best_up {
                    f.push(
                        format!("{along}: largest rise, from {a} to {b}"),
                        FactValue::number(Decimal { units: d, scale }),
                    );
                }
                if let Some((d, a, b)) = best_down {
                    f.push(
                        format!("{along}: largest fall, from {a} to {b}"),
                        FactValue::number(Decimal { units: d, scale }),
                    );
                }
                if let Some((p, _)) = sums.iter().max_by_key(|(_, s)| s.units) {
                    f.push(
                        format!("{along}: highest period"),
                        FactValue::Text { text: p.clone() },
                    );
                }
            }
        }
    }

    if scanned < table.total_rows {
        notes.push(format!(
            "only the first {scanned} of {} rows were read: every fact is about those rows",
            table.total_rows
        ));
    }
    if table.columns_skipped > 0 {
        notes.push(format!(
            "{} columns past the limit weren't read",
            table.columns_skipped
        ));
    }
    for c in &cols {
        if c.profile.date_order_assumed {
            notes.push(format!(
                "{}: dates read as day/month (month/day would also fit)",
                q(c.name)
            ));
        }
        if c.profile.too_long > 0 {
            notes.push(format!(
                "{}: {} values have more than {} digits, so the column isn't treated as numbers",
                q(c.name),
                c.profile.too_long,
                crate::value::MAX_DIGITS
            ));
        }
        if c.profile.decimal_mark == Some(DecimalMark::Comma) {
            notes.push(format!("{}: numbers use a decimal comma", q(c.name)));
        }
    }
    if !f.too_large.is_empty() {
        notes.push(format!(
            "left out because the exact result is too large to compute: {}",
            f.too_large.join("; ")
        ));
    }
    if f.cut {
        notes.push(format!(
            "the fact sheet stops at {} facts; some per-group and per-period facts are left out",
            opts.max_facts
        ));
    }
    notes.push(format!(
        "missing means an empty cell or one of: {}",
        MISSING_TOKENS.join(", ")
    ));
    FactSheet {
        source: source.to_string(),
        sheet: table.sheet.clone(),
        rows_scanned: scanned,
        rows_total: table.total_rows,
        columns: cols.into_iter().map(|c| c.profile).collect(),
        facts: f.out,
        notes,
    }
}

impl FactSheet {
    /// The fact by id.
    pub fn fact(&self, id: &str) -> Option<&Fact> {
        self.facts.iter().find(|f| f.id == id)
    }

    /// The fact by label (tests and evals).
    pub fn by_label(&self, label: &str) -> Option<&Fact> {
        self.facts.iter().find(|f| f.label == label)
    }

    /// Plain text, one fact per line: `[F12] "Revenue" sum: 12345.50`.
    pub fn render(&self) -> String {
        let mut s = format!("Fact sheet for {}", q(&self.source));
        if let Some(sh) = &self.sheet {
            s.push_str(&format!(", sheet {}", q(sh)));
        }
        s.push_str(&format!(
            ". {} of {} rows scanned, {} columns.\n",
            self.rows_scanned,
            self.rows_total,
            self.columns.len()
        ));
        for f in &self.facts {
            s.push_str(&format!("[{}] {}: {}\n", f.id, f.label, f.value.text()));
        }
        if !self.notes.is_empty() {
            s.push_str("Notes:\n");
            for n in &self.notes {
                s.push_str(&format!("- {n}\n"));
            }
        }
        s
    }
}
