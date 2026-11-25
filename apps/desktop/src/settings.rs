//! App settings (T-SHELL-02), stored as JSON values in the app's own `app_settings`
//! table. The app collects no usage data; `telemetry_enabled` is kept only so older saved
//! settings still load (T-STORE-11 removed the switch).

use dn_core::model::{Capability, ModelRef};
use dn_store::{Connection, Result, StoreError, now_ms};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The app's migration set (name `app`), composed by `dn_store::open`.
pub fn migrations() -> &'static [&'static str] {
    &[
        "CREATE TABLE app_settings (
        key        TEXT    PRIMARY KEY NOT NULL,
        value      TEXT    NOT NULL,
        updated_at INTEGER NOT NULL
    );",
        // T-STORE-11: every keychain id dn-tools stored for an MCP server, so "Delete all
        // local data" also finds secrets of servers no longer in settings (the keychain
        // can't be listed).
        "CREATE TABLE app_mcp_secret_ids (
        id         TEXT    PRIMARY KEY NOT NULL,
        created_at INTEGER NOT NULL
    );",
        // T-SHELL-08: local MCP servers the user allowed to start, per exact command line
        // (program, arguments and env var names). A change asks again.
        "CREATE TABLE app_mcp_launch_approvals (
        server_id    TEXT    NOT NULL,
        command_line TEXT    NOT NULL,
        created_at   INTEGER NOT NULL,
        PRIMARY KEY (server_id, command_line)
    );",
    ]
}

/// Remembers that an MCP secret id was written to the keychain.
pub fn record_mcp_secret_id(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO app_mcp_secret_ids (id, created_at) VALUES (?1, ?2)",
        params![id, now_ms()],
    )?;
    Ok(())
}

/// Every MCP secret id ever written (and not yet forgotten).
pub fn known_mcp_secret_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id FROM app_mcp_secret_ids ORDER BY id")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

pub fn record_mcp_launch(conn: &Connection, server_id: &str, command_line: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO app_mcp_launch_approvals (server_id, command_line, created_at)
         VALUES (?1, ?2, ?3)",
        params![server_id, command_line, now_ms()],
    )?;
    Ok(())
}

pub fn mcp_launch_approved(conn: &Connection, server_id: &str, command_line: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM app_mcp_launch_approvals WHERE server_id = ?1 AND command_line = ?2",
            params![server_id, command_line],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub fn forget_mcp_launches(conn: &Connection, server_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM app_mcp_launch_approvals WHERE server_id = ?1",
        [server_id],
    )?;
    Ok(())
}

pub fn forget_mcp_secret_id(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM app_mcp_secret_ids WHERE id = ?1", [id])?;
    Ok(())
}

/// A model slot the shell exposes in settings.
#[derive(Clone, Debug, Serialize)]
pub struct SlotInfo {
    pub slot_id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub capability: Capability,
    /// Whether Settings shows the slot. A slot nothing reads yet stays valid in saved settings
    /// but is hidden, so the screen doesn't promise work the app doesn't do.
    #[serde(skip)]
    pub shown: bool,
}

pub const SLOTS: &[SlotInfo] = &[
    SlotInfo {
        slot_id: "chat",
        label: "Chat",
        description: "Answers in conversations.",
        capability: Capability::Chat,
        shown: true,
    },
    SlotInfo {
        slot_id: "fast",
        label: "Fast",
        description: "Titles, summaries and other quick background tasks.",
        capability: Capability::Chat,
        // Nothing uses it yet (chat titles are the first message). Show it again when
        // something reads `slot("fast")`.
        shown: false,
    },
    SlotInfo {
        slot_id: "embed",
        label: "Embeddings",
        description: "Knowledge search (arrives with dn-knowledge).",
        capability: Capability::Embedding,
        shown: true,
    },
];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// slot id → `ModelRef` string. Missing = not configured (never a silent fallback).
    pub slots: BTreeMap<String, String>,
    /// Unused: no telemetry exists, and the switch was removed (T-STORE-11). Kept so older
    /// saved settings round-trip.
    pub telemetry_enabled: bool,
    /// The user dismissed the first-run AI notice (T-STORE-09). "Delete all local data"
    /// clears it, so the notice shows again.
    pub ai_notice_seen: bool,
    /// MCP servers the tool runtime connects to at startup (secret *ids* only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<dn_tools::McpServerConfig>,
    /// The user's choice per tool ("always ask", "off", ...), keyed by capability id
    /// (`mcp.<server>.<tool>`); applied after every connect (T-SHELL-11).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_overrides: BTreeMap<String, dn_tools::CapabilityOverride>,
    /// server id → tool name → fingerprint of the tool as last reviewed (the first connect
    /// counts). Tools that are new or differ always ask until reviewed (T-SHELL-11).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub reviewed_tools: BTreeMap<String, BTreeMap<String, String>>,
    /// OpenAI-compatible servers (T-PRV-05); their keys are in the keychain
    /// ([`RemoteServer::key_id`]). Managed by the server commands.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remote_servers: Vec<RemoteServer>,
}

/// An OpenAI-compatible server (Ollama, LM Studio, llama.cpp, vLLM, a hosted gateway). Its
/// models are `remote:<id>:<model>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteServer {
    /// Lowercase letters, digits and `-`; unique; used in model refs.
    pub id: String,
    pub name: String,
    /// Checked by [`dn_providers::check_server_url`] (https, or http for local hosts).
    pub base_url: String,
    /// Which server it is (for its thinking switch), detected when added or refreshed.
    #[serde(default)]
    pub family: dn_providers::ServerFamily,
    /// The models it listed when added or last refreshed.
    #[serde(default)]
    pub models: Vec<RemoteModel>,
}

impl RemoteServer {
    /// The keychain id of its optional key (`provider/remote.<id>`).
    pub fn key_id(&self) -> String {
        remote_key_id(&self.id)
    }

    pub fn model(&self, model_id: &str) -> Option<&RemoteModel> {
        self.models.iter().find(|m| m.id == model_id)
    }
}

pub fn remote_key_id(server_id: &str) -> String {
    format!("remote.{server_id}")
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoteModel {
    pub id: String,
    /// Tool calls as the server reports them; `None` when it doesn't say.
    #[serde(default)]
    pub tool_calls: Option<bool>,
    /// The user's answer for a model whose server doesn't say (`None`: not set).
    #[serde(default)]
    pub tool_calls_user: Option<bool>,
    #[serde(default)]
    pub embedding: bool,
}

/// Whether `id` can name a remote server.
pub fn valid_server_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
}

impl Settings {
    pub fn validate(&self) -> std::result::Result<(), String> {
        for (slot, model) in &self.slots {
            if !SLOTS.iter().any(|s| s.slot_id == slot) {
                return Err(format!("unknown slot `{slot}`"));
            }
            model
                .parse::<ModelRef>()
                .map_err(|e| format!("slot `{slot}`: {e}"))?;
        }
        for s in &self.mcp_servers {
            s.validate()
                .map_err(|e| format!("MCP server `{}`: {e}", s.id))?;
        }
        let mut ids = std::collections::HashSet::new();
        for r in &self.remote_servers {
            if !valid_server_id(&r.id) || !ids.insert(r.id.as_str()) {
                return Err(format!("server id `{}` is invalid or used twice", r.id));
            }
            dn_providers::check_server_url(&r.base_url)
                .map_err(|e| format!("server `{}`: {e}", r.name))?;
        }
        Ok(())
    }

    /// Takes the parts other commands manage (MCP servers, tool choices and reviews) from
    /// `stored`, so saving the rest of the settings can't change or drop them.
    pub fn keep_managed(&mut self, stored: Settings) {
        self.mcp_servers = stored.mcp_servers;
        self.tool_overrides = stored.tool_overrides;
        self.reviewed_tools = stored.reviewed_tools;
        self.remote_servers = stored.remote_servers;
    }

    pub fn slot(&self, slot_id: &str) -> Option<ModelRef> {
        self.slots.get(slot_id).and_then(|s| s.parse().ok())
    }
}

const KEY: &str = "settings";

pub fn load(conn: &Connection) -> Result<Settings> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            [KEY],
            |r| r.get(0),
        )
        .optional()?;
    match raw {
        None => Ok(Settings::default()),
        Some(s) => Ok(serde_json::from_str(&s)?),
    }
}

pub fn save(conn: &Connection, s: &Settings) -> Result<()> {
    s.validate().map_err(StoreError::Invalid)?;
    conn.execute(
        "INSERT INTO app_settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![KEY, serde_json::to_string(s)?, now_ms()],
    )?;
    Ok(())
}

/// Deletes the saved settings ("Delete all local data").
pub fn delete_all(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM app_settings", [])?;
    conn.execute("DELETE FROM app_mcp_secret_ids", [])?;
    conn.execute("DELETE FROM app_mcp_launch_approvals", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        dn_store::open_in_memory(&[dn_store::MigrationSet::new("app", migrations())]).unwrap()
    }

    #[test]
    fn a_hidden_slot_stays_valid_in_saved_settings() {
        let c = db();
        let mut s = Settings::default();
        s.slots.insert("fast".into(), "local:dev-fake".into());
        save(&c, &s).unwrap();
        assert_eq!(
            load(&c).unwrap().slot("fast").unwrap().to_string(),
            "local:dev-fake"
        );
        assert!(!SLOTS.iter().find(|s| s.slot_id == "fast").unwrap().shown);
    }

    #[test]
    fn a_settings_save_keeps_servers_and_tool_choices() {
        let mut stored = Settings {
            mcp_servers: vec![dn_tools::McpServerConfig::http(
                "work",
                "https://mcp.example.test/mcp",
            )],
            ..Default::default()
        };
        stored.tool_overrides.insert(
            "mcp.work.write".into(),
            dn_tools::CapabilityOverride {
                disabled: true,
                ..Default::default()
            },
        );
        stored
            .reviewed_tools
            .insert("work".into(), [("write".into(), "fp".into())].into());
        // What the UI sends: its own fields only, or an edited copy of the stored ones.
        let mut from_ui = Settings {
            ai_notice_seen: true,
            ..Default::default()
        };
        from_ui
            .reviewed_tools
            .insert("work".into(), Default::default());
        from_ui.keep_managed(stored.clone());
        assert!(from_ui.ai_notice_seen);
        assert_eq!(from_ui.mcp_servers, stored.mcp_servers);
        assert_eq!(from_ui.tool_overrides, stored.tool_overrides);
        assert_eq!(from_ui.reviewed_tools, stored.reviewed_tools);
    }

    #[test]
    fn defaults_have_telemetry_off_and_no_models() {
        let c = db();
        let s = load(&c).unwrap();
        assert!(!s.telemetry_enabled);
        assert!(!s.ai_notice_seen);
        assert!(s.slots.is_empty());
    }

    #[test]
    fn save_and_load() {
        let c = db();
        let mut s = Settings::default();
        s.slots.insert("chat".into(), "local:dev-fake".into());
        save(&c, &s).unwrap();
        s.telemetry_enabled = true;
        save(&c, &s).unwrap();
        assert_eq!(load(&c).unwrap(), s);
        assert_eq!(
            load(&c).unwrap().slot("chat"),
            Some("local:dev-fake".parse().unwrap())
        );
    }

    #[test]
    fn rejects_bad_slots() {
        let c = db();
        let mut s = Settings::default();
        s.slots.insert("chat".into(), "gpt-4o".into());
        assert!(save(&c, &s).is_err());
        let mut s = Settings::default();
        s.slots.insert("nope".into(), "local:x".into());
        assert!(save(&c, &s).is_err());
    }
}
