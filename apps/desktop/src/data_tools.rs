//! The `data_profile` tool in project chats (#366): a fact sheet for one of the project's
//! CSV or spreadsheet files, computed by dn-data, so the model quotes numbers instead of
//! calculating them. Read-only; it reads only the local copies of the chat's own project
//! files, found by their name, never a path.

use crate::state::AppState;
use dn_agent::ToolOutput;
use dn_core::Id;
use dn_core::chat::{ToolCall, ToolSpec};
use std::sync::Arc;

pub use dn_data::tool::{CAPABILITY_ID, TOOL_NAME};

pub struct DataTools {
    pub state: Arc<AppState>,
    pub project_id: Id,
}

impl DataTools {
    pub fn specs(&self) -> Vec<ToolSpec> {
        if self.state.agent.manifest.allows_tool(CAPABILITY_ID) {
            vec![dn_data::tool::spec()]
        } else {
            Vec::new()
        }
    }

    /// Runs a call. Every failure is a message the model can act on (a missing file names
    /// the project's data files).
    pub async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let args = match dn_data::tool::Args::parse(&call.arguments) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(e),
        };
        let state = Arc::clone(&self.state);
        let project_id = self.project_id;
        // File reading and parsing are blocking, and a malformed workbook could make the
        // parser panic: off the async workers, and a panic is an error, not a crash.
        let r =
            tauri::async_runtime::spawn_blocking(move || profile(&state, project_id, &args)).await;
        match r {
            Ok(Ok(text)) => ToolOutput::text(text),
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => {
                tracing::warn!("data_profile failed: {e}");
                ToolOutput::error("The file couldn't be read as data.")
            }
        }
    }
}

fn profile(state: &AppState, project_id: Id, args: &dn_data::tool::Args) -> Result<String, String> {
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
        .find(|f| f.name.eq_ignore_ascii_case(args.file.trim()))
    else {
        return Err(if data_files.is_empty() {
            format!(
                "This project has no file called \"{}\", and no CSV or spreadsheet files.",
                args.file
            )
        } else {
            format!(
                "This project has no file called \"{}\". Its data files: {}.",
                args.file,
                data_files.join(", ")
            )
        });
    };
    if !dn_data::tool::reads(&file.name) {
        return Err(format!(
            "\"{}\" isn't a CSV or spreadsheet file, so it has no fact sheet.",
            file.name
        ));
    }
    let root = state
        .project_files_root()
        .ok_or("Project files aren't available here.")?;
    let bytes = std::fs::read(dn_store::project::file_path(root, file)).map_err(|e| {
        tracing::warn!("data_profile: a project file's copy can't be read: {e}");
        format!("The copy of \"{}\" can't be read.", file.name)
    })?;
    dn_data::tool::run(&file.name, &bytes, args.sheet.as_deref()).map(|p| p.text)
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
            1,
            "the default assistant allows data.profile"
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
