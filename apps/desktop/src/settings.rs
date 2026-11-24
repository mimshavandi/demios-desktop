//! App settings (T-SHELL-02), stored as JSON values in the app's own `app_settings`
//! table. Telemetry is off by default and nothing in the app sends any yet.

use dn_core::model::{Capability, ModelRef};
use dn_store::{Connection, Result, StoreError, now_ms};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The app's migration set (name `app`), composed by `dn_store::open`.
pub fn migrations() -> &'static [&'static str] {
    &["CREATE TABLE app_settings (
        key        TEXT    PRIMARY KEY NOT NULL,
        value      TEXT    NOT NULL,
        updated_at INTEGER NOT NULL
    );"]
}

/// A model slot the shell exposes in settings.
#[derive(Clone, Debug, Serialize)]
pub struct SlotInfo {
    pub slot_id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub capability: Capability,
}

pub const SLOTS: &[SlotInfo] = &[
    SlotInfo {
        slot_id: "chat",
        label: "Chat",
        description: "Answers in conversations.",
        capability: Capability::Chat,
    },
    SlotInfo {
        slot_id: "fast",
        label: "Fast",
        description: "Titles, summaries and other quick background tasks.",
        capability: Capability::Chat,
    },
    SlotInfo {
        slot_id: "embed",
        label: "Embeddings",
        description: "Knowledge search (arrives with dn-knowledge).",
        capability: Capability::Embedding,
    },
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// slot id → `ModelRef` string. Missing = not configured (never a silent fallback).
    pub slots: BTreeMap<String, String>,
    /// Off by default. No telemetry is collected in this build either way.
    pub telemetry_enabled: bool,
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
        Ok(())
    }

    pub fn slot(&self, slot_id: &str) -> Option<ModelRef> {
        self.slots.get(slot_id).and_then(|s| s.parse().ok())
    }
}

const KEY: &str = "settings";

pub fn load(conn: &Connection) -> Result<Settings> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM app_settings WHERE key = ?1", [KEY], |r| {
            r.get(0)
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        dn_store::open_in_memory(&[dn_store::MigrationSet::new("app", migrations())]).unwrap()
    }

    #[test]
    fn defaults_have_telemetry_off_and_no_models() {
        let c = db();
        let s = load(&c).unwrap();
        assert!(!s.telemetry_enabled);
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
