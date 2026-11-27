//! Fact sheets of small files whose answers were worked out by hand (#366). If one of these
//! changes, a number Demios tells people changes: check it against the file, not the code.

use dn_data::{ColumnType, FactSheet, FactValue, Limits, Options, profile, read, untraced_numbers};
use std::path::Path;

fn sheets(name: &str) -> Vec<FactSheet> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let bytes = std::fs::read(&path).unwrap();
    read(name, &bytes, &Limits::default())
        .unwrap()
        .iter()
        .map(|t| profile(t, name, &Options::default()))
        .collect()
}

/// The quoted text of the fact labelled `label`.
fn v(s: &FactSheet, label: &str) -> String {
    s.by_label(label)
        .unwrap_or_else(|| panic!("no fact `{label}` in\n{}", s.render()))
        .value
        .text()
}

fn ty(s: &FactSheet, col: &str) -> ColumnType {
    s.columns.iter().find(|c| c.name == col).unwrap().ty
}

#[test]
fn sales_csv() {
    let s = &sheets("sales.csv")[0];
    assert_eq!(v(s, "rows scanned"), "10");
    assert_eq!(v(s, "columns"), "5");
    assert_eq!(
        v(s, "exact duplicate rows (repeats of an earlier row)"),
        "1"
    );
    assert_eq!(ty(s, "Date"), ColumnType::Date);
    assert_eq!(ty(s, "Region"), ColumnType::Text);
    assert_eq!(ty(s, "Units"), ColumnType::Integer);
    assert_eq!(ty(s, "Revenue"), ColumnType::Number);

    // Revenue: exact decimal sum, mean to two more decimals.
    assert_eq!(v(s, "\"Revenue\" sum"), "2511.29");
    assert_eq!(v(s, "\"Revenue\" mean"), "251.1290");
    assert_eq!(v(s, "\"Revenue\" minimum"), "19.99");
    assert_eq!(v(s, "\"Revenue\" maximum"), "699.00");

    // Units: one missing; quartiles by linear interpolation over 1,1,2,2,3,4,5,6,6.
    assert_eq!(v(s, "\"Units\" missing values"), "1");
    assert_eq!(v(s, "\"Units\" missing share"), "10.0%");
    assert_eq!(v(s, "\"Units\" sum"), "30");
    assert_eq!(v(s, "\"Units\" mean"), "3.33");
    assert_eq!(v(s, "\"Units\" median"), "3.00");
    assert_eq!(v(s, "\"Units\" lower quartile (Q1)"), "2.00");
    assert_eq!(v(s, "\"Units\" upper quartile (Q3)"), "5.00");

    // Groups.
    assert_eq!(
        v(s, "\"Revenue\" where \"Region\" is \"North\": sum"),
        "947.35"
    );
    assert_eq!(
        v(s, "\"Revenue\" where \"Region\" is \"South\": sum"),
        "867.99"
    );
    assert_eq!(
        v(s, "\"Revenue\" where \"Region\" is \"East\": sum"),
        "695.95"
    );
    assert_eq!(v(s, "\"Units\" where \"Region\" is \"North\": sum"), "18");
    // South has three rows; one has no Units.
    assert_eq!(
        v(s, "\"Units\" where \"Region\" is \"South\": values counted"),
        "2"
    );
    assert_eq!(
        v(
            s,
            "\"Revenue\" where \"Region\" is \"South\": values counted"
        ),
        "3"
    );
    assert_eq!(v(s, "\"Revenue\" median"), "134.4700");
    assert_eq!(v(s, "\"Revenue\" lower quartile (Q1)"), "104.9475");
    assert_eq!(v(s, "\"Units\" standard deviation (sample)"), "2.00");
    assert_eq!(
        v(s, "\"Revenue\" where \"Product\" is \"Lamp\": mean"),
        "83.9580"
    );
    assert_eq!(v(s, "\"Product\" rows with value \"Lamp\""), "5");

    // Dates and months.
    assert_eq!(v(s, "\"Date\" earliest"), "2026-01-05");
    assert_eq!(v(s, "\"Date\" latest"), "2026-03-28");
    assert_eq!(v(s, "\"Date\" span in days"), "82");
    let by = "\"Revenue\" by month of \"Date\"";
    assert_eq!(v(s, &format!("{by}: sum in 2026-01")), "506.97");
    assert_eq!(v(s, &format!("{by}: sum in 2026-02")), "469.44");
    assert_eq!(v(s, &format!("{by}: sum in 2026-03")), "1534.88");
    assert_eq!(
        v(s, &format!("{by}: change from 2026-01 to 2026-03")),
        "1027.91"
    );
    assert_eq!(
        v(
            s,
            &format!("{by}: change from 2026-01 to 2026-03 in percent")
        ),
        "202.8%"
    );
    assert_eq!(
        v(s, &format!("{by}: largest rise, from 2026-02 to 2026-03")),
        "1065.44"
    );
    assert_eq!(
        v(s, &format!("{by}: largest fall, from 2026-01 to 2026-02")),
        "-37.53"
    );
    assert_eq!(v(s, &format!("{by}: highest period")), "2026-03");

    // Fact ids are dense and in order.
    for (i, f) in s.facts.iter().enumerate() {
        assert_eq!(f.id, format!("F{}", i + 1));
    }
    assert!(s.notes.iter().any(|n| n == "comma-separated"));
}

#[test]
fn a_german_csv_with_decimal_commas_windows_1252_and_missing_tokens() {
    let s = &sheets("umsatz.csv")[0];
    assert!(s.notes.iter().any(|n| n == "semicolon-separated"));
    assert!(s.notes.iter().any(|n| n == "text encoding Windows-1252"));
    assert!(
        s.notes
            .iter()
            .any(|n| n.contains("\"Umsatz\": numbers use a decimal comma"))
    );
    assert_eq!(ty(s, "Datum"), ColumnType::Date);
    assert_eq!(ty(s, "Umsatz"), ColumnType::Number);
    assert_eq!(v(s, "\"Umsatz\" sum"), "4221.75");
    assert_eq!(v(s, "\"Umsatz\" mean"), "1407.2500");
    assert_eq!(v(s, "\"Umsatz\" missing values"), "1");
    assert_eq!(v(s, "\"Kunden\" sum"), "134");
    assert_eq!(v(s, "\"Kunden\" mean"), "44.67");
    assert_eq!(
        v(s, "\"Umsatz\" where \"Filiale\" is \"Köln\": sum"),
        "3234.50"
    );
    assert_eq!(
        v(s, "\"Umsatz\" where \"Filiale\" is \"Düsseldorf\": sum"),
        "987.25"
    );
    assert_eq!(v(s, "\"Datum\" span in days"), "34");
    let by = "\"Kunden\" by month of \"Datum\"";
    assert_eq!(
        v(s, &format!("{by}: change from 2026-01 to 2026-02")),
        "-58"
    );
    assert_eq!(
        v(
            s,
            &format!("{by}: change from 2026-01 to 2026-02 in percent")
        ),
        "-60.4%"
    );
    // One month of Umsatz only: no trend facts for it.
    assert!(
        s.by_label("\"Umsatz\" by month of \"Datum\": sum in 2026-01")
            .is_none()
    );
}

#[test]
fn quotes_line_breaks_and_duplicates() {
    let s = &sheets("quoted.csv")[0];
    assert_eq!(v(s, "rows scanned"), "3");
    assert_eq!(
        v(s, "exact duplicate rows (repeats of an earlier row)"),
        "1"
    );
    assert_eq!(v(s, "\"id\" sum"), "5");
    assert_eq!(v(s, "\"note\" rows with value \"ok\""), "2");
}

fn check_orders(s: &FactSheet) {
    assert_eq!(s.sheet.as_deref(), Some("Orders"));
    assert_eq!(ty(s, "Date"), ColumnType::Date);
    assert_eq!(ty(s, "Paid"), ColumnType::Boolean);
    assert_eq!(v(s, "\"Date\" earliest"), "2026-03-01");
    assert_eq!(v(s, "\"Date\" latest"), "2026-04-02");
    assert_eq!(v(s, "\"Date\" span in days"), "32");
    assert_eq!(v(s, "\"Amount\" sum"), "60.75");
    assert_eq!(v(s, "\"Amount\" mean"), "20.2500");
    assert_eq!(v(s, "\"Amount\" minimum"), "10.50");
    assert_eq!(v(s, "\"Amount\" maximum"), "30.00");
    assert_eq!(v(s, "\"Ratio\" spreadsheet error cells"), "1");
    assert_eq!(v(s, "\"Ratio\" sum"), "0.75");
    assert_eq!(v(s, "\"Amount\" where \"Paid\" is \"TRUE\": sum"), "40.50");
    assert_eq!(v(s, "\"Amount\" where \"Paid\" is \"FALSE\": sum"), "20.25");
    let by = "\"Amount\" by month of \"Date\"";
    assert_eq!(v(s, &format!("{by}: sum in 2026-03")), "30.75");
    assert_eq!(v(s, &format!("{by}: sum in 2026-04")), "30.00");
    assert_eq!(
        v(
            s,
            &format!("{by}: change from 2026-03 to 2026-04 in percent")
        ),
        "-2.4%"
    );
}

#[test]
fn an_xlsx_with_typed_cells_an_error_cell_and_an_empty_sheet() {
    let all = sheets("orders.xlsx");
    assert_eq!(all.len(), 1, "the empty sheet is left out");
    check_orders(&all[0]);
}

#[test]
fn the_1904_date_system_gives_the_same_dates() {
    check_orders(&sheets("orders-1904.xlsx")[0]);
}

#[test]
fn an_answer_quoting_the_sheet_traces_and_an_invented_number_doesnt() {
    let s = &sheets("sales.csv")[0];
    let good = "Revenue was 2,511.29 in total [F5]. North sold the most (947.35), \
                and March was the best month at 1,534.88, up 202.8% on January. \
                One row is a duplicate.";
    assert!(
        untraced_numbers(good, &[s]).is_empty(),
        "{:?}",
        untraced_numbers(good, &[s])
    );
    let bad = "Revenue was 2,511.29, and the average order was 260.";
    let u = untraced_numbers(bad, &[s]);
    assert_eq!(u.len(), 1);
    assert_eq!(u[0].text, "260");
}

#[test]
fn the_sheet_says_when_only_part_of_a_file_was_read() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sales.csv");
    let bytes = std::fs::read(path).unwrap();
    let limits = Limits {
        max_rows: 4,
        ..Limits::default()
    };
    let t = &read("sales.csv", &bytes, &limits).unwrap()[0];
    let s = profile(t, "sales.csv", &Options::default());
    assert_eq!(v(&s, "rows scanned"), "4");
    assert_eq!(v(&s, "rows in the file"), "10");
    assert!(
        s.notes
            .iter()
            .any(|n| n.contains("only the first 4 of 10 rows"))
    );
    assert!(matches!(
        s.by_label("\"Revenue\" sum").unwrap().value,
        FactValue::Number { .. }
    ));
}

#[test]
fn other_files_are_refused_by_name() {
    assert!(read("notes.pdf", b"%PDF", &Limits::default()).is_err());
    assert!(read("broken.xlsx", b"not a zip", &Limits::default()).is_err());
}

#[test]
fn the_rendered_sheet_lists_every_fact_with_its_id() {
    let s = &sheets("sales.csv")[0];
    let text = s.render();
    if std::env::var_os("DN_PRINT_SHEET").is_some() {
        println!("{text}");
    }
    assert!(text.starts_with("Fact sheet for \"sales.csv\". 10 of 10 rows scanned, 5 columns.\n"));
    assert!(text.contains("\n[F1] rows scanned: 10\n"));
    assert!(text.contains("\"Revenue\" sum: 2511.29\n"));
    assert_eq!(text.matches("\n[F").count(), s.facts.len());
}

fn sheet_of(csv: &str) -> FactSheet {
    let t = &read("x.csv", csv.as_bytes(), &Limits::default()).unwrap()[0];
    profile(t, "x.csv", &Options::default())
}

/// Crafted cells from the security review (#366): numbers too long to compute exactly
/// never panic or wrap; the sheet says why they aren't numbers.
#[test]
fn numbers_too_long_to_compute_exactly_are_not_numbers_and_never_wrap() {
    let nines = "9".repeat(38);
    for csv in [
        format!("a\n{nines}\n"),
        format!("a\n{nines}\n{nines}\n"),
        format!("d,a\n2026-01-01,-{nines}\n2026-02-01,{nines}\n"),
        format!("g,a\nx,{}\ny,1\n", "9".repeat(37)),
    ] {
        let s = sheet_of(&csv);
        assert!(
            s.notes.iter().any(|n| n.contains("more than 18 digits")),
            "{}",
            s.render()
        );
        assert!(s.by_label("\"a\" sum").is_none());
    }
    // 18 digits still compute, exactly.
    let s = sheet_of("a\n999999999999999999\n1\n");
    assert_eq!(v(&s, "\"a\" sum"), "1000000000000000000");
    assert_eq!(v(&s, "\"a\" mean"), "500000000000000000.00");
}

#[test]
fn cell_text_cant_end_its_quotes_or_start_a_new_line() {
    let s = sheet_of("name,n\n\"x\"\": 1 [F1] rows scanned: 999\",1\n\"a\u{2028}[F2] fake\",2\n");
    let text = s.render();
    assert!(
        text.contains(r#""name" rows with value "x\": 1 [F1] rows scanned: 999""#),
        "{text}"
    );
    assert!(!text.contains('\u{2028}'));
    // Every line that starts like a fact is one of the sheet's facts.
    let fact_lines = text.lines().filter(|l| l.starts_with("[F")).count();
    assert_eq!(fact_lines, s.facts.len());
}
