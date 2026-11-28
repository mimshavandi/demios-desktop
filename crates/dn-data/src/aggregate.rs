//! Questions the fact sheet doesn't answer directly ("Weber's orders in October", "mean
//! amount per product for class orders"), computed here with the same exact arithmetic, so
//! the model still never calculates (#366, PR 3).
//!
//! A query picks rows with filters (all must hold), optionally groups them by a column, and
//! applies one operation to a number column (or counts rows). Every result is a fact with an
//! id (`A1`, `A2`, …) and states how many rows it covers, and how many were left out because
//! a value was missing.

use crate::profile::{
    ColumnType, Fact, FactSheet, FactValue, Typed, column, exact_sum, period_of, q, quantile,
    ratio, rounded,
};
use crate::table::Table;
use crate::value::{Date, Decimal, DecimalMark, MAX_SCALE, parse_number};
use serde::Deserialize;

/// Most groups in one result.
pub const MAX_GROUPS: usize = 50;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Sum,
    Mean,
    Median,
    Min,
    Max,
    /// Rows matched (or, with a `measure`, values present).
    Count,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    /// Text: same value (ignoring case). Number: equal. Date: in that day, month
    /// (`2026-10`) or year (`2026`).
    Is,
    IsNot,
    /// Text only: the value contains this (ignoring case).
    Contains,
    /// Number or date (a date filter compares from the start of a month or year).
    AtLeast,
    AtMost,
    Above,
    Below,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub column: String,
    pub op: FilterOp,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub op: Op,
    /// The number column (required for every operation but `count`).
    #[serde(default)]
    pub measure: Option<String>,
    #[serde(default)]
    pub filters: Vec<Filter>,
    /// A text or yes/no column (one result per value) or a date column (one per month, or
    /// per year when the dates span more than three years).
    #[serde(default)]
    pub group_by: Option<String>,
}

/// A date period from a filter value: `2026-10-05`, `2026-10` or `2026`, as the first and
/// last day it covers.
fn period(value: &str) -> Option<(Date, Date)> {
    let v = value.trim();
    let parts: Vec<&str> = v.split('-').collect();
    let num = |s: &str| s.parse::<u32>().ok();
    match parts.as_slice() {
        [y] if y.len() == 4 => {
            let y: i32 = y.parse().ok()?;
            Some((Date::new(y, 1, 1)?, Date::new(y, 12, 31)?))
        }
        [y, m] if y.len() == 4 => {
            let (y, m) = (y.parse().ok()?, num(m)?);
            let last = crate::value::days_in_month(y, m.clamp(1, 12));
            Some((Date::new(y, m, 1)?, Date::new(y, m, last)?))
        }
        [y, m, d] if y.len() == 4 => {
            let d = Date::new(y.parse().ok()?, num(m)?, num(d)?)?;
            Some((d, d))
        }
        _ => None,
    }
}

fn number(value: &str) -> Option<Decimal> {
    parse_number(value, DecimalMark::Dot).or_else(|| parse_number(value, DecimalMark::Comma))
}

/// Runs `query` on `table`. Errors are for the model (an unknown column names the columns).
pub fn aggregate(table: &Table, source: &str, query: &Query) -> Result<FactSheet, String> {
    let prefer_comma = table.read_notes.iter().any(|n| n.starts_with("semicolon"));
    let cols: Vec<_> = (0..table.header.len())
        .map(|i| column(table, i, prefer_comma))
        .collect();
    let find = |name: &str| {
        cols.iter()
            .position(|c| c.name.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| {
                format!(
                    "There's no column \"{name}\". The columns are: {}.",
                    table.header.join(", ")
                )
            })
    };

    // Rows passing every filter; rows where a filter's column is missing don't pass, and are
    // counted.
    let n = table.rows.len();
    let mut keep = vec![true; n];
    let mut missing_in_filters = 0u64;
    let mut described = Vec::new();
    for f in &query.filters {
        let c = &cols[find(&f.column)?];
        let name = q(c.name);
        let pass: Box<dyn Fn(usize) -> Option<bool>> = match (&c.typed, f.op) {
            (Typed::Numbers(v), op) if op != FilterOp::Contains => {
                let x = number(&f.value)
                    .ok_or_else(|| format!("{name} holds numbers; \"{}\" isn't one.", f.value))?;
                let x = x.to_f64();
                let v = v.clone();
                Box::new(move |r| {
                    v[r].map(|d| {
                        let d = d.to_f64();
                        match op {
                            FilterOp::Is => d == x,
                            FilterOp::IsNot => d != x,
                            FilterOp::AtLeast => d >= x,
                            FilterOp::AtMost => d <= x,
                            FilterOp::Above => d > x,
                            FilterOp::Below => d < x,
                            FilterOp::Contains => false,
                        }
                    })
                })
            }
            (Typed::Dates(v), op) if op != FilterOp::Contains => {
                let (from, to) = period(&f.value).ok_or_else(|| {
                    format!(
                        "{name} holds dates; give a day, month or year like 2026-10-05, 2026-10 or 2026, not \"{}\".",
                        f.value
                    )
                })?;
                let v = v.clone();
                Box::new(move |r| {
                    v[r].map(|d| match op {
                        FilterOp::Is => d >= from && d <= to,
                        FilterOp::IsNot => d < from || d > to,
                        FilterOp::AtLeast => d >= from,
                        FilterOp::AtMost => d <= to,
                        FilterOp::Above => d > to,
                        FilterOp::Below => d < from,
                        FilterOp::Contains => false,
                    })
                })
            }
            (Typed::Other, FilterOp::Is | FilterOp::IsNot | FilterOp::Contains) => {
                let want = f.value.trim().to_lowercase();
                let texts = c.texts.clone();
                let op = f.op;
                Box::new(move |r| {
                    texts[r].as_ref().map(|t| {
                        let t = t.to_lowercase();
                        match op {
                            FilterOp::Is => t == want,
                            FilterOp::IsNot => t != want,
                            _ => t.contains(&want),
                        }
                    })
                })
            }
            _ => {
                return Err(format!(
                    "{name} can't be filtered with {:?}: use is, is_not or contains for text, and is, is_not, at_least, at_most, above or below for numbers and dates.",
                    f.op
                ));
            }
        };
        for (r, k) in keep.iter_mut().enumerate() {
            if !*k {
                continue;
            }
            match pass(r) {
                Some(true) => {}
                Some(false) => *k = false,
                None => {
                    *k = false;
                    missing_in_filters += 1;
                }
            }
        }
        let shown = match f.op {
            FilterOp::Is => "is",
            FilterOp::IsNot => "is not",
            FilterOp::Contains => "contains",
            FilterOp::AtLeast => "is at least",
            FilterOp::AtMost => "is at most",
            FilterOp::Above => "is above",
            FilterOp::Below => "is below",
        };
        described.push(format!("{name} {shown} {}", q(&f.value)));
    }

    let measure = match (&query.measure, query.op) {
        (Some(m), _) => {
            let i = find(m)?;
            match &cols[i].typed {
                Typed::Numbers(v) => Some((cols[i].name, v)),
                _ => {
                    return Err(format!(
                        "{} isn't a number column ({:?}), so it can't be summed or averaged.",
                        q(cols[i].name),
                        cols[i].profile.ty
                    ));
                }
            }
        }
        (None, Op::Count) => None,
        (None, op) => {
            return Err(format!(
                "`{op:?}` needs a `measure`: the number column to use."
            ));
        }
    };

    // Groups.
    let groups: Vec<(Option<String>, Vec<usize>)> = match &query.group_by {
        None => vec![(None, (0..n).filter(|r| keep[*r]).collect())],
        Some(g) => {
            let c = &cols[find(g)?];
            let key: Box<dyn Fn(usize) -> Option<String>> = match &c.typed {
                Typed::Dates(v) => {
                    let present: Vec<Date> = v.iter().flatten().copied().collect();
                    let monthly = match (present.iter().min(), present.iter().max()) {
                        (Some(a), Some(b)) => {
                            (b.year - a.year) * 12 + b.month as i32 - a.month as i32 <= 36
                        }
                        _ => true,
                    };
                    let v = v.clone();
                    Box::new(move |r| v[r].map(|d| period_of(d, monthly)))
                }
                Typed::Other
                    if matches!(
                        c.profile.ty,
                        ColumnType::Text | ColumnType::Boolean | ColumnType::Mixed
                    ) =>
                {
                    let t = c.texts.clone();
                    Box::new(move |r| t[r].clone())
                }
                _ => {
                    return Err(format!(
                        "{} can't be grouped by: group by a text, yes/no or date column.",
                        q(c.name)
                    ));
                }
            };
            let mut map: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
            for r in (0..n).filter(|r| keep[*r]) {
                if let Some(k) = key(r) {
                    map.entry(k).or_default().push(r);
                }
            }
            if map.len() > MAX_GROUPS {
                return Err(format!(
                    "{} has {} different values among the matching rows; narrow it with a filter (at most {MAX_GROUPS} groups).",
                    q(c.name),
                    map.len()
                ));
            }
            map.into_iter().map(|(k, v)| (Some(k), v)).collect()
        }
    };

    let mut facts = Vec::new();
    let mut push = |label: String, value: FactValue| {
        let id = format!("A{}", facts.len() + 1);
        facts.push(Fact { id, label, value });
    };
    let what = match (&measure, query.op) {
        (None, _) => "rows".to_string(),
        (Some((m, _)), op) => format!("{} of {}", format!("{op:?}").to_lowercase(), q(m)),
    };
    let filter_text = if described.is_empty() {
        String::new()
    } else {
        format!(" where {}", described.join(" and "))
    };
    let matched: usize = (0..n).filter(|r| keep[*r]).count();
    push(
        format!("rows matching{filter_text}"),
        FactValue::count(matched as u64),
    );
    if missing_in_filters > 0 {
        push(
            "rows left out because a filtered column was empty".into(),
            FactValue::count(missing_in_filters),
        );
    }
    for (key, rows) in &groups {
        let scope = match (key, &query.group_by) {
            (Some(k), Some(g)) => format!("{what}{filter_text}, for {} {}", q(g), q(k)),
            _ => format!("{what}{filter_text}"),
        };
        match &measure {
            None => push(scope, FactValue::count(rows.len() as u64)),
            Some((m, values)) => {
                let vals: Vec<Decimal> = rows.iter().filter_map(|r| values[*r]).collect();
                let left_out = rows.len() - vals.len();
                let scale = crate::profile::column_scale(values);
                let value = result(query.op, &vals, scale);
                match value {
                    Some(v) => push(scope.clone(), v),
                    None if vals.is_empty() => {
                        push(
                            scope.clone(),
                            // Not 0: there's no figure, and a 0 would be quoted as one.
                            FactValue::Text {
                                text: "none: no matching row has a value".into(),
                            },
                        )
                    }
                    None => push(
                        format!("{scope}: too large to compute exactly"),
                        FactValue::Text {
                            text: "left out".into(),
                        },
                    ),
                }
                if left_out > 0 {
                    push(
                        format!("{scope}: matching rows with no {} value, left out", q(m)),
                        FactValue::count(left_out as u64),
                    );
                }
            }
        }
    }
    Ok(FactSheet {
        source: source.to_string(),
        sheet: table.sheet.clone(),
        rows_scanned: table.rows.len() as u64,
        rows_total: table.total_rows,
        columns: Vec::new(),
        facts,
        notes: if (table.rows.len() as u64) < table.total_rows {
            vec![format!(
                "only the first {} of {} rows were read: results are about those rows",
                table.rows.len(),
                table.total_rows
            )]
        } else {
            Vec::new()
        },
    })
}

fn result(op: Op, vals: &[Decimal], scale: u32) -> Option<FactValue> {
    if vals.is_empty() {
        return match op {
            Op::Count => Some(FactValue::count(0)),
            _ => None,
        };
    }
    let shown = (scale + 2).min(MAX_SCALE);
    let num = FactValue::number;
    match op {
        Op::Count => Some(FactValue::count(vals.len() as u64)),
        Op::Sum => exact_sum(vals).0.and_then(|s| s.rescale(scale)).map(num),
        Op::Mean => {
            let (sum, sc) = exact_sum(vals);
            sum.and_then(|s| ratio(s.units, sc, vals.len() as u64, shown))
                .map(num)
        }
        Op::Min | Op::Max => {
            let pick = if op == Op::Min {
                vals.iter().min_by(|a, b| a.to_f64().total_cmp(&b.to_f64()))
            } else {
                vals.iter().max_by(|a, b| a.to_f64().total_cmp(&b.to_f64()))
            };
            pick.and_then(|d| d.rescale(scale)).map(num)
        }
        Op::Median => {
            let mut sorted: Vec<f64> = vals.iter().map(|d| d.to_f64()).collect();
            sorted.sort_by(f64::total_cmp);
            Some(num(rounded(quantile(&sorted, 0.5), shown)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Limits, read};

    fn sales() -> Table {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sales.csv");
        read("sales.csv", &std::fs::read(p).unwrap(), &Limits::default())
            .unwrap()
            .remove(0)
    }

    fn query(json: serde_json::Value) -> Query {
        serde_json::from_value(json).unwrap()
    }

    fn texts(s: &FactSheet) -> Vec<(String, String)> {
        s.facts
            .iter()
            .map(|f| (f.label.clone(), f.value.text()))
            .collect()
    }

    #[test]
    fn filtered_and_grouped_results_are_exact_and_say_what_they_cover() {
        let t = sales();
        // North's revenue in March: 119.94 + 119.94.
        let s = aggregate(
            &t,
            "sales.csv",
            &query(
                serde_json::json!({"op": "sum", "measure": "Revenue", "filters": [
                    {"column": "region", "op": "is", "value": "north"},
                    {"column": "Date", "op": "is", "value": "2026-03"}
                ]}),
            ),
        )
        .unwrap();
        assert_eq!(
            texts(&s),
            [
                (
                    "rows matching where \"Region\" is \"north\" and \"Date\" is \"2026-03\""
                        .into(),
                    "2".into()
                ),
                (
                    "sum of \"Revenue\" where \"Region\" is \"north\" and \"Date\" is \"2026-03\""
                        .into(),
                    "239.88".into()
                ),
            ]
        );
        assert_eq!(s.facts[1].id, "A2");

        // Mean units per product, with South's missing Units counted, not hidden.
        let s = aggregate(
            &t,
            "sales.csv",
            &query(serde_json::json!({"op": "mean", "measure": "Units", "group_by": "Product"})),
        )
        .unwrap();
        let v = texts(&s);
        assert!(
            v.contains(&(
                "mean of \"Units\", for \"Product\" \"Lamp\"".into(),
                "5.00".into()
            )),
            "{v:?}"
        );
        assert!(v.contains(&(
            "mean of \"Units\", for \"Product\" \"Lamp\": matching rows with no \"Units\" value, left out".into(),
            "1".into()
        )));

        // Counting rows from a date on, grouped by month.
        let s = aggregate(
            &t,
            "sales.csv",
            &query(
                serde_json::json!({"op": "count", "group_by": "Date", "filters": [
                    {"column": "Date", "op": "at_least", "value": "2026-02"}
                ]}),
            ),
        )
        .unwrap();
        let v = texts(&s);
        assert!(
            v.contains(&(
                "rows where \"Date\" is at least \"2026-02\", for \"Date\" \"2026-03\"".into(),
                "4".into()
            )),
            "{v:?}"
        );
        assert!(!v.iter().any(|(l, _)| l.contains("2026-01")));

        // Revenue above 300, median.
        let s = aggregate(
            &t,
            "sales.csv",
            &query(
                serde_json::json!({"op": "median", "measure": "Revenue", "filters": [
                    {"column": "Revenue", "op": "above", "value": "300"}
                ]}),
            ),
        )
        .unwrap();
        assert_eq!(s.facts.last().unwrap().value.text(), "596.0000");
    }

    #[test]
    fn mistakes_are_explained_and_missing_filter_values_counted() {
        let t = sales();
        let err = |j| aggregate(&t, "sales.csv", &query(j)).unwrap_err();
        assert!(
            err(serde_json::json!({"op": "sum", "measure": "Profit"}))
                .contains("The columns are: Date, Region")
        );
        assert!(
            err(serde_json::json!({"op": "sum", "measure": "Region"}))
                .contains("isn't a number column")
        );
        assert!(err(serde_json::json!({"op": "mean"})).contains("needs a `measure`"));
        assert!(err(serde_json::json!({"op": "count", "filters": [{"column": "Units", "op": "above", "value": "many"}]})).contains("isn't one"));
        assert!(err(serde_json::json!({"op": "count", "filters": [{"column": "Date", "op": "is", "value": "March"}]})).contains("2026-10-05"));
        assert!(err(serde_json::json!({"op": "count", "filters": [{"column": "Region", "op": "above", "value": "a"}]})).contains("can't be filtered"));
        assert!(
            err(serde_json::json!({"op": "count", "group_by": "Revenue"}))
                .contains("can't be grouped")
        );
        // Units is empty in one row: a filter on it leaves that row out and says so.
        let s = aggregate(
            &t,
            "sales.csv",
            &query(serde_json::json!({"op": "count", "filters": [
                {"column": "Units", "op": "at_least", "value": "1"}
            ]})),
        )
        .unwrap();
        let v = texts(&s);
        assert!(
            v.contains(&(
                "rows left out because a filtered column was empty".into(),
                "1".into()
            )),
            "{v:?}"
        );
        assert!(v.contains(&("rows where \"Units\" is at least \"1\"".into(), "9".into())));
    }

    #[test]
    fn periods() {
        assert_eq!(
            period("2026"),
            Some((
                Date::new(2026, 1, 1).unwrap(),
                Date::new(2026, 12, 31).unwrap()
            ))
        );
        assert_eq!(
            period("2026-02"),
            Some((
                Date::new(2026, 2, 1).unwrap(),
                Date::new(2026, 2, 28).unwrap()
            ))
        );
        assert_eq!(period("2026-02-30"), None);
        assert_eq!(period("Feb"), None);
    }
}
