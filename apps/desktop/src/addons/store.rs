//! The add-ons' tables in the app database (ADR-0013 §3, §5): who owns what, what the
//! person allowed, and each add-on's settings namespace.
//!
//! **Ownership** is checked against `app_addon_owned`, never by comparing name prefixes:
//! a tool id, job, skill or secret name belongs to the one add-on that claimed it, and a
//! second claim is refused.

use dn_core::Id;
use dn_plugin_api::{ChatScope, Consent, Permission, PluginId, PluginManifest};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeSet;

pub fn migrations() -> &'static [&'static str] {
    &["CREATE TABLE app_addon_owned (
            kind      TEXT NOT NULL,
            key       TEXT NOT NULL,
            plugin_id TEXT NOT NULL,
            PRIMARY KEY (kind, key)
        );
        CREATE INDEX app_addon_owned_plugin ON app_addon_owned (plugin_id);
        CREATE TABLE app_addon_state (
            plugin_id        TEXT PRIMARY KEY,
            enabled          INTEGER NOT NULL DEFAULT 1,
            -- The permissions the person was asked about, as a JSON list: a new version
            -- that asks for one not in this list needs consent again.
            asked            TEXT NOT NULL DEFAULT '[]',
            asked_version    TEXT,
            chats_all        INTEGER NOT NULL DEFAULT 0,
            chats_projects   TEXT NOT NULL DEFAULT '[]',
            knowledge        TEXT NOT NULL DEFAULT '[]'
        );
        CREATE TABLE app_addon_permissions (
            plugin_id  TEXT NOT NULL,
            permission TEXT NOT NULL,
            PRIMARY KEY (plugin_id, permission)
        );
        CREATE TABLE app_addon_settings (
            plugin_id TEXT NOT NULL,
            key       TEXT NOT NULL,
            value     TEXT NOT NULL,
            PRIMARY KEY (plugin_id, key)
        );"]
}

/// What a claim is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owned {
    Tool,
    Job,
    Skill,
    Secret,
}

impl Owned {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Job => "job",
            Self::Skill => "skill",
            Self::Secret => "secret",
        }
    }
}

/// Claims `key` for `plugin`. Refuses a key another add-on owns; claiming one's own again
/// is fine.
pub fn claim(conn: &Connection, kind: Owned, key: &str, plugin: &PluginId) -> Result<(), String> {
    match owner(conn, kind, key).map_err(|e| e.to_string())? {
        Some(o) if o != plugin.as_str() => Err(format!(
            "the {} `{key}` already belongs to the add-on `{o}`",
            kind.as_str()
        )),
        Some(_) => Ok(()),
        None => conn
            .execute(
                "INSERT INTO app_addon_owned (kind, key, plugin_id) VALUES (?1, ?2, ?3)",
                params![kind.as_str(), key, plugin.as_str()],
            )
            .map(|_| ())
            .map_err(|e| e.to_string()),
    }
}

/// Who owns `key`, if anyone.
pub fn owner(conn: &Connection, kind: Owned, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT plugin_id FROM app_addon_owned WHERE kind = ?1 AND key = ?2",
        params![kind.as_str(), key],
        |r| r.get(0),
    )
    .optional()
}

/// Everything of `kind` that `plugin` owns.
pub fn owned(conn: &Connection, kind: Owned, plugin: &PluginId) -> rusqlite::Result<Vec<String>> {
    let mut st = conn.prepare(
        "SELECT key FROM app_addon_owned WHERE kind = ?1 AND plugin_id = ?2 ORDER BY key",
    )?;
    let v = st
        .query_map(params![kind.as_str(), plugin.as_str()], |r| r.get(0))?
        .collect();
    v
}

/// Every add-on that owns anything, whether or not it is in this build any more.
pub fn known_plugins(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut st = conn.prepare(
        "SELECT plugin_id FROM app_addon_owned UNION SELECT plugin_id FROM app_addon_state
         ORDER BY 1",
    )?;
    let v = st.query_map([], |r| r.get(0))?.collect();
    v
}

/// The stored state of an add-on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddonState {
    pub enabled: bool,
    pub asked: BTreeSet<Permission>,
    pub asked_version: Option<String>,
    pub allowed: BTreeSet<Permission>,
    pub chats: ChatScope,
    pub knowledge: Vec<Id>,
}

fn perm_name(p: Permission) -> &'static str {
    p.as_str()
}

fn perm_parse(s: &str) -> Option<Permission> {
    Permission::ALL.into_iter().find(|p| p.as_str() == s)
}

fn ids_json(v: &[Id]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

fn ids_parse(s: &str) -> rusqlite::Result<Vec<Id>> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub fn state(conn: &Connection, plugin: &PluginId) -> rusqlite::Result<AddonState> {
    let row = conn
        .query_row(
            "SELECT enabled, asked, asked_version, chats_all, chats_projects, knowledge
             FROM app_addon_state WHERE plugin_id = ?1",
            [plugin.as_str()],
            |r| {
                Ok((
                    r.get::<_, bool>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let mut st =
        conn.prepare("SELECT permission FROM app_addon_permissions WHERE plugin_id = ?1")?;
    let allowed: BTreeSet<Permission> = st
        .query_map([plugin.as_str()], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok().and_then(|s| perm_parse(&s)))
        .collect();
    Ok(match row {
        None => AddonState {
            enabled: true,
            asked: BTreeSet::new(),
            asked_version: None,
            allowed,
            chats: ChatScope::Projects(Vec::new()),
            knowledge: Vec::new(),
        },
        Some((enabled, asked, asked_version, all, projects, knowledge)) => {
            let asked: Vec<String> = serde_json::from_str(&asked).unwrap_or_default();
            AddonState {
                enabled,
                asked: asked.iter().filter_map(|s| perm_parse(s)).collect(),
                asked_version,
                allowed,
                chats: if all {
                    ChatScope::All
                } else {
                    ChatScope::Projects(ids_parse(&projects)?)
                },
                knowledge: ids_parse(&knowledge)?,
            }
        }
    })
}

fn ensure_state(conn: &Connection, plugin: &PluginId) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO app_addon_state (plugin_id) VALUES (?1)",
        [plugin.as_str()],
    )?;
    Ok(())
}

pub fn set_enabled(conn: &Connection, plugin: &PluginId, on: bool) -> rusqlite::Result<()> {
    ensure_state(conn, plugin)?;
    conn.execute(
        "UPDATE app_addon_state SET enabled = ?2 WHERE plugin_id = ?1",
        params![plugin.as_str(), on],
    )?;
    Ok(())
}

/// The permissions `manifest` asks for that the person hasn't been asked about yet.
pub fn not_asked(st: &AddonState, manifest: &PluginManifest) -> Vec<Permission> {
    Permission::ALL
        .into_iter()
        .filter(|p| manifest.asks_for(*p) && !st.asked.contains(p))
        .collect()
}

/// Saves the person's answer for the permissions in `asked`: those in `consent` are allowed
/// (with its scopes), the rest of `asked` aren't. Permissions answered earlier and not in
/// `asked` keep their answer.
pub fn save_consent(
    conn: &Connection,
    manifest: &PluginManifest,
    asked: &[Permission],
    consent: &Consent,
) -> rusqlite::Result<()> {
    let plugin = &manifest.id;
    let tx = conn.unchecked_transaction()?;
    ensure_state(&tx, plugin)?;
    let prev = state(&tx, plugin)?;
    let mut all_asked = prev.asked.clone();
    for p in asked {
        all_asked.insert(*p);
        if consent.permissions.contains(p) {
            tx.execute(
                "INSERT OR IGNORE INTO app_addon_permissions (plugin_id, permission)
                 VALUES (?1, ?2)",
                params![plugin.as_str(), perm_name(*p)],
            )?;
        } else {
            tx.execute(
                "DELETE FROM app_addon_permissions WHERE plugin_id = ?1 AND permission = ?2",
                params![plugin.as_str(), perm_name(*p)],
            )?;
        }
    }
    let asked_json: Vec<&str> = all_asked.iter().map(|p| p.as_str()).collect();
    let (all, projects) = match &consent.chats {
        ChatScope::All => (true, Vec::new()),
        ChatScope::Projects(p) => (false, p.clone()),
        _ => (false, Vec::new()),
    };
    // Scopes change only when the permission they belong to was part of this answer.
    let chats_answered = asked.contains(&Permission::ChatsRead);
    let knowledge_answered = asked.contains(&Permission::KnowledgeSearch);
    tx.execute(
        "UPDATE app_addon_state SET asked = ?2, asked_version = ?3,
            chats_all = CASE WHEN ?4 THEN ?5 ELSE chats_all END,
            chats_projects = CASE WHEN ?4 THEN ?6 ELSE chats_projects END,
            knowledge = CASE WHEN ?7 THEN ?8 ELSE knowledge END
         WHERE plugin_id = ?1",
        params![
            plugin.as_str(),
            serde_json::to_string(&asked_json).unwrap_or_else(|_| "[]".into()),
            manifest.version,
            chats_answered,
            all,
            ids_json(&projects),
            knowledge_answered,
            ids_json(&consent.knowledge_projects),
        ],
    )?;
    tx.commit()
}

/// Turns one permission that was asked about off, or back on. Turning on one that was never
/// asked about needs [`save_consent`] (the consent dialog).
pub fn set_permission(
    conn: &Connection,
    plugin: &PluginId,
    p: Permission,
    on: bool,
) -> Result<(), String> {
    let st = state(conn, plugin).map_err(|e| e.to_string())?;
    if on && !st.asked.contains(&p) {
        return Err(format!("`{}` needs the consent dialog first", p.as_str()));
    }
    let r = if on {
        conn.execute(
            "INSERT OR IGNORE INTO app_addon_permissions (plugin_id, permission) VALUES (?1, ?2)",
            params![plugin.as_str(), perm_name(p)],
        )
    } else {
        conn.execute(
            "DELETE FROM app_addon_permissions WHERE plugin_id = ?1 AND permission = ?2",
            params![plugin.as_str(), perm_name(p)],
        )
    };
    r.map(|_| ()).map_err(|e| e.to_string())
}

pub fn setting_get(
    conn: &Connection,
    plugin: &PluginId,
    key: &str,
) -> rusqlite::Result<Option<serde_json::Value>> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM app_addon_settings WHERE plugin_id = ?1 AND key = ?2",
            params![plugin.as_str(), key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.and_then(|s| serde_json::from_str(&s).ok()))
}

pub fn setting_set(
    conn: &Connection,
    plugin: &PluginId,
    key: &str,
    value: &serde_json::Value,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO app_addon_settings (plugin_id, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (plugin_id, key) DO UPDATE SET value = excluded.value",
        params![plugin.as_str(), key, value.to_string()],
    )?;
    Ok(())
}

pub fn setting_remove(conn: &Connection, plugin: &PluginId, key: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM app_addon_settings WHERE plugin_id = ?1 AND key = ?2",
        params![plugin.as_str(), key],
    )?;
    Ok(())
}

/// Removes everything stored for one add-on: ownership, state, consent and settings.
pub fn delete_plugin(conn: &Connection, plugin: &str) -> rusqlite::Result<()> {
    for table in [
        "app_addon_owned",
        "app_addon_state",
        "app_addon_permissions",
        "app_addon_settings",
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE plugin_id = ?1"),
            [plugin],
        )?;
    }
    Ok(())
}

/// "Delete all local data".
pub fn delete_all(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DELETE FROM app_addon_owned; DELETE FROM app_addon_state;
         DELETE FROM app_addon_permissions; DELETE FROM app_addon_settings;",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        for m in migrations() {
            c.execute_batch(m).unwrap();
        }
        c
    }

    fn id(s: &str) -> PluginId {
        PluginId::new(s).unwrap()
    }

    #[test]
    fn ownership_is_checked_against_the_table_not_by_prefix() {
        let c = db();
        claim(&c, Owned::Tool, "plugin.a.x", &id("a")).unwrap();
        claim(&c, Owned::Tool, "plugin.a.x", &id("a")).unwrap();
        let e = claim(&c, Owned::Tool, "plugin.a.x", &id("b")).unwrap_err();
        assert!(e.contains("belongs to the add-on `a`"), "{e}");
        // A key that merely starts with another add-on's id is nobody's until claimed.
        assert_eq!(owner(&c, Owned::Tool, "plugin.a.y").unwrap(), None);
        assert_eq!(owned(&c, Owned::Tool, &id("a")).unwrap(), ["plugin.a.x"]);
    }

    #[test]
    fn consent_is_per_permission_and_a_new_version_only_asks_for_the_new_ones() {
        let c = db();
        let m = PluginManifest::new(id("brief"), "Brief", "1.0")
            .with_permission(dn_plugin_api::PermissionRequest::Memory)
            .with_permission(dn_plugin_api::PermissionRequest::Notify);
        let st = state(&c, &m.id).unwrap();
        assert_eq!(not_asked(&st, &m), [Permission::Memory, Permission::Notify]);
        save_consent(
            &c,
            &m,
            &[Permission::Memory, Permission::Notify],
            &Consent::new([Permission::Memory]),
        )
        .unwrap();
        let st = state(&c, &m.id).unwrap();
        assert!(st.allowed.contains(&Permission::Memory));
        assert!(!st.allowed.contains(&Permission::Notify));
        assert!(not_asked(&st, &m).is_empty());
        let m2 = PluginManifest::new(id("brief"), "Brief", "1.1")
            .with_permission(dn_plugin_api::PermissionRequest::Memory)
            .with_permission(dn_plugin_api::PermissionRequest::Notify)
            .with_permission(dn_plugin_api::PermissionRequest::Jobs);
        assert_eq!(not_asked(&st, &m2), [Permission::Jobs]);
        // Turning off works; turning on one never asked about doesn't.
        set_permission(&c, &m.id, Permission::Memory, false).unwrap();
        assert!(
            !state(&c, &m.id)
                .unwrap()
                .allowed
                .contains(&Permission::Memory)
        );
        assert!(set_permission(&c, &m.id, Permission::Jobs, true).is_err());
        set_permission(&c, &m.id, Permission::Notify, true).unwrap();
        assert!(
            state(&c, &m.id)
                .unwrap()
                .allowed
                .contains(&Permission::Notify)
        );
    }

    #[test]
    fn settings_are_per_add_on_and_deleted_with_it() {
        let c = db();
        setting_set(&c, &id("a"), "tone", &serde_json::json!("short")).unwrap();
        setting_set(&c, &id("b"), "tone", &serde_json::json!("long")).unwrap();
        assert_eq!(
            setting_get(&c, &id("a"), "tone").unwrap(),
            Some(serde_json::json!("short"))
        );
        delete_plugin(&c, "a").unwrap();
        assert_eq!(setting_get(&c, &id("a"), "tone").unwrap(), None);
        assert!(setting_get(&c, &id("b"), "tone").unwrap().is_some());
        delete_all(&c).unwrap();
        assert_eq!(setting_get(&c, &id("b"), "tone").unwrap(), None);
    }
}
