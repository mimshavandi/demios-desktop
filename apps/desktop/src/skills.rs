//! Chat skills (T-SKL-01): the bundled library in `skills/library/*/SKILL.md`, the skill each
//! chat uses, and what a skill changes in a turn.
//!
//! A skill adds its instructions to the system prompt and narrows the tools through
//! [`dn_tools::Narrowed`]. It never widens anything: the tools still come from the agent
//! manifest, and approvals, risk tiers and per-tool choices are decided by the executor
//! after the narrowing.

use crate::error::{IpcError, IpcResult};
use dn_agent::{ChatSkill, ToolExecutor};
use dn_core::Id;
use dn_store::{Connection, Result};
use dn_tools::{CapabilityResolver, Narrowed, ToolAllowlist};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;

/// The library's folder under `skills/`.
pub const LIBRARY_DIR: &str = "library";

/// A skill as the composer's picker shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub label: String,
    pub description: String,
    /// The tool patterns it allows (`[]` = no tools).
    pub allowed_tools: Vec<String>,
}

/// A loaded skill with its checked allowlist.
pub struct Skill {
    pub skill: ChatSkill,
    pub allow: ToolAllowlist,
}

impl Skill {
    pub fn info(&self) -> SkillInfo {
        SkillInfo {
            name: self.skill.name.clone(),
            label: self.skill.label(),
            description: self.skill.description.clone(),
            allowed_tools: self.skill.allowed_tools.clone(),
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

/// The skills the app offers, by name.
#[derive(Default)]
pub struct Library {
    skills: Vec<Arc<Skill>>,
}

impl Library {
    /// Loads `<skills>/library`. A broken file is logged and left out; the rest still load.
    pub fn load(skills: Option<&Path>) -> Self {
        let Some(dir) = skills.map(|d| d.join(LIBRARY_DIR)).filter(|d| d.is_dir()) else {
            return Self::default();
        };
        let (loaded, errors) = dn_agent::chat_skill::load_dir(&dir);
        for e in errors {
            eprintln!("could not load a skill: {e}");
        }
        Self::from_skills(loaded)
    }

    pub fn from_skills(skills: Vec<ChatSkill>) -> Self {
        let skills = skills
            .into_iter()
            .filter_map(|s| match ToolAllowlist::new(&s.allowed_tools) {
                Ok(allow) => Some(Arc::new(Skill { skill: s, allow })),
                Err(e) => {
                    eprintln!("could not load the skill `{}`: {e}", s.name);
                    None
                }
            })
            .collect();
        Self { skills }
    }

    pub fn list(&self) -> Vec<SkillInfo> {
        self.skills.iter().map(|s| s.info()).collect()
    }

    pub fn get(&self, name: &str) -> Option<Arc<Skill>> {
        self.skills.iter().find(|s| s.skill.name == name).cloned()
    }
}

/// App-owned table: the skill each chat uses (no row = no skill).
pub fn migrations() -> &'static [&'static str] {
    &["CREATE TABLE app_conversation_skills (
        conversation_id TEXT    PRIMARY KEY NOT NULL,
        skill           TEXT    NOT NULL,
        updated_at      INTEGER NOT NULL
    );"]
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
        None => forget_chat(conn, conversation_id)?,
    };
    Ok(())
}

pub fn forget_chat(conn: &Connection, conversation_id: Id) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM app_conversation_skills WHERE conversation_id = ?1",
        [conversation_id.to_string()],
    )?)
}

pub fn delete_all(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM app_conversation_skills", [])?;
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
