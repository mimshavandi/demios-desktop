//! Parsing cell text into numbers and dates, and exact decimal arithmetic.
//!
//! Numbers are kept as scaled integers (`Decimal`) so that sums of money-like columns are
//! exact: `0.10 + 0.20` is `0.30`, not `0.30000000000000004`.

use serde::Serialize;

/// Which character separates decimals in a column's numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecimalMark {
    /// `1,234.56`
    Dot,
    /// `1.234,56`
    Comma,
}

/// A decimal number: `units / 10^scale`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decimal {
    pub units: i128,
    pub scale: u32,
}

/// Most decimals kept exactly; more are rounded.
pub const MAX_SCALE: u32 = 9;

/// Most significant digits a number may have to be computed with. Longer ones (an id, a
/// crafted cell) aren't numbers here, so sums of a whole column and the scaling in means
/// stay far inside `i128` (18 digits × 2·10^5 rows × 10^11 is about 2·10^34).
pub const MAX_DIGITS: usize = 18;

impl Decimal {
    pub fn from_f64(v: f64) -> Option<Self> {
        if !v.is_finite() {
            return None;
        }
        // Shortest representation that round-trips, then exact from its digits.
        parse_plain(&format!("{v}"))
    }

    pub fn to_f64(self) -> f64 {
        self.units as f64 / 10f64.powi(self.scale as i32)
    }

    pub fn rescale(self, scale: u32) -> Option<Self> {
        if scale >= self.scale {
            let f = 10i128.checked_pow(scale - self.scale)?;
            Some(Self {
                units: self.units.checked_mul(f)?,
                scale,
            })
        } else {
            let f = 10i128.pow(self.scale - scale);
            // Round half away from zero.
            let q = self.units / f;
            let r = self.units % f;
            let q = if r.abs() * 2 >= f { q + r.signum() } else { q };
            Some(Self { units: q, scale })
        }
    }

    /// Text with exactly `self.scale` decimals, `.` as the mark, no grouping.
    pub fn text(self) -> String {
        let neg = self.units < 0;
        let digits = self.units.unsigned_abs().to_string();
        let s = self.scale as usize;
        let (int, frac) = if s == 0 {
            (digits, String::new())
        } else if digits.len() > s {
            let (a, b) = digits.split_at(digits.len() - s);
            (a.to_string(), b.to_string())
        } else {
            ("0".to_string(), format!("{digits:0>s$}"))
        };
        let mut out = String::new();
        if neg && self.units != 0 {
            out.push('-');
        }
        out.push_str(&int);
        if s > 0 {
            out.push('.');
            out.push_str(&frac);
        }
        out
    }
}

/// `-12.5`, `+3`, `.5`: digits with at most one `.`, no grouping.
fn parse_plain(s: &str) -> Option<Decimal> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    if !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let significant = int.trim_start_matches('0').len() + frac.len().min(MAX_SCALE as usize);
    if significant > MAX_DIGITS {
        return None;
    }
    if frac.len() > MAX_SCALE as usize {
        // Too many decimals to keep exactly: round to MAX_SCALE.
        let keep = MAX_SCALE as usize + 1;
        let digits = format!("{int}{}", &frac[..keep]);
        let units: i128 = digits.parse().ok()?;
        let exact = Decimal {
            units: if neg { -units } else { units },
            scale: keep as u32,
        };
        return exact.rescale(MAX_SCALE).map(trim_zeros);
    }
    let digits = format!("{int}{frac}");
    let units: i128 = if digits.is_empty() {
        0
    } else {
        digits.parse().ok()?
    };
    Some(Decimal {
        units: if neg { -units } else { units },
        scale: frac.len() as u32,
    })
}

fn trim_zeros(mut d: Decimal) -> Decimal {
    while d.scale > 0 && d.units % 10 == 0 {
        d.units /= 10;
        d.scale -= 1;
    }
    d
}

/// A number in cell text, as written in a column with decimal mark `mark`: grouping by
/// the other mark (or a space, `'` or a thin space) is allowed when the groups are of three;
/// a leading currency sign (`€ $ £`) or a trailing one (`€`) and spaces are dropped.
/// Percentages (`12 %`) are not numbers here: the column profile reports them as text.
pub fn parse_number(text: &str, mark: DecimalMark) -> Option<Decimal> {
    let t = text.trim();
    let t = t
        .trim_start_matches(['€', '$', '£'])
        .trim_end_matches(['€', '$', '£'])
        .trim();
    if t.is_empty() {
        return None;
    }
    let (sign, body) = match t.as_bytes()[0] {
        b'-' => ("-", &t[1..]),
        b'+' => ("", &t[1..]),
        _ => ("", t),
    };
    let body = body.trim_start_matches(['€', '$', '£']).trim();
    let (dec, group) = match mark {
        DecimalMark::Dot => ('.', ','),
        DecimalMark::Comma => (',', '.'),
    };
    let (int, frac) = match body.split_once(dec) {
        Some((a, b)) => (a, Some(b)),
        None => (body, None),
    };
    if let Some(f) = frac
        && (f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let is_sep = |c: char| c == group || c == ' ' || c == '\'' || c == '\u{202f}' || c == '\u{a0}';
    let int_digits: String = if int.contains(is_sep) {
        let parts: Vec<&str> = int.split(is_sep).collect();
        let ok = !parts[0].is_empty()
            && parts[0].len() <= 3
            && parts[0].bytes().all(|b| b.is_ascii_digit())
            && parts[1..]
                .iter()
                .all(|p| p.len() == 3 && p.bytes().all(|b| b.is_ascii_digit()));
        if !ok {
            return None;
        }
        parts.concat()
    } else {
        if !int.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        int.to_string()
    };
    if int_digits.is_empty() && frac.is_none() {
        return None;
    }
    match frac {
        Some(f) => parse_plain(&format!("{sign}{int_digits}.{f}")),
        None => parse_plain(&format!("{sign}{int_digits}")),
    }
}

/// A calendar date.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl Date {
    pub fn new(year: i32, month: u32, day: u32) -> Option<Self> {
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        Some(Self { year, month, day })
    }

    /// `2026-03-05`
    pub fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Days since 1970-01-01 (proleptic Gregorian).
    pub fn days(self) -> i64 {
        let (y, m, d) = (
            i64::from(self.year),
            i64::from(self.month),
            i64::from(self.day),
        );
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ][(month - 1) as usize]
}

/// How a column writes its dates, when they're text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DateOrder {
    /// `2026-03-05` (time after it allowed)
    Iso,
    /// `05.03.2026`, `05/03/2026`
    DayMonth,
    /// `03/05/2026`
    MonthDay,
}

/// The three numbers of a date written with `.`, `/` or `-` between them, year last; the
/// time after a space or `T` is ignored.
fn dmy_parts(text: &str) -> Option<(u32, u32, i32)> {
    let t = text.trim();
    let t = t.split([' ', 'T']).next()?;
    let parts: Vec<&str> = t.split(['.', '/', '-']).collect();
    if parts.len() != 3 || parts[2].len() != 4 || parts[0].len() > 2 || parts[1].len() > 2 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

fn iso_parts(text: &str) -> Option<Date> {
    let t = text.trim();
    let t = t.split([' ', 'T']).next()?;
    let parts: Vec<&str> = t.split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return None;
    }
    Date::new(
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    )
}

pub fn parse_date(text: &str, order: DateOrder) -> Option<Date> {
    match order {
        DateOrder::Iso => iso_parts(text),
        DateOrder::DayMonth => dmy_parts(text).and_then(|(d, m, y)| Date::new(y, m, d)),
        DateOrder::MonthDay => dmy_parts(text).and_then(|(m, d, y)| Date::new(y, m, d)),
    }
}

/// The date order a column's texts use, if every one of them is a date in it. `ambiguous`
/// is true when day/month and month/day would both fit (then day/month is assumed and the
/// fact sheet says so).
pub fn date_order<'a>(texts: impl Iterator<Item = &'a str> + Clone) -> Option<(DateOrder, bool)> {
    if texts.clone().all(|t| iso_parts(t).is_some()) {
        return Some((DateOrder::Iso, false));
    }
    let dm = texts
        .clone()
        .all(|t| parse_date(t, DateOrder::DayMonth).is_some());
    let md = texts
        .clone()
        .all(|t| parse_date(t, DateOrder::MonthDay).is_some());
    match (dm, md) {
        (true, true) => {
            // `05.03.2026` with dots is day/month by convention; slashes are ambiguous.
            let slashes = texts.clone().any(|t| t.contains('/'));
            Some((DateOrder::DayMonth, slashes))
        }
        (true, false) => Some((DateOrder::DayMonth, false)),
        (false, true) => Some((DateOrder::MonthDay, false)),
        (false, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(t: &str, m: DecimalMark) -> Option<String> {
        parse_number(t, m).map(Decimal::text)
    }

    #[test]
    fn numbers_in_both_styles() {
        use DecimalMark::*;
        assert_eq!(n("1,234.56", Dot).as_deref(), Some("1234.56"));
        assert_eq!(n("1.234,56", Comma).as_deref(), Some("1234.56"));
        assert_eq!(n("1234,5", Comma).as_deref(), Some("1234.5"));
        assert_eq!(n("-0.10", Dot).as_deref(), Some("-0.10"));
        assert_eq!(n("€ 12.50", Dot).as_deref(), Some("12.50"));
        assert_eq!(n("12,50 €", Comma).as_deref(), Some("12.50"));
        assert_eq!(n("1 234 567", Dot).as_deref(), Some("1234567"));
        assert_eq!(n("42", Comma).as_deref(), Some("42"));
        // Grouping must be in threes; text isn't a number.
        assert_eq!(n("1,23.4", Dot), None);
        assert_eq!(n("1.234,56", Dot), None);
        assert_eq!(n("12 %", Dot), None);
        assert_eq!(n("abc", Dot), None);
        assert_eq!(n("", Dot), None);
        assert_eq!(n("1.2.3", Dot), None);
        assert_eq!(n("1e3", Dot), None);
        // More than 18 significant digits isn't a number to compute with.
        assert_eq!(
            n("123456789012345678", Dot).as_deref(),
            Some("123456789012345678")
        );
        assert_eq!(n("1234567890123456789", Dot), None);
        assert_eq!(n(&"9".repeat(38), Dot), None);
        assert!(Decimal::from_f64(1e37).is_none());
    }

    #[test]
    fn decimals_sum_exactly_and_round_half_away_from_zero() {
        let a = parse_number("0.10", DecimalMark::Dot).unwrap();
        let b = parse_number("0.2", DecimalMark::Dot).unwrap();
        let s = a.rescale(2).unwrap().units + b.rescale(2).unwrap().units;
        assert_eq!(Decimal { units: s, scale: 2 }.text(), "0.30");
        assert_eq!(
            Decimal {
                units: 125,
                scale: 2
            }
            .rescale(1)
            .unwrap()
            .text(),
            "1.3"
        );
        assert_eq!(
            Decimal {
                units: -125,
                scale: 2
            }
            .rescale(1)
            .unwrap()
            .text(),
            "-1.3"
        );
        assert_eq!(Decimal { units: 5, scale: 3 }.text(), "0.005");
        assert_eq!(
            Decimal::from_f64(0.1 + 0.2)
                .unwrap()
                .rescale(2)
                .unwrap()
                .text(),
            "0.30"
        );
        assert_eq!(Decimal::from_f64(1e-12).unwrap().text(), "0");
        assert_eq!(Decimal::from_f64(2.5).unwrap().text(), "2.5");
    }

    #[test]
    fn dates_and_their_order() {
        assert_eq!(
            parse_date("2026-03-05", DateOrder::Iso),
            Date::new(2026, 3, 5)
        );
        assert_eq!(
            parse_date("2026-03-05T10:00:00", DateOrder::Iso),
            Date::new(2026, 3, 5)
        );
        assert_eq!(
            parse_date("05.03.2026", DateOrder::DayMonth),
            Date::new(2026, 3, 5)
        );
        assert_eq!(parse_date("29.02.2025", DateOrder::DayMonth), None);
        let us = ["03/25/2026", "04/01/2026"];
        assert_eq!(
            date_order(us.iter().copied()),
            Some((DateOrder::MonthDay, false))
        );
        let eu = ["25/03/2026", "01/04/2026"];
        assert_eq!(
            date_order(eu.iter().copied()),
            Some((DateOrder::DayMonth, false))
        );
        let either = ["01/04/2026", "02/05/2026"];
        assert_eq!(
            date_order(either.iter().copied()),
            Some((DateOrder::DayMonth, true))
        );
        let dots = ["01.04.2026", "02.05.2026"];
        assert_eq!(
            date_order(dots.iter().copied()),
            Some((DateOrder::DayMonth, false))
        );
        assert_eq!(date_order(["2026-01-01", "soon"].iter().copied()), None);
        assert_eq!(Date::new(1970, 1, 1).unwrap().days(), 0);
        assert_eq!(
            Date::new(2026, 3, 1).unwrap().days() - Date::new(2026, 2, 1).unwrap().days(),
            28
        );
    }
}
