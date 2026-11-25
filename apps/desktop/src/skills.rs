//! Chat skills (T-SKL-01, T-SKL-02): the bundled library in `skills/library/*/SKILL.md`,
//! the user's own skills in `<data dir>/skills/*/SKILL.md`, the skill each chat uses, and
//! what a skill changes in a turn.
//!
//! A skill adds its instructions to the system prompt and narrows the tools through
//! [`dn_tools::Narrowed`]. It never widens anything: the tools still come from the agent
//! manifest, and approvals, risk tiers and per-tool choices are decided by the executor
//! after the narrowing. The user's own skills follow exactly the same rules.

use crate::error::{IpcError, IpcResult};
use dn_agent::{ChatSkill, ManifestError, ToolExecutor};
use dn_core::Id;
use dn_store::{Connection, Result};
use dn_tools::{CapabilityResolver, Narrowed, ToolAllowlist};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// The library's folder under `skills/`.
pub const LIBRARY_DIR: &str = "library";
/// The user's skills folder under the app's data directory.
pub const USER_DIR: &str = "skills";
/// At most this many of the user's skills load; the rest are reported.
pub const MAX_USER_SKILLS: usize = 100;

/// Where a skill comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    /// Shipped with the app.
    Builtin,
    /// A file in the user's skills folder.
    User,
}

/// A skill as the composer's picker and Settings show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub label: String,
    pub description: String,
    /// The tool patterns it allows (`[]` = no tools).
    pub allowed_tools: Vec<String>,
    pub source: SkillSource,
}

/// A skill file that didn't load, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkillProblem {
    /// The file, relative to its skills folder (`my-skill/SKILL.md`).
    pub file: String,
    pub message: String,
    /// Shipped with the app (a broken build) or the user's own file.
    pub source: SkillSource,
}

/// What Settings shows: every skill, the files that didn't load, and the folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkillsOverview {
    pub skills: Vec<SkillInfo>,
    pub problems: Vec<SkillProblem>,
    /// The user's skills folder (it may not exist yet).
    pub folder: Option<String>,
}

/// A loaded skill with its checked allowlist.
pub struct Skill {
    pub skill: ChatSkill,
    pub allow: ToolAllowlist,
    pub source: SkillSource,
}

impl Skill {
    pub fn info(&self) -> SkillInfo {
        SkillInfo {
            name: self.skill.name.clone(),
            label: self.skill.label(),
            description: self.skill.description.clone(),
            allowed_tools: self.skill.allowed_tools.clone(),
            source: self.source,
        }
    }

    /// The system-prompt part for this skill.
    pub fn system_text(&self) -> String {
        format!(
            "The user chose the skill \"{}\" for this chat. Follow these instructions for \
             the rest of the chat:\n\n{}",
            self.skill.label(),
            self.skill.instructions
        )
    }

    pub fn allows(&self, capability_id: &str) -> bool {
        self.allow.allows(capability_id)
    }
}

/// One load of both folders.
#[derive(Default)]
struct Loaded {
    skills: Vec<Arc<Skill>>,
    problems: Vec<SkillProblem>,
}

/// The skills the app offers, by name. [`Library::reload`] reads the folders again; a chat
/// turn takes the skill as it was when the turn started.
#[derive(Default)]
pub struct Library {
    bundled: Option<PathBuf>,
    user: Option<PathBuf>,
    loaded: RwLock<Arc<Loaded>>,
}

impl Library {
    /// Loads `<skills>/library` and the user's folder. A file that doesn't load is left
    /// out; the rest still load. Problems in the user's folder are kept for Settings.
    pub fn load(skills: Option<&Path>, user: Option<PathBuf>) -> Self {
        let lib = Self {
            bundled: skills.map(|d| d.join(LIBRARY_DIR)),
            user,
            loaded: Default::default(),
        };
        lib.reload();
        lib
    }

    /// Built-in skills only, without folders (tests). A bad allowlist panics.
    pub fn from_skills(skills: Vec<ChatSkill>) -> Self {
        let mut loaded = Loaded::default();
        for s in skills {
            let allow = ToolAllowlist::new(&s.allowed_tools).expect("valid allowed-tools");
            loaded.skills.push(Arc::new(Skill {
                skill: s,
                allow,
                source: SkillSource::Builtin,
            }));
        }
        Self {
            loaded: RwLock::new(Arc::new(loaded)),
            ..Default::default()
        }
    }

    /// Reads both folders again (Settings › Reload, and at start).
    pub fn reload(&self) {
        let mut out = Loaded::default();
        if let Some(dir) = self.bundled.as_deref().filter(|d| d.is_dir()) {
            // A broken built-in file is a broken build: Settings shows it like a user file's
            // problem instead of the skill just missing.
            let (skills, errors) = dn_agent::chat_skill::load_dir(dir);
            for e in &errors {
                eprintln!("could not load a built-in skill: {e}");
            }
            out.problems
                .extend(errors.iter().map(|e| problem(dir, e, SkillSource::Builtin)));
            for s in skills {
                match ToolAllowlist::new(&s.allowed_tools) {
                    Ok(allow) => out.skills.push(Arc::new(Skill {
                        skill: s,
                        allow,
                        source: SkillSource::Builtin,
                    })),
                    Err(message) => out.problems.push(SkillProblem {
                        file: format!("{}/{}", s.name, dn_agent::chat_skill::SKILL_FILE),
                        message,
                        source: SkillSource::Builtin,
                    }),
                }
            }
        }
        if let Some(dir) = self.user.as_deref().filter(|d| d.is_dir()) {
            let (skills, errors) = dn_agent::chat_skill::load_dir(dir);
            out.problems
                .extend(errors.iter().map(|e| problem(dir, e, SkillSource::User)));
            let mut taken = 0;
            for s in skills {
                let file = format!("{}/{}", s.name, dn_agent::chat_skill::SKILL_FILE);
                if out.skills.iter().any(|b| b.skill.name == s.name) {
                    out.problems.push(SkillProblem {
                        file,
                        source: SkillSource::User,
                        message: format!(
                            "A built-in skill is already called \"{}\". Rename the folder and \
                             the name in the file.",
                            s.name
                        ),
                    });
                    continue;
                }
                if taken == MAX_USER_SKILLS {
                    out.problems.push(SkillProblem {
                        file,
                        source: SkillSource::User,
                        message: format!("Only {MAX_USER_SKILLS} of your skills can load."),
                    });
                    continue;
                }
                // The parser already checked the patterns; this can't fail, but stay closed.
                match ToolAllowlist::new(&s.allowed_tools) {
                    Ok(allow) => {
                        taken += 1;
                        out.skills.push(Arc::new(Skill {
                            skill: s,
                            allow,
                            source: SkillSource::User,
                        }));
                    }
                    Err(message) => out.problems.push(SkillProblem {
                        file,
                        message,
                        source: SkillSource::User,
                    }),
                }
            }
        }
        out.problems.sort_by(|a, b| a.file.cmp(&b.file));
        // A panic elsewhere while holding the lock doesn't make the data wrong: keep using it.
        *self.loaded.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(out);
    }

    fn current(&self) -> Arc<Loaded> {
        Arc::clone(&self.loaded.read().unwrap_or_else(|p| p.into_inner()))
    }

    pub fn list(&self) -> Vec<SkillInfo> {
        self.current().skills.iter().map(|s| s.info()).collect()
    }

    pub fn get(&self, name: &str) -> Option<Arc<Skill>> {
        self.current()
            .skills
            .iter()
            .find(|s| s.skill.name == name)
            .cloned()
    }

    pub fn user_dir(&self) -> Option<&Path> {
        self.user.as_deref()
    }

    pub fn overview(&self) -> SkillsOverview {
        let l = self.current();
        SkillsOverview {
            skills: l.skills.iter().map(|s| s.info()).collect(),
            problems: l.problems.clone(),
            folder: self.user.as_ref().map(|p| p.display().to_string()),
        }
    }
}

/// A load error as Settings shows it: the file relative to the folder, and each problem
/// with the field it is about.
fn problem(dir: &Path, e: &ManifestError, source: SkillSource) -> SkillProblem {
    let file = e
        .file
        .strip_prefix(dir)
        .unwrap_or(&e.file)
        .to_string_lossy()
        .replace('\\', "/");
    let message = e
        .problems
        .iter()
        .map(|p| match p.path.trim_start_matches('/') {
            "" => p.message.clone(),
            field => format!("{field}: {}", p.message),
        })
        .collect::<Vec<_>>()
        .join("; ");
    SkillProblem {
        file,
        message,
        source,
    }
}

/// App-owned tables: the skill each chat uses (no row = no skill), and the skill each
/// answer was written with (T-SKL-03; kept with its label so it still reads after the skill
/// is renamed or removed).
pub fn migrations() -> &'static [&'static str] {
    &[
        "CREATE TABLE app_conversation_skills (
        conversation_id TEXT    PRIMARY KEY NOT NULL,
        skill           TEXT    NOT NULL,
        updated_at      INTEGER NOT NULL
    );",
        "CREATE TABLE app_answer_skills (
        message_id      TEXT    PRIMARY KEY NOT NULL,
        conversation_id TEXT    NOT NULL,
        skill           TEXT    NOT NULL,
        label           TEXT    NOT NULL,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX app_answer_skills_conversation ON app_answer_skills (conversation_id);",
    ]
}

/// The skill an answer was written with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AnswerSkill {
    pub message_id: Id,
    pub name: String,
    pub label: String,
}

/// Remembers that `message_id` (an assistant answer) is written with `skill`.
pub fn record_answer_skill(
    conn: &Connection,
    conversation_id: Id,
    message_id: Id,
    skill: &Skill,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO app_answer_skills
         (message_id, conversation_id, skill, label, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            message_id.to_string(),
            conversation_id.to_string(),
            skill.skill.name,
            skill.skill.label(),
            dn_store::now_ms()
        ],
    )?;
    Ok(())
}

/// The answers of a chat that were written with a skill, oldest first.
pub fn answer_skills(conn: &Connection, conversation_id: Id) -> Result<Vec<AnswerSkill>> {
    let mut stmt = conn.prepare(
        "SELECT message_id, skill, label FROM app_answer_skills
         WHERE conversation_id = ?1 ORDER BY created_at, message_id",
    )?;
    let rows = stmt
        .query_map([conversation_id.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, name, label)| {
            Some(AnswerSkill {
                // `Id` is a transparent UUID: parse it the way serde reads it.
                message_id: serde_json::from_value(serde_json::Value::String(id)).ok()?,
                name,
                label,
            })
        })
        .collect())
}

/// The skill name saved for a chat, if any.
pub fn chat_skill(conn: &Connection, conversation_id: Id) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT skill FROM app_conversation_skills WHERE conversation_id = ?1",
            [conversation_id.to_string()],
            |r| r.get(0),
        )
        .optional()?)
}

/// Saves (or, with `None`, clears) the skill for a chat.
pub fn set_chat_skill(conn: &Connection, conversation_id: Id, skill: Option<&str>) -> Result<()> {
    match skill {
        Some(s) => conn.execute(
            "INSERT INTO app_conversation_skills (conversation_id, skill, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(conversation_id) DO UPDATE SET skill = ?2, updated_at = ?3",
            params![conversation_id.to_string(), s, dn_store::now_ms()],
        )?,
        // Only the choice: answers keep the skill they were written with.
        None => conn.execute(
            "DELETE FROM app_conversation_skills WHERE conversation_id = ?1",
            [conversation_id.to_string()],
        )?,
    };
    Ok(())
}

/// Forgets a chat's skill and its answers' skills (the chat was deleted).
pub fn forget_chat(conn: &Connection, conversation_id: Id) -> Result<usize> {
    let id = conversation_id.to_string();
    let n = conn.execute(
        "DELETE FROM app_conversation_skills WHERE conversation_id = ?1",
        [&id],
    )?;
    conn.execute(
        "DELETE FROM app_answer_skills WHERE conversation_id = ?1",
        [&id],
    )?;
    Ok(n)
}

pub fn delete_all(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM app_conversation_skills", [])?;
    conn.execute("DELETE FROM app_answer_skills", [])?;
    Ok(())
}

/// Checks a choice from the UI: `None` clears, a name must be in the library.
pub fn check_choice(library: &Library, skill: Option<&str>) -> IpcResult<()> {
    match skill {
        Some(name) if library.get(name).is_none() => Err(IpcError::invalid(format!(
            "There is no skill called \"{name}\"."
        ))),
        _ => Ok(()),
    }
}

/// `tools`, narrowed to what `skill` allows. `resolve` maps a model-facing tool name to its
/// capability id.
pub fn narrow(
    tools: Arc<dyn ToolExecutor>,
    skill: &Skill,
    resolve: CapabilityResolver,
) -> Arc<dyn ToolExecutor> {
    Arc::new(Narrowed::new(tools, skill.allow.clone(), resolve))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn skill(name: &str, tools: &[&str]) -> ChatSkill {
        ChatSkill {
            name: name.into(),
            description: format!("{name} description"),
            allowed_tools: tools.iter().map(|t| t.to_string()).collect(),
            license: None,
            instructions: format!("Do {name}."),
        }
    }

    #[test]
    fn the_bundled_library_loads_without_errors() {
        let dir = crate::agent::skills_dir(None)
            .expect("repo skills/")
            .join(LIBRARY_DIR);
        let (skills, errors) = dn_agent::chat_skill::load_dir(&dir);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!skills.is_empty());
        for s in &skills {
            ToolAllowlist::new(&s.allowed_tools).unwrap();
            assert!(s.license.is_some(), "{} states its licence", s.name);
        }
    }

    /// A temporary user skills folder, removed on drop.
    struct Folder(PathBuf);
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    impl Folder {
        fn new() -> Self {
            let d = std::env::temp_dir().join(format!("dn-skills-{}", Id::new()));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn write(&self, folder: &str, text: &str) {
            let d = self.0.join(folder);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("SKILL.md"), text).unwrap();
        }
    }

    fn file(name: &str, tools: &str, extra: &str) -> String {
        format!(
            "---\nname: {name}\ndescription: {name} things.\nallowed-tools: {tools}\n{extra}---\nDo {name}.\n"
        )
    }

    #[test]
    fn user_skills_load_next_to_the_built_in_ones_with_the_same_rules() {
        let user = Folder::new();
        user.write("haiku", &file("haiku", "[]", ""));
        user.write("notes", &file("notes", "[mcp.files.read_file]", ""));
        let bundled = crate::agent::skills_dir(None).expect("repo skills/");
        let lib = Library::load(Some(&bundled), Some(user.0.clone()));
        let o = lib.overview();
        assert!(o.problems.is_empty(), "{:?}", o.problems);
        let haiku = o.skills.iter().find(|s| s.name == "haiku").unwrap();
        assert_eq!(haiku.source, SkillSource::User);
        assert!(
            o.skills
                .iter()
                .any(|s| s.name == "summarize" && s.source == SkillSource::Builtin)
        );
        // Same narrowing as a built-in skill: only the listed tool.
        let notes = lib.get("notes").unwrap();
        assert!(notes.allows("mcp.files.read_file"));
        assert!(!notes.allows("mcp.files.write_file"));
        assert!(!notes.allows("artifact.create"));
        assert_eq!(o.folder.as_deref(), Some(&*user.0.display().to_string()));
    }

    #[test]
    fn broken_user_files_are_reported_with_their_error_and_left_out() {
        let user = Folder::new();
        user.write("ok", &file("ok", "[]", ""));
        user.write("widen", &file("widen", "[]", "auto-approve: true\n"));
        user.write("bad-tools", &file("bad-tools", "['mcp.*.write']", ""));
        user.write("Wrong", &file("wrong", "[]", ""));
        user.write("summarize", &file("summarize", "[]", ""));
        user.write("empty", "no front matter here");
        let bundled = crate::agent::skills_dir(None).expect("repo skills/");
        let lib = Library::load(Some(&bundled), Some(user.0.clone()));
        let o = lib.overview();
        let users: Vec<&str> = o
            .skills
            .iter()
            .filter(|s| s.source == SkillSource::User)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(users, ["ok"]);
        let files: Vec<&str> = o.problems.iter().map(|p| p.file.as_str()).collect();
        assert_eq!(
            files,
            [
                "Wrong/SKILL.md",
                "bad-tools/SKILL.md",
                "empty/SKILL.md",
                "summarize/SKILL.md",
                "widen/SKILL.md"
            ]
        );
        let msg = |f: &str| &o.problems.iter().find(|p| p.file == f).unwrap().message;
        assert!(
            msg("widen/SKILL.md").contains("auto-approve"),
            "{}",
            msg("widen/SKILL.md")
        );
        assert!(msg("bad-tools/SKILL.md").starts_with("allowed-tools/0:"));
        assert!(msg("Wrong/SKILL.md").contains("folder name"));
        assert!(msg("summarize/SKILL.md").contains("built-in skill"));
        assert!(msg("empty/SKILL.md").contains("front matter"));
        // The built-in skill of that name is the one in use.
        assert_eq!(lib.get("summarize").unwrap().source, SkillSource::Builtin);
    }

    #[test]
    fn a_broken_built_in_skill_is_reported_not_just_missing() {
        let root = Folder::new();
        root.write("library/fine", &file("fine", "[]", ""));
        root.write("library/broken", &file("broken", "[]", "risk: read\n"));
        let lib = Library::load(Some(&root.0), None);
        assert!(lib.get("fine").is_some());
        assert!(lib.get("broken").is_none());
        let p = &lib.overview().problems;
        assert_eq!(p.len(), 1);
        assert_eq!(
            (p[0].file.as_str(), p[0].source),
            ("broken/SKILL.md", SkillSource::Builtin)
        );
    }

    #[test]
    fn reload_picks_up_new_and_fixed_files() {
        let user = Folder::new();
        user.write("draft", "---\nname: draft\n");
        let lib = Library::load(None, Some(user.0.clone()));
        assert!(lib.get("draft").is_none());
        assert_eq!(lib.overview().problems.len(), 1);
        user.write("draft", &file("draft", "[]", ""));
        lib.reload();
        assert!(lib.get("draft").is_some());
        assert!(lib.overview().problems.is_empty());
    }

    #[test]
    fn huge_files_are_refused_before_reading() {
        let user = Folder::new();
        let big = format!(
            "{}{}",
            file("big", "[]", ""),
            "x".repeat(dn_agent::chat_skill::MAX_FILE_BYTES as usize)
        );
        user.write("big", &big);
        let lib = Library::load(None, Some(user.0.clone()));
        assert!(lib.get("big").is_none());
        assert!(lib.overview().problems[0].message.contains("larger than"));
    }

    #[test]
    fn a_missing_user_folder_is_fine() {
        let lib = Library::load(None, Some(std::env::temp_dir().join("dn-skills-none")));
        assert!(lib.list().is_empty());
        assert!(lib.overview().problems.is_empty());
    }

    #[test]
    fn choices_are_saved_per_chat_and_cleared() {
        let conn = dn_store::open_in_memory(&crate::state::migration_sets()).unwrap();
        let (a, b) = (Id::new(), Id::new());
        assert_eq!(chat_skill(&conn, a).unwrap(), None);
        set_chat_skill(&conn, a, Some("proofread")).unwrap();
        set_chat_skill(&conn, a, Some("summarize")).unwrap();
        set_chat_skill(&conn, b, Some("proofread")).unwrap();
        assert_eq!(chat_skill(&conn, a).unwrap().as_deref(), Some("summarize"));
        set_chat_skill(&conn, a, None).unwrap();
        assert_eq!(chat_skill(&conn, a).unwrap(), None);
        delete_all(&conn).unwrap();
        assert_eq!(chat_skill(&conn, b).unwrap(), None);
    }

    #[test]
    fn unknown_choices_are_refused() {
        let lib = Library::from_skills(vec![skill("proofread", &[])]);
        assert!(check_choice(&lib, Some("proofread")).is_ok());
        assert!(check_choice(&lib, None).is_ok());
        assert_eq!(
            check_choice(&lib, Some("nope")).unwrap_err().code,
            "invalid_input"
        );
    }
}
