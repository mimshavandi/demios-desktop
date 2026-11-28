//! The data tools in project chats (#366): `data_profile` (a fact sheet for one of the
//! project's CSV or spreadsheet files) and `data_aggregate` (one computed figure the sheet
//! doesn't have), both computed by dn-data, so the model quotes numbers instead of
//! calculating them. Read-only; they read only the local copies of the chat's own project
//! files, found by their name, never a path.

use crate::state::AppState;
use dn_agent::ToolOutput;
use dn_core::Id;
use dn_core::chat::{ToolCall, ToolSpec};
use dn_data::tool::{AGGREGATE_CAPABILITY_ID, AGGREGATE_TOOL_NAME};
use std::sync::Arc;

pub use dn_data::tool::{CAPABILITY_ID, TOOL_NAME};

/// Whether `name` is one of these tools.
pub fn is_data_tool(name: &str) -> bool {
    name == TOOL_NAME || name == AGGREGATE_TOOL_NAME
}

/// The capability id of a data tool, as manifests and skills name it.
pub fn capability_id(name: &str) -> Option<&'static str> {
    match name {
        TOOL_NAME => Some(CAPABILITY_ID),
        AGGREGATE_TOOL_NAME => Some(AGGREGATE_CAPABILITY_ID),
        _ => None,
    }
}

pub struct DataTools {
    pub state: Arc<AppState>,
    pub project_id: Id,
}

impl DataTools {
    /// The tools the assistant's manifest allows.
    pub fn specs(&self) -> Vec<ToolSpec> {
        let m = &self.state.agent.manifest;
        let mut v = Vec::new();
        if m.allows_tool(CAPABILITY_ID) {
            v.push(dn_data::tool::spec());
        }
        if m.allows_tool(AGGREGATE_CAPABILITY_ID) {
            v.push(dn_data::tool::aggregate_spec());
        }
        v
    }

    pub fn offers(&self, name: &str) -> bool {
        self.specs().iter().any(|s| s.name == name)
    }

    /// Runs a call. Every failure is a message the model can act on (a missing file names
    /// the project's data files).
    pub async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let request = match call.name.as_str() {
            TOOL_NAME => dn_data::tool::Args::parse(&call.arguments).map(Request::Profile),
            AGGREGATE_TOOL_NAME => {
                dn_data::tool::AggregateArgs::parse(&call.arguments).map(Request::Aggregate)
            }
            other => Err(format!("`{other}` isn't a data tool.")),
        };
        let request = match request {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(e),
        };
        let state = Arc::clone(&self.state);
        let project_id = self.project_id;
        // File reading and parsing are blocking, and a malformed workbook could make the
        // parser panic: off the async workers, and a panic is an error, not a crash.
        let r =
            tauri::async_runtime::spawn_blocking(move || run(&state, project_id, &request)).await;
        match r {
            Ok(Ok(text)) => ToolOutput::text(text),
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => {
                tracing::warn!("{} failed: {e}", call.name);
                ToolOutput::error("The file couldn't be read as data.")
            }
        }
    }
}

enum Request {
    Profile(dn_data::tool::Args),
    Aggregate(dn_data::tool::AggregateArgs),
}

impl Request {
    fn file(&self) -> &str {
        match self {
            Request::Profile(a) => &a.file,
            Request::Aggregate(a) => &a.file,
        }
    }
}

fn run(state: &AppState, project_id: Id, request: &Request) -> Result<String, String> {
    let wanted = request.file();
    let files = {
        let db = state.db().map_err(|e| e.message)?;
        dn_store::project::list_files(&db, project_id).map_err(|e| e.to_string())?
    };
    let data_files: Vec<&str> = files
        .iter()
        .filter(|f| dn_data::tool::reads(&f.name))
        .map(|f| f.name.as_str())
        .collect();
    let Some(file) = files
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case(wanted.trim()))
    else {
        return Err(if data_files.is_empty() {
            format!(
                "This project has no file called \"{wanted}\", and no CSV or spreadsheet files."
            )
        } else {
            format!(
                "This project has no file called \"{wanted}\". Its data files: {}.",
                data_files.join(", ")
            )
        });
    };
    if !dn_data::tool::reads(&file.name) {
        return Err(format!(
            "\"{}\" isn't a CSV or spreadsheet file, so it has no data to compute with.",
            file.name
        ));
    }
    let root = state
        .project_files_root()
        .ok_or("Project files aren't available here.")?;
    let bytes = std::fs::read(dn_store::project::file_path(root, file)).map_err(|e| {
        tracing::warn!("data tool: a project file's copy can't be read: {e}");
        format!("The copy of \"{}\" can't be read.", file.name)
    })?;
    let (capability, result) = match request {
        Request::Profile(a) => (
            CAPABILITY_ID,
            dn_data::tool::run(&file.name, &bytes, a.sheet.as_deref()),
        ),
        Request::Aggregate(a) => (
            AGGREGATE_CAPABILITY_ID,
            dn_data::tool::run_aggregate(&file.name, &bytes, a),
        ),
    };
    // Chat doesn't run on dn-runtime yet, which wraps every tool result: wrap it here, once.
    // (Removed when chat moves onto the runtime, ADR-0020 R1.)
    result.map(|p| dn_data::tool::wrap(capability, &file.name, &p.text))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state with a project and its files folder, holding `files` as (name, content).
    fn state_with(files: &[(&str, &[u8])]) -> (Arc<AppState>, Id, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("dn-data-tool-{}", Id::new()));
        let st = AppState::open_for_tests_with_agent(&dir.join("db.sqlite3")).unwrap();
        let project = {
            let db = st.db().unwrap();
            dn_store::project::create_project(&db, "Shop").unwrap()
        };
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let paths: Vec<std::path::PathBuf> = files
            .iter()
            .map(|(n, c)| {
                let p = src.join(n);
                std::fs::write(&p, c).unwrap();
                p
            })
            .collect();
        crate::projects::add_files(&st, project.id, &paths).unwrap();
        (st, project.id, dir)
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: TOOL_NAME.into(),
            arguments: args,
        }
    }

    #[test]
    fn a_project_csv_gets_its_fact_sheet_and_other_files_dont() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (st, project, dir) = state_with(&[
            ("sales.csv", b"region,revenue\nNorth,10.50\nSouth,4\n"),
            ("notes.md", b"# Notes"),
        ]);
        let tools = DataTools {
            state: Arc::clone(&st),
            project_id: project,
        };
        assert_eq!(
            tools.specs().len(),
            2,
            "the default assistant allows data.profile and data.aggregate"
        );
        rt.block_on(async {
            let out = tools
                .execute(&call(serde_json::json!({"file": "SALES.csv"})))
                .await;
            assert!(!out.is_error);
            let dn_core::chat::ContentPart::Text { text } = &out.content[0] else {
                panic!()
            };
            assert!(text.contains("\"revenue\" sum: 14.50"), "{text}");
            assert!(text.contains("<<<SOURCE tool:data.profile file=\"sales.csv\""));

            let out = tools
                .execute(&call(serde_json::json!({"file": "missing.csv"})))
                .await;
            assert!(out.is_error);
            assert!(format!("{:?}", out.content).contains("Its data files: sales.csv"));

            let out = tools
                .execute(&call(serde_json::json!({"file": "notes.md"})))
                .await;
            assert!(out.is_error);
            assert!(format!("{:?}", out.content).contains("isn't a CSV or spreadsheet"));

            // A path is only ever a name to look up: nothing outside the project is read.
            let out = tools
                .execute(&call(serde_json::json!({"file": "../../etc/passwd"})))
                .await;
            assert!(out.is_error);
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn data_aggregate_answers_a_filtered_question_wrapped_once() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (st, project, dir) = state_with(&[(
            "sales.csv",
            b"region,revenue\nNorth,10.50\nSouth,4\nNorth,2.25\n",
        )]);
        let tools = DataTools {
            state: st,
            project_id: project,
        };
        assert!(tools.offers(dn_data::tool::AGGREGATE_TOOL_NAME));
        rt.block_on(async {
            let out = tools
                .execute(&ToolCall {
                    id: "c".into(),
                    name: AGGREGATE_TOOL_NAME.into(),
                    arguments: serde_json::json!({"file": "sales.csv", "op": "sum", "measure": "revenue",
                        "filters": [{"column": "region", "op": "is", "value": "north"}]}),
                })
                .await;
            assert!(!out.is_error, "{:?}", out.content);
            let dn_core::chat::ContentPart::Text { text } = &out.content[0] else {
                panic!()
            };
            assert!(text.contains("sum of \"revenue\" where \"region\" is \"north\": 12.75"), "{text}");
            assert_eq!(text.matches("\n<<<SOURCE ").count(), 1, "wrapped once: {text}");
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn another_projects_files_are_out_of_reach() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (st, _shop, dir) = state_with(&[("sales.csv", b"a\n1\n")]);
        let other = {
            let db = st.db().unwrap();
            dn_store::project::create_project(&db, "Other").unwrap()
        };
        let tools = DataTools {
            state: st,
            project_id: other.id,
        };
        rt.block_on(async {
            let out = tools
                .execute(&call(serde_json::json!({"file": "sales.csv"})))
                .await;
            assert!(out.is_error);
            assert!(format!("{:?}", out.content).contains("no CSV or spreadsheet files"));
        });
        let _ = std::fs::remove_dir_all(dir);
    }
}
