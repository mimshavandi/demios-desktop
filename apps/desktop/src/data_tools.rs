//! The data tools in project chats and chats with attached data files (#366, #370): `data_profile` (a fact sheet for one of the
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

/// The files a chat's data tools can read: its project's files and the files attached to
/// the chat itself (#370).
pub struct DataTools {
    pub state: Arc<AppState>,
    pub project_id: Option<Id>,
    pub conversation_id: Option<Id>,
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
        let (project_id, conversation_id) = (self.project_id, self.conversation_id);
        // File reading and parsing are blocking, and a malformed workbook could make the
        // parser panic: off the async workers, and a panic is an error, not a crash.
        let r = tauri::async_runtime::spawn_blocking(move || {
            run(&state, project_id, conversation_id, &request)
        })
        .await;
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

/// A file the data tools may read, and where it comes from.
struct Candidate {
    name: String,
    path: std::path::PathBuf,
    attached: bool,
}

/// The chat's attached files first, then its project's files.
fn candidates(
    state: &AppState,
    project_id: Option<Id>,
    conversation_id: Option<Id>,
) -> Result<Vec<Candidate>, String> {
    let db = state.db().map_err(|e| e.message)?;
    let mut out = Vec::new();
    if let (Some(c), Some(root)) = (conversation_id, state.chat_files_root()) {
        for f in crate::chat_files::list(&db, c).map_err(|e| e.to_string())? {
            out.push(Candidate {
                path: crate::chat_files::file_path(root, &f),
                name: f.name,
                attached: true,
            });
        }
    }
    if let (Some(p), Some(root)) = (project_id, state.project_files_root()) {
        for f in dn_store::project::list_files(&db, p).map_err(|e| e.to_string())? {
            out.push(Candidate {
                path: dn_store::project::file_path(root, &f),
                name: f.name,
                attached: false,
            });
        }
    }
    Ok(out)
}

/// A file name (or the model's guess at one) for a message outside the untrusted block:
/// one line, `"` swapped, delimiters neutralised.
fn shown(name: &str) -> String {
    let flat: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    dn_core::untrusted::neutralise(&flat.replace('"', "'"))
}

fn run(
    state: &AppState,
    project_id: Option<Id>,
    conversation_id: Option<Id>,
    request: &Request,
) -> Result<String, String> {
    let wanted = request.file();
    let shown_wanted = shown(wanted);
    let files = candidates(state, project_id, conversation_id)?;
    let data_files: Vec<String> = files
        .iter()
        .filter(|f| dn_data::tool::reads(&f.name))
        .map(|f| shown(&f.name))
        .collect();
    let matching: Vec<&Candidate> = files
        .iter()
        .filter(|f| f.name.eq_ignore_ascii_case(wanted.trim()))
        .collect();
    let Some(file) = matching.first().copied() else {
        return Err(if data_files.is_empty() {
            format!(
                "This chat has no file called \"{shown_wanted}\", and no CSV or spreadsheet files."
            )
        } else {
            format!(
                "This chat has no file called \"{shown_wanted}\". Its data files: {}.",
                data_files.join(", ")
            )
        });
    };
    if !dn_data::tool::reads(&file.name) {
        return Err(format!(
            "\"{}\" isn't a CSV or spreadsheet file, so it has no data to compute with.",
            shown(&file.name)
        ));
    }
    // Said, not chosen silently: an attached file comes before a project file of the same name.
    let note = (matching.len() > 1 && file.attached).then(|| {
        format!(
            "Note: this is the \"{}\" attached to this chat; the project also has a file with that name.\n",
            shown(&file.name)
        )
    });
    let bytes = std::fs::read(&file.path).map_err(|e| {
        tracing::warn!("data tool: a file's copy can't be read: {e}");
        format!("The copy of \"{}\" can't be read.", shown(&file.name))
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
    result.map(|p| {
        format!(
            "{}{}",
            note.unwrap_or_default(),
            dn_data::tool::wrap(capability, &file.name, &p.text)
        )
    })
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
            project_id: Some(project),
            conversation_id: None,
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
            project_id: Some(project),
            conversation_id: None,
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

    fn text_of(out: &ToolOutput) -> String {
        match &out.content[0] {
            dn_core::chat::ContentPart::Text { text } => text.clone(),
            _ => panic!(),
        }
    }

    #[test]
    fn a_file_attached_to_a_chat_outside_a_project_is_readable_and_wins_over_a_project_file() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // A project with sales.csv (revenue 1), and a chat with its own sales.csv (revenue 7).
        let (st, project, dir) = state_with(&[("sales.csv", b"region,revenue\nNorth,1\n")]);
        let conv = st.create_conversation().unwrap();
        let src = dir.join("attached");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("sales.csv"), b"region,revenue\nNorth,7\n").unwrap();
        std::fs::write(src.join("extra.csv"), b"a\n5\n").unwrap();
        let added = crate::chat_files::add(
            &st,
            conv.id,
            &[src.join("sales.csv"), src.join("extra.csv")],
        )
        .unwrap();
        assert_eq!(added.added.len(), 2);
        let call = |file: &str| ToolCall {
            id: "c".into(),
            name: TOOL_NAME.into(),
            arguments: serde_json::json!({"file": file}),
        };
        rt.block_on(async {
            // A chat that isn't in a project reads its own attachment.
            let alone = DataTools {
                state: Arc::clone(&st),
                project_id: None,
                conversation_id: Some(conv.id),
            };
            let out = alone.execute(&call("extra.csv")).await;
            assert!(!out.is_error, "{:?}", out.content);
            assert!(text_of(&out).contains("\"a\" sum: 5"));
            // In a project, the attachment comes first, and the result says so.
            let both = DataTools {
                state: Arc::clone(&st),
                project_id: Some(project),
                conversation_id: Some(conv.id),
            };
            let out = both.execute(&call("sales.csv")).await;
            let t = text_of(&out);
            assert!(t.contains("\"revenue\" sum: 7"), "{t}");
            assert!(
                t.starts_with("Note: this is the \"sales.csv\" attached to this chat"),
                "{t}"
            );
            // Another chat's attachment is out of reach.
            let other = st.create_conversation().unwrap();
            let out = DataTools {
                state: Arc::clone(&st),
                project_id: None,
                conversation_id: Some(other.id),
            }
            .execute(&call("extra.csv"))
            .await;
            assert!(out.is_error);
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
            project_id: Some(other.id),
            conversation_id: None,
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
