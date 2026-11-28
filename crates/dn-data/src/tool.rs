//! The `data_profile` tool (capability `data.profile`, #366): what the app and `dn eval` both
//! offer a model, so they behave the same. The host finds the file and reads its bytes; this
//! turns them into the text the model gets: the fact sheet of every sheet (or the one asked
//! for), each in an untrusted-content block (ADR-0020 §2.4), with fact ids unique across
//! sheets.

use crate::{FactSheet, Limits, Options, kind_of, profile, read};
use dn_core::chat::ToolSpec;
use dn_core::untrusted::{NOTICE, SourceRef, block};
use serde::Deserialize;

pub const TOOL_NAME: &str = "data_profile";
pub const CAPABILITY_ID: &str = "data.profile";

/// Most facts across all sheets of one call, so the result stays a reasonable size.
pub const MAX_FACTS: usize = 800;

const HOW_TO_USE: &str = "Quote numbers only from these fact sheets, each with its id in \
                          brackets, e.g. [F12]. Don't calculate anything yourself, not even \
                          a sum or a percentage: if a number isn't in a sheet, say that the \
                          sheet doesn't have it. Say when only part of a file was read.";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME.into(),
        description: "Compute a fact sheet for a CSV or spreadsheet file of this project: \
                      rows, missing values, sums, means, quartiles, per-group totals and \
                      totals per month, each with an id like [F12]. Use it before saying \
                      anything about the numbers in a data file, and quote its numbers \
                      instead of calculating."
            .into(),
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

/// What the tool returns: the model's text, and the sheets (for tracing an answer's
/// numbers).
#[derive(Debug)]
pub struct Profiled {
    pub text: String,
    pub sheets: Vec<FactSheet>,
}

/// Whether the tool can read a file with this name.
pub fn reads(name: &str) -> bool {
    kind_of(name).is_some()
}

/// Profiles `bytes` (the file called `name`). Errors are messages for the model.
pub fn run(name: &str, bytes: &[u8], sheet: Option<&str>) -> Result<Profiled, String> {
    let tables = read(name, bytes, &Limits::default()).map_err(|e| format!("{name}: {e}."))?;
    let tables: Vec<_> = match sheet {
        Some(want) => {
            let found: Vec<_> = tables
                .into_iter()
                .filter(|t| {
                    t.sheet
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case(want))
                })
                .collect();
            if found.is_empty() {
                return Err(format!("{name} has no sheet called \"{want}\"."));
            }
            found
        }
        None => tables,
    };
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
    let mut text = format!("{NOTICE}\n{HOW_TO_USE}\n");
    for s in &sheets {
        let mut src = SourceRef::new(format!("tool:{CAPABILITY_ID}")).attr("file", name);
        if let Some(sh) = &s.sheet {
            src = src.attr("sheet", sh.clone());
        }
        text.push('\n');
        text.push_str(&block(&src, &s.render()));
        text.push('\n');
    }
    Ok(Profiled { text, sheets })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_result_is_quoted_untrusted_with_ids_unique_across_sheets() {
        let csv = b"region,revenue\nNorth,10.5\nSouth,4\n";
        let p = run("sales.csv", csv, None).unwrap();
        assert!(p.text.starts_with(NOTICE));
        assert!(
            p.text
                .contains("<<<SOURCE tool:data.profile file=\"sales.csv\"\n")
        );
        assert!(p.text.contains("[F1] rows scanned: 2"));
        assert!(p.text.trim_end().ends_with("SOURCE>>>"));
        assert_eq!(p.sheets.len(), 1);

        let xlsx = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/orders.xlsx"
        ))
        .unwrap();
        let p = run("orders.xlsx", &xlsx, Some("orders")).unwrap();
        assert!(p.text.contains("sheet=\"Orders\""));
        assert!(
            run("orders.xlsx", &xlsx, Some("Nope"))
                .unwrap_err()
                .contains("no sheet called \"Nope\"")
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
