//! The data tools (#366), shared by the app and `dn eval` so they behave the same:
//!
//! - `data_profile` (capability `data.profile`): the fact sheet of every sheet of a file (or
//!   the one asked for), fact ids unique across sheets (`F1`, `F2`, …);
//! - `data_aggregate` (capability `data.aggregate`): one computed answer the sheet doesn't
//!   have (a filtered sum, a mean per group), as facts `A1`, `A2`, ….
//!
//! The host finds the file and reads its bytes; this returns **plain text**. The rule for
//! the model (quote with the ids, never calculate) is in the tools' descriptions, which are
//! trusted, not in the output, which is data. Wrapping the output as untrusted content is
//! the host's job: dn-runtime does it for every tool result (ADR-0020 §2.4); until a host
//! runs on it, the host calls [`wrap`].

use crate::aggregate::{Filter, Op, Query, aggregate};
use crate::{FactSheet, Limits, Options, Table, kind_of, profile, read};
use dn_core::chat::ToolSpec;
use dn_core::untrusted::{NOTICE, SourceRef, block};
use serde::Deserialize;

pub const TOOL_NAME: &str = "data_profile";
pub const CAPABILITY_ID: &str = "data.profile";
pub const AGGREGATE_TOOL_NAME: &str = "data_aggregate";
pub const AGGREGATE_CAPABILITY_ID: &str = "data.aggregate";

/// Most facts across all sheets of one call, so the result stays a reasonable size.
pub const MAX_FACTS: usize = 800;

/// How the model must use the results; part of both tools' descriptions.
const QUOTE_RULE: &str = "Quote numbers only from the result, each followed by its id in \
                          brackets, e.g. [F12] or [A3]. Never calculate anything yourself, \
                          not even a sum, a difference or a percentage: if the result doesn't \
                          have a number, say so, or ask data_aggregate for it. Say when only \
                          part of a file was read.";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME.into(),
        description: format!(
            "Compute a fact sheet for a CSV or spreadsheet file of this project: rows, \
             missing values, sums, means, quartiles, per-group totals and totals per month, \
             each with an id like [F12]. Use it before saying anything about the numbers in \
             a data file. {QUOTE_RULE}"
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "file": {"type": "string", "description": "The file's name, as listed in the project."},
                "sheet": {"type": "string", "description": "Only this sheet of a spreadsheet (default: every sheet)."}
            },
            "required": ["file"],
            "additionalProperties": false
        }),
    }
}

pub fn aggregate_spec() -> ToolSpec {
    ToolSpec {
        name: AGGREGATE_TOOL_NAME.into(),
        description: format!(
            "Compute one figure from a CSV or spreadsheet file of this project that its fact \
             sheet doesn't have: a sum, mean, median, minimum, maximum or count, over the \
             rows that pass every filter, optionally one per value of a column (or per \
             month of a date column). Each result has an id like [A3] and says how many \
             rows it covers. Call data_profile first to see the columns. {QUOTE_RULE}"
        ),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "file": {"type": "string", "description": "The file's name, as listed in the project."},
                "sheet": {"type": "string", "description": "The sheet, for a spreadsheet with more than one."},
                "op": {"type": "string", "enum": ["sum", "mean", "median", "min", "max", "count"]},
                "measure": {"type": "string", "description": "The number column (not needed to count rows)."},
                "filters": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "column": {"type": "string"},
                            "op": {"type": "string", "enum": ["is", "is_not", "contains", "at_least", "at_most", "above", "below"]},
                            "value": {"type": "string", "description": "Text, a number, or a date as 2026-10-05, a month as 2026-10, a year as 2026."}
                        },
                        "required": ["column", "op", "value"],
                        "additionalProperties": false
                    }
                },
                "group_by": {"type": "string", "description": "A text, yes/no or date column: one result per value (per month for dates)."}
            },
            "required": ["file", "op"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Args {
    pub file: String,
    #[serde(default)]
    pub sheet: Option<String>,
}

impl Args {
    pub fn parse(arguments: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(arguments.clone()).map_err(|e| {
            format!("The arguments don't fit the tool: {e}. Give `file` (and optionally `sheet`).")
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateArgs {
    pub file: String,
    #[serde(default)]
    pub sheet: Option<String>,
    pub op: Op,
    #[serde(default)]
    pub measure: Option<String>,
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub group_by: Option<String>,
}

impl AggregateArgs {
    pub fn parse(arguments: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(arguments.clone()).map_err(|e| {
            format!(
                "The arguments don't fit the tool: {e}. Give `file` and `op`, and `measure`, \
                 `filters` or `group_by` as needed."
            )
        })
    }

    fn query(&self) -> Query {
        Query {
            op: self.op,
            measure: self.measure.clone(),
            filters: self.filters.clone(),
            group_by: self.group_by.clone(),
        }
    }
}

/// What a tool returns: the model's text (plain), and the sheets (for tracing an answer's
/// numbers).
#[derive(Debug)]
pub struct Profiled {
    pub text: String,
    pub sheets: Vec<FactSheet>,
}

/// Whether the tools can read a file with this name.
pub fn reads(name: &str) -> bool {
    kind_of(name).is_some()
}

/// The tables of `bytes`, or only the sheet asked for.
fn tables(name: &str, bytes: &[u8], sheet: Option<&str>) -> Result<Vec<Table>, String> {
    let tables = read(name, bytes, &Limits::default()).map_err(|e| format!("{name}: {e}."))?;
    match sheet {
        None => Ok(tables),
        Some(want) => {
            let names: Vec<String> = tables.iter().filter_map(|t| t.sheet.clone()).collect();
            let found: Vec<Table> = tables
                .into_iter()
                .filter(|t| {
                    t.sheet
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case(want))
                })
                .collect();
            if found.is_empty() {
                Err(if names.is_empty() {
                    format!("{name} has no sheets: leave out `sheet`.")
                } else {
                    format!(
                        "{name} has no sheet called \"{want}\". Its sheets: {}.",
                        names.join(", ")
                    )
                })
            } else {
                Ok(found)
            }
        }
    }
}

/// `data_profile` on `bytes` (the file called `name`). Errors are messages for the model.
pub fn run(name: &str, bytes: &[u8], sheet: Option<&str>) -> Result<Profiled, String> {
    let tables = tables(name, bytes, sheet)?;
    let per_sheet = (MAX_FACTS / tables.len().max(1)).max(100);
    let opts = Options {
        max_facts: per_sheet,
        ..Options::default()
    };
    let mut sheets: Vec<FactSheet> = tables.iter().map(|t| profile(t, name, &opts)).collect();
    // Fact ids unique across the call: sheet 2 continues where sheet 1 stopped.
    let mut next = 1;
    for s in &mut sheets {
        for f in &mut s.facts {
            f.id = format!("F{next}");
            next += 1;
        }
    }
    let text = sheets
        .iter()
        .map(FactSheet::render)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Profiled { text, sheets })
}

/// `data_aggregate` on `bytes` (the file called `name`). Errors are messages for the model.
pub fn run_aggregate(name: &str, bytes: &[u8], args: &AggregateArgs) -> Result<Profiled, String> {
    let mut tables = tables(name, bytes, args.sheet.as_deref())?;
    if tables.len() > 1 {
        let names: Vec<String> = tables.iter().filter_map(|t| t.sheet.clone()).collect();
        return Err(format!(
            "{name} has more than one sheet ({}): say which with `sheet`.",
            names.join(", ")
        ));
    }
    let table = tables.remove(0);
    let sheet = aggregate(&table, name, &args.query())?;
    let mut text = format!("Result for {}", sheet.source);
    if let Some(s) = &sheet.sheet {
        text.push_str(&format!(", sheet {s}"));
    }
    text.push_str(":\n");
    for f in &sheet.facts {
        text.push_str(&format!("[{}] {}: {}\n", f.id, f.label, f.value.text()));
    }
    for n in &sheet.notes {
        text.push_str(&format!("Note: {n}\n"));
    }
    Ok(Profiled {
        text,
        sheets: vec![sheet],
    })
}

/// The result as untrusted content, for a host whose tool results aren't wrapped for it
/// (dn-runtime wraps every tool result itself; this is for hosts until they run on it).
pub fn wrap(capability: &str, file: &str, text: &str) -> String {
    let src = SourceRef::new(format!("tool:{capability}")).attr("file", file);
    format!("{NOTICE}\n{}", block(&src, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_result_is_plain_text_and_the_rule_is_in_the_description() {
        let csv = b"region,revenue\nNorth,10.5\nSouth,4\n";
        let p = run("sales.csv", csv, None).unwrap();
        assert!(p.text.starts_with("Fact sheet for \"sales.csv\""));
        assert!(!p.text.contains("<<<SOURCE"), "the host wraps it");
        assert!(
            !p.text.contains("Never calculate"),
            "instructions don't go in data"
        );
        assert!(p.text.contains("[F1] rows scanned: 2"));
        assert!(spec().description.contains("Never calculate"));
        assert!(aggregate_spec().description.contains("Never calculate"));
        let w = wrap(CAPABILITY_ID, "sales.csv", &p.text);
        assert!(w.starts_with(NOTICE));
        assert!(w.contains("<<<SOURCE tool:data.profile file=\"sales.csv\"\n"));
        assert!(w.trim_end().ends_with("SOURCE>>>"));
    }

    #[test]
    fn sheets_are_chosen_by_name_and_ids_are_unique_across_them() {
        let xlsx = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/orders.xlsx"
        ))
        .unwrap();
        let p = run("orders.xlsx", &xlsx, Some("orders")).unwrap();
        assert!(p.text.contains("sheet \"Orders\""));
        assert!(
            run("orders.xlsx", &xlsx, Some("Nope"))
                .unwrap_err()
                .contains("Its sheets: Orders")
        );
        let a = AggregateArgs::parse(
            &serde_json::json!({"file": "orders.xlsx", "op": "sum", "measure": "Amount"}),
        )
        .unwrap();
        let r = run_aggregate("orders.xlsx", &xlsx, &a).unwrap();
        assert!(
            r.text.contains("[A2] sum of \"Amount\": 60.75"),
            "{}",
            r.text
        );
    }

    #[test]
    fn bad_arguments_and_files_say_what_to_do() {
        assert!(Args::parse(&serde_json::json!({"file": "a.csv"})).is_ok());
        assert!(
            Args::parse(&serde_json::json!({"path": "a.csv"}))
                .unwrap_err()
                .contains("Give `file`")
        );
        assert!(
            AggregateArgs::parse(&serde_json::json!({"file": "a.csv", "op": "total"}))
                .unwrap_err()
                .contains("Give `file` and `op`")
        );
        assert!(
            run("notes.pdf", b"%PDF", None)
                .unwrap_err()
                .contains("isn't a file type")
        );
        assert!(
            run("empty.csv", b"\n", None)
                .unwrap_err()
                .contains("no data")
        );
        assert!(reads("A.XLSX") && reads("b.csv") && !reads("c.pdf"));
    }
}
