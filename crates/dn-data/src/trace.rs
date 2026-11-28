//! Whether the numbers in an answer come from the fact sheet.
//!
//! Every number written in the answer must equal a fact's value at the precision the answer
//! shows (`12.35` matches `12.3456`; `12.4` doesn't match `12.3456`), in either decimal
//! style (`1,234.5` or `1.234,5`), with an optional `%` (matched against percentage facts)
//! or `k` / `M` suffix. Not counted as numbers: fact ids (`[F12]`), dates and periods
//! (`2026-03`, `05.03.2026`), years on their own (1900–2100) and list numbering at the start
//! of a line (`1.`, `2)`).

use crate::profile::{FactSheet, FactValue};
use serde::Serialize;

/// A number in the answer that no fact has.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Untraced {
    /// As written.
    pub text: String,
    /// Byte offset in the answer.
    pub at: usize,
}

/// A number as written: its candidate values (one per decimal style that reads it) and the
/// precision shown.
#[derive(Debug)]
struct Written {
    text: String,
    at: usize,
    /// (value, decimals shown)
    readings: Vec<(f64, u32)>,
    percent: bool,
    factor: f64,
}

fn readings(core: &str) -> Vec<(f64, u32)> {
    let mut out = Vec::new();
    for (dec, group) in [('.', ','), (',', '.')] {
        let (int, frac) = match core.rsplit_once(dec) {
            Some((a, b)) if !b.contains(group) => (a, Some(b)),
            _ => (core, None),
        };
        if frac.is_some_and(|f| f.is_empty() || f.contains([dec, group])) {
            continue;
        }
        let groups: Vec<&str> = int
            .split([group, ' ', '\u{202f}', '\u{a0}', '\''])
            .collect();
        let grouped = groups.len() > 1;
        if grouped
            && (groups[0].is_empty()
                || groups[0].len() > 3
                || groups[1..].iter().any(|g| g.len() != 3))
        {
            continue;
        }
        if int.contains(dec) {
            continue;
        }
        let digits: String = groups.concat();
        if !digits.bytes().all(|b| b.is_ascii_digit()) || digits.is_empty() {
            continue;
        }
        let f = frac.unwrap_or("");
        if let Ok(v) = format!("{digits}.{f}0").parse::<f64>() {
            let r = (v, f.len() as u32);
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out
}

fn is_date_like(s: &str) -> bool {
    let parts: Vec<&str> = s.split(['-', '.', '/']).collect();
    let digits = |p: &&str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    match parts.as_slice() {
        // 2026-03, 2026-03-05
        [y, m] | [y, m, _]
            if y.len() == 4 && m.len() == 2 && s.contains('-') && parts.iter().all(digits) =>
        {
            true
        }
        // 05.03.2026, 5/3/2026
        [d, m, y] if y.len() == 4 && d.len() <= 2 && m.len() <= 2 && parts.iter().all(digits) => {
            true
        }
        _ => false,
    }
}

fn scan(answer: &str) -> Vec<Written> {
    let b = answer.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if !c.is_ascii_digit() {
            i += 1;
            continue;
        }
        // Fact ids: `F12`.
        if i > 0
            && (b[i - 1] == b'F' || b[i - 1] == b'S')
            && (i < 2 || !b[i - 2].is_ascii_alphanumeric())
        {
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            continue;
        }
        // Part of a word (e.g. `Q3`, `H2O`): not a number.
        if i > 0 && (b[i - 1].is_ascii_alphabetic() || b[i - 1] == b'_') {
            while i < b.len() && b[i].is_ascii_alphanumeric() {
                i += 1;
            }
            continue;
        }
        let start = i;
        let mut end = i;
        // Digits and separators, a separator only between digits.
        while end < b.len() {
            let ch = b[end];
            let separator = matches!(ch, b'.' | b',' | b'\'' | b'-' | b'/' | b' ')
                && end + 1 < b.len()
                && b[end + 1].is_ascii_digit()
                && (ch != b' ' || is_group_space(b, start, end));
            if !(ch.is_ascii_digit() || separator) {
                break;
            }
            end += 1;
        }
        let token = &answer[start..end];
        i = end;
        if is_date_like(token) {
            continue;
        }
        if token.contains(['-', '/']) {
            // Ranges like `10-20`: each side is its own number.
            let mut off = start;
            for part in token.split(['-', '/']) {
                if !part.is_empty() {
                    push_number(answer, &mut out, part, off, end);
                }
                off += part.len() + 1;
            }
            continue;
        }
        // List numbering: `1.` or `2)` first on a line.
        let line_start = answer[..start].rfind('\n').map_or(0, |p| p + 1);
        let before = answer[line_start..start].trim();
        let after = answer[end..].chars().next();
        if (before.is_empty() || before == "-" || before == "*")
            && matches!(after, Some('.') | Some(')'))
            && answer[end + 1..].starts_with([' ', '\t'])
            && !token.contains(['.', ','])
        {
            continue;
        }
        push_number(answer, &mut out, token, start, end);
    }
    out
}

/// A space groups thousands only between a 1–3 digit group and a 3-digit group.
fn is_group_space(b: &[u8], start: usize, at: usize) -> bool {
    let prev = &b[start..at];
    let last_group = prev.rsplit(|c| !c.is_ascii_digit()).next().unwrap_or(&[]);
    let next: Vec<u8> = b[at + 1..]
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();
    (1..=3).contains(&last_group.len()) && next.len() == 3
}

fn push_number(answer: &str, out: &mut Vec<Written>, token: &str, start: usize, end: usize) {
    let t = token.trim_end_matches([',', '.']).trim();
    if t.is_empty() {
        return;
    }
    let rest = answer[end..].trim_start_matches([' ', '\u{a0}']);
    let percent =
        rest.starts_with('%') || rest.starts_with("percent") || rest.starts_with("Prozent");
    let factor =
        if rest.starts_with(['k', 'K']) && !rest[1..].starts_with(|c: char| c.is_alphabetic()) {
            1e3
        } else if (rest.starts_with('M') && !rest[1..].starts_with(|c: char| c.is_alphabetic()))
            || rest.starts_with("Mio")
            || rest.starts_with("million")
        {
            1e6
        } else {
            1.0
        };
    let readings = readings(t);
    if readings.is_empty() {
        return;
    }
    // A year on its own.
    if !percent
        && factor == 1.0
        && t.len() == 4
        && t.bytes().all(|c| c.is_ascii_digit())
        && (1900..=2100).contains(&t.parse::<u32>().unwrap_or(0))
    {
        return;
    }
    out.push(Written {
        text: answer[start..start + t.len()].to_string(),
        at: start,
        readings,
        percent,
        factor,
    });
}

fn matches(w: &Written, value: f64) -> bool {
    w.readings.iter().any(|(a, decimals)| {
        let tol = 0.5 * 10f64.powi(-(*decimals as i32)) * w.factor + 1e-9 * value.abs().max(1.0);
        (a * w.factor - value).abs() <= tol
    })
}

/// Whether `answer` writes `value` (at the precision the answer shows; as a percentage when
/// `percent`). For checking known answers in evals.
pub fn mentions(answer: &str, value: f64, percent: bool) -> bool {
    scan(answer)
        .iter()
        .any(|w| (w.percent || !percent) && matches(w, value))
}

/// The numbers in `answer` that no fact of `sheets` has.
pub fn untraced_numbers(answer: &str, sheets: &[&FactSheet]) -> Vec<Untraced> {
    let mut numbers = Vec::new();
    let mut percents = Vec::new();
    for s in sheets {
        for f in &s.facts {
            match f.value {
                FactValue::Number { value, .. } => numbers.push(value),
                FactValue::Percent { value, .. } => percents.push(value),
                _ => {}
            }
        }
    }
    scan(answer)
        .into_iter()
        .filter(|w| {
            // `12.5%` matches percentage facts only; a plain number may match either kind
            // (`12.5 of 100 rows are missing` quotes a share).
            let found = percents.iter().any(|v| matches(w, *v))
                || (!w.percent && numbers.iter().any(|v| matches(w, *v)));
            !found
        })
        .map(|w| Untraced {
            text: w.text,
            at: w.at,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Fact;

    fn sheet(facts: &[(&str, FactValue)]) -> FactSheet {
        FactSheet {
            source: "t.csv".into(),
            sheet: None,
            rows_scanned: 0,
            rows_total: 0,
            columns: vec![],
            facts: facts
                .iter()
                .enumerate()
                .map(|(i, (l, v))| Fact {
                    id: format!("F{}", i + 1),
                    label: (*l).into(),
                    value: v.clone(),
                })
                .collect(),
            notes: vec![],
        }
    }

    fn num(v: f64) -> FactValue {
        FactValue::Number {
            text: v.to_string(),
            value: v,
        }
    }

    fn texts(answer: &str, s: &FactSheet) -> Vec<String> {
        untraced_numbers(answer, &[s])
            .into_iter()
            .map(|u| u.text)
            .collect()
    }

    #[test]
    fn numbers_match_at_the_precision_written() {
        let s = sheet(&[
            ("sum", num(12345.5)),
            ("mean", num(12.3456)),
            ("rows", num(250.0)),
            (
                "share",
                FactValue::Percent {
                    text: "12.5".into(),
                    value: 12.5,
                },
            ),
        ]);
        assert!(
            texts(
                "The total is 12,345.50 [F1], or 12.345,5 in German style.",
                &s
            )
            .is_empty()
        );
        assert!(texts("On average 12.35 per row (about 12).", &s).is_empty());
        assert!(texts("Missing: 12.5% of 250 rows; that's 12.5 percent.", &s).is_empty());
        assert!(texts("Roughly 12.3k in total.", &s).is_empty());
        // 12,346 would be a fair rounding of 12,345.5; 12,350 isn't.
        assert!(texts("The total is about 12,346.", &s).is_empty());
        assert_eq!(
            texts("The mean is 12.4 and the total 12,350.", &s),
            ["12.4", "12,350"]
        );
        assert_eq!(texts("Up 7% since March.", &s), ["7"]);
    }

    #[test]
    fn mentions_finds_a_known_answer_at_the_precision_written() {
        assert!(mentions("Revenue was 2,511.29 in total.", 2511.29, false));
        assert!(mentions("About 2,511 in total.", 2511.29, false));
        assert!(!mentions("About 2,600 in total.", 2511.29, false));
        assert!(mentions("Up 202.8% on January.", 202.8, true));
        assert!(!mentions("Up 202.8 on January.", 202.8, true));
    }

    #[test]
    fn ids_dates_years_and_list_numbers_are_not_numbers() {
        let s = sheet(&[("rows", num(250.0))]);
        let answer = "1. In 2026 the data runs from 2026-01-05 to 05.03.2026 [F1] [F12].\n2) 250 rows in Q3.\n- 3. tip";
        assert!(texts(answer, &s).is_empty(), "{:?}", texts(answer, &s));
        assert_eq!(texts("Between 240-260 rows.", &s), ["240", "260"]);
    }
}
