//! What sync needs from dn-store's tables (docs/design/dn-sync.md §3.5), and nothing more.
//! dn-sync never touches `store_*` tables itself; it calls these functions.
//!
//! - **`store_sync_log`:** triggers on chats, finished messages and projects append the
//!   kind, the id and whether the row was deleted, with an increasing `rev`.
//!   - Sync asks for what changed since the last `rev` it handled ([`log_since`]).
//!   - It then reads the rows as they are now, and prunes the log ([`prune_log`]).
//!   - Messages are logged once they leave `streaming`, never while still streaming.
//! - **Rows:** each table is read and written as plain field values ([`ChatRow`],
//!   [`ProjectRow`], [`MessageRow`]). Writing a row from another device goes through the
//!   same table, so the log records it too; sync recognises its own writes by their values.
//! - **"Delete all local data"** ([`crate::chat::delete_all`]) also empties the log, so a
//!   local wipe is never sent to other devices as deletions.

use crate::chat::parse_id;
use crate::{Result, StoreError};
use dn_core::Id;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// dn-store migration #5 (in the `store` set): the sync switch, the change log and its
/// triggers. Nothing is logged until [`enable`]; no existing table is altered.
pub(crate) const MIGRATION: &str = "
-- Whether this device syncs (paired with a Node). Off on most installs: then the triggers
-- below log nothing, so the log never grows on a device that never pairs.
CREATE TABLE store_sync_state (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
);
INSERT INTO store_sync_state (id, enabled) VALUES (1, 0);

CREATE TABLE store_sync_log (
    rev     INTEGER PRIMARY KEY AUTOINCREMENT,
    kind    TEXT    NOT NULL CHECK (kind IN ('conversation', 'message', 'project')),
    id      TEXT    NOT NULL,
    deleted INTEGER NOT NULL CHECK (deleted IN (0, 1))
);

CREATE TRIGGER store_sync_conversation_insert AFTER INSERT ON store_conversations
WHEN (SELECT enabled FROM store_sync_state) = 1
BEGIN INSERT INTO store_sync_log (kind, id, deleted) VALUES ('conversation', NEW.id, 0); END;
CREATE TRIGGER store_sync_conversation_update AFTER UPDATE ON store_conversations
WHEN (SELECT enabled FROM store_sync_state) = 1
BEGIN INSERT INTO store_sync_log (kind, id, deleted) VALUES ('conversation', NEW.id, 0); END;
CREATE TRIGGER store_sync_conversation_delete AFTER DELETE ON store_conversations
WHEN (SELECT enabled FROM store_sync_state) = 1
BEGIN INSERT INTO store_sync_log (kind, id, deleted) VALUES ('conversation', OLD.id, 1); END;

CREATE TRIGGER store_sync_message_insert AFTER INSERT ON store_messages
WHEN NEW.status <> 'streaming' AND (SELECT enabled FROM store_sync_state) = 1
BEGIN INSERT INTO store_sync_log (kind, id, deleted) VALUES ('message', NEW.id, 0); END;
CREATE TRIGGER store_sync_message_update AFTER UPDATE ON store_messages
WHEN NEW.status <> 'streaming' AND (SELECT enabled FROM store_sync_state) = 1
BEGIN INSERT INTO store_sync_log (kind, id, deleted) VALUES ('message', NEW.id, 0); END;
CREATE TRIGGER store_sync_message_delete AFTER DELETE ON store_messages
WHEN OLD.status <> 'streaming' AND (SELECT enabled FROM store_sync_state) = 1
BEGIN INSERT INTO store_sync_log (kind, id, deleted) VALUES ('message', OLD.id, 1); END;

CREATE TRIGGER store_sync_project_insert AFTER INSERT ON store_projects
WHEN (SELECT enabled FROM store_sync_state) = 1
BEGIN INSERT INTO store_sync_log (kind, id, deleted) VALUES ('project', NEW.id, 0); END;
CREATE TRIGGER store_sync_project_update AFTER UPDATE ON store_projects
WHEN (SELECT enabled FROM store_sync_state) = 1
BEGIN INSERT INTO store_sync_log (kind, id, deleted) VALUES ('project', NEW.id, 0); END;
CREATE TRIGGER store_sync_project_delete AFTER DELETE ON store_projects
WHEN (SELECT enabled FROM store_sync_state) = 1
BEGIN INSERT INTO store_sync_log (kind, id, deleted) VALUES ('project', OLD.id, 1); END;
";

/// Turns logging on (when this device pairs) and logs what is already here once, parents
/// first, so the other devices get it. Turning it on again does nothing.
pub fn enable(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let on: bool = tx.query_row("SELECT enabled FROM store_sync_state", [], |r| r.get(0))?;
    if !on {
        tx.execute("UPDATE store_sync_state SET enabled = 1", [])?;
        tx.execute_batch(
            "INSERT INTO store_sync_log (kind, id, deleted)
                 SELECT 'project', id, 0 FROM store_projects ORDER BY created_at, id;
             INSERT INTO store_sync_log (kind, id, deleted)
                 SELECT 'conversation', id, 0 FROM store_conversations ORDER BY created_at, id;
             INSERT INTO store_sync_log (kind, id, deleted)
                 SELECT 'message', id, 0 FROM store_messages WHERE status <> 'streaming'
                 ORDER BY created_at, id;",
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Turns logging off (unpaired) and empties the log.
pub fn disable(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE store_sync_state SET enabled = 0", [])?;
    tx.execute("DELETE FROM store_sync_log", [])?;
    tx.commit()?;
    Ok(())
}

pub fn enabled(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row("SELECT enabled FROM store_sync_state", [], |r| r.get(0))?)
}

/// Checks that `text` is an id in the one form this app writes (lowercase, hyphenated
/// UUID). Ids from other devices must be in that form: another spelling of the same UUID
/// would make a second row the app can never address, and slip past a tombstone.
pub fn canonical_id(text: &str) -> Result<Id> {
    let id = parse_id(text)?;
    if id.to_string() != text {
        return Err(StoreError::Invalid(format!(
            "id `{text}` isn't in canonical form"
        )));
    }
    Ok(id)
}

/// Which table a log entry is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Conversation,
    Message,
    Project,
}

impl Kind {
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "conversation" => Self::Conversation,
            "message" => Self::Message,
            "project" => Self::Project,
            other => return Err(StoreError::Invalid(format!("sync log kind `{other}`"))),
        })
    }
}

/// The latest log entry for one row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub rev: i64,
    pub kind: Kind,
    pub id: String,
    pub deleted: bool,
}

/// What changed after `rev`: one entry per row, the latest, in log order. Children come
/// after their parent when both changed (a chat before its messages).
pub fn log_since(conn: &Connection, rev: i64) -> Result<Vec<LogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT l.rev, l.kind, l.id, l.deleted FROM store_sync_log l
         JOIN (SELECT kind, id, MAX(rev) AS rev FROM store_sync_log WHERE rev > ?1
               GROUP BY kind, id) last ON last.rev = l.rev
         ORDER BY l.rev",
    )?;
    let rows = stmt.query_map([rev], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, bool>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (rev, kind, id, deleted) = row?;
        out.push(LogEntry {
            rev,
            kind: Kind::parse(&kind)?,
            id,
            deleted,
        });
    }
    Ok(out)
}

/// Drops log entries at or before `rev` (sync has handled them).
pub fn prune_log(conn: &Connection, rev: i64) -> Result<()> {
    conn.execute("DELETE FROM store_sync_log WHERE rev <= ?1", [rev])?;
    Ok(())
}

/// A chat's synced fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRow {
    pub id: String,
    pub title: String,
    pub model: Option<String>,
    pub project_id: Option<String>,
    pub deleted_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn chat(conn: &Connection, id: &str) -> Result<Option<ChatRow>> {
    Ok(conn
        .query_row(
            "SELECT id, title, model, project_id, deleted_at, created_at, updated_at
             FROM store_conversations WHERE id = ?1",
            [id],
            |r| {
                Ok(ChatRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    model: r.get(2)?,
                    project_id: r.get(3)?,
                    deleted_at: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            },
        )
        .optional()?)
}

/// Writes a chat from another device: inserts it, or updates the given row.
pub fn put_chat(conn: &Connection, row: &ChatRow) -> Result<()> {
    canonical_id(&row.id)?;
    if let Some(p) = &row.project_id {
        canonical_id(p)?;
    }
    conn.execute(
        "INSERT INTO store_conversations
            (id, title, model, project_id, deleted_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (id) DO UPDATE SET
            title = excluded.title, model = excluded.model, project_id = excluded.project_id,
            deleted_at = excluded.deleted_at, created_at = excluded.created_at,
            updated_at = excluded.updated_at",
        params![
            row.id,
            row.title,
            row.model,
            row.project_id,
            row.deleted_at,
            row.created_at,
            row.updated_at
        ],
    )?;
    Ok(())
}

/// Deletes a chat for good, from another device: its messages go, a tombstone stays
/// (as [`crate::chat::purge_deleted`] does). Run it inside the caller's transaction.
pub fn purge_chat(tx: &Connection, id: &str, deleted_at: i64) -> Result<()> {
    canonical_id(id)?;
    tx.execute(
        "INSERT OR REPLACE INTO store_tombstones (id, deleted_at, kind) VALUES (?1, ?2, 'conversation')",
        params![id, deleted_at],
    )?;
    tx.execute(
        "DELETE FROM store_messages WHERE conversation_id = ?1",
        [id],
    )?;
    tx.execute("DELETE FROM store_conversations WHERE id = ?1", [id])?;
    Ok(())
}

/// The ids of a chat's messages (to record them as gone when the chat is purged).
pub fn chat_message_ids(conn: &Connection, chat_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id FROM store_messages WHERE conversation_id = ?1")?;
    let rows = stmt.query_map([chat_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A project's synced fields (its files sync later, with their contents).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    pub instructions: String,
    pub deleted_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn project(conn: &Connection, id: &str) -> Result<Option<ProjectRow>> {
    Ok(conn
        .query_row(
            "SELECT id, name, instructions, deleted_at, created_at, updated_at
             FROM store_projects WHERE id = ?1",
            [id],
            |r| {
                Ok(ProjectRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    instructions: r.get(2)?,
                    deleted_at: r.get(3)?,
                    created_at: r.get(4)?,
                    updated_at: r.get(5)?,
                })
            },
        )
        .optional()?)
}

pub fn put_project(conn: &Connection, row: &ProjectRow) -> Result<()> {
    canonical_id(&row.id)?;
    conn.execute(
        "INSERT INTO store_projects (id, name, instructions, deleted_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (id) DO UPDATE SET
            name = excluded.name, instructions = excluded.instructions,
            deleted_at = excluded.deleted_at, created_at = excluded.created_at,
            updated_at = excluded.updated_at",
        params![
            row.id,
            row.name,
            row.instructions,
            row.deleted_at,
            row.created_at,
            row.updated_at
        ],
    )?;
    Ok(())
}

/// Deletes a project for good, from another device. Its chats were purged by their own
/// changes; its files' rows go with it (their local copies are cleaned up at start). Run it
/// inside the caller's transaction.
pub fn purge_project(tx: &Connection, id: &str, deleted_at: i64) -> Result<()> {
    canonical_id(id)?;
    tx.execute(
        "INSERT OR REPLACE INTO store_tombstones (id, deleted_at, kind) VALUES (?1, ?2, 'project')",
        params![id, deleted_at],
    )?;
    tx.execute(
        "UPDATE store_conversations SET project_id = NULL WHERE project_id = ?1",
        [id],
    )?;
    tx.execute("DELETE FROM store_projects WHERE id = ?1", [id])?;
    Ok(())
}

/// A finished message, as stored (content and tool calls as their JSON text).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRow {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub tool_calls: String,
    pub tool_call_id: Option<String>,
    pub model: Option<String>,
    pub status: String,
    pub finish_reason: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn message(conn: &Connection, id: &str) -> Result<Option<MessageRow>> {
    Ok(conn
        .query_row(
            "SELECT id, conversation_id, role, content, tool_calls, tool_call_id, model, status,
                    finish_reason, error, created_at, updated_at
             FROM store_messages WHERE id = ?1 AND status <> 'streaming'",
            [id],
            |r| {
                Ok(MessageRow {
                    id: r.get(0)?,
                    conversation_id: r.get(1)?,
                    role: r.get(2)?,
                    content: r.get(3)?,
                    tool_calls: r.get(4)?,
                    tool_call_id: r.get(5)?,
                    model: r.get(6)?,
                    status: r.get(7)?,
                    finish_reason: r.get(8)?,
                    error: r.get(9)?,
                    created_at: r.get(10)?,
                    updated_at: r.get(11)?,
                })
            },
        )
        .optional()?)
}

/// Checks a message from another device: canonical ids, and every field what this app
/// writes, so the chat still reads afterwards. A streaming message is refused. Returns its
/// chat's id.
pub fn check_message(row: &MessageRow) -> Result<Id> {
    canonical_id(&row.id)?;
    let chat = canonical_id(&row.conversation_id)?;
    let typed = |v: &str| serde_json::Value::String(v.to_owned());
    serde_json::from_value::<dn_core::chat::Role>(typed(&row.role))?;
    serde_json::from_value::<crate::chat::MessageStatus>(typed(&row.status))?;
    if let Some(f) = &row.finish_reason {
        serde_json::from_value::<dn_core::chat::FinishReason>(typed(f))?;
    }
    serde_json::from_str::<Vec<dn_core::chat::ContentPart>>(&row.content)?;
    serde_json::from_str::<Vec<dn_core::chat::ToolCall>>(&row.tool_calls)?;
    if row.status == "streaming" {
        return Err(StoreError::Invalid(
            "a streaming message isn't synced".into(),
        ));
    }
    Ok(chat)
}

/// Writes a finished message from another device. Its chat must exist; a message already
/// here is left as it is (messages never change once finished).
pub fn insert_message(conn: &Connection, row: &MessageRow) -> Result<bool> {
    let chat = check_message(row)?;
    // Its chat must be here, live or in the trash.
    let here: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM store_conversations WHERE id = ?1",
            [chat.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if here.is_none() {
        return Err(StoreError::NotFound(format!("conversation {chat}")));
    }
    let n = conn.execute(
        "INSERT OR IGNORE INTO store_messages
           (id, conversation_id, role, content, tool_calls, tool_call_id, model, status,
            finish_reason, error, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            row.id,
            row.conversation_id,
            row.role,
            row.content,
            row.tool_calls,
            row.tool_call_id,
            row.model,
            row.status,
            row.finish_reason,
            row.error,
            row.created_at,
            row.updated_at
        ],
    )?;
    Ok(n == 1)
}

/// Removes a message, from another device (an answer regenerated there).
pub fn delete_message(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM store_messages WHERE id = ?1", [id])?;
    Ok(())
}

/// The id as a [`dn_core::Id`], for callers that need one.
pub fn id(text: &str) -> Result<Id> {
    parse_id(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{self, MessageOutcome, MessageStatus, NewMessage};
    use crate::migrate::{MigrationSet, run_migrations};

    #[test]
    fn an_existing_database_migrates_in_place_and_logs_what_it_has() {
        // A database as the current release left it: every store migration before this one.
        let all = crate::migrations();
        let before: &'static [&'static str] = &all[..all.len() - 1];
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        run_migrations(&mut conn, &[MigrationSet::new("store", before)]).unwrap();
        let p = crate::project::create_project(&conn, "Trip").unwrap();
        let c = chat::create_conversation_in(&conn, "Plans", Some(p.id)).unwrap();
        let done = chat::add_message(&conn, c.id, NewMessage::user_text("Hi")).unwrap();
        let streaming =
            chat::add_message(&conn, c.id, NewMessage::assistant_placeholder(None)).unwrap();

        // Then the new migration runs on it. Nothing is logged until sync is turned on.
        run_migrations(&mut conn, &[MigrationSet::new("store", all)]).unwrap();
        assert!(log_since(&conn, 0).unwrap().is_empty());
        enable(&conn).unwrap();
        let log = log_since(&conn, 0).unwrap();
        let kinds: Vec<_> = log.iter().map(|e| (e.kind, e.id.clone())).collect();
        assert_eq!(
            kinds,
            vec![
                (Kind::Project, p.id.to_string()),
                (Kind::Conversation, c.id.to_string()),
                (Kind::Message, done.id.to_string()),
            ],
            "existing rows are logged once, parents first, and the streaming answer isn't"
        );
        // The data itself is unchanged.
        assert_eq!(chat::get_conversation(&conn, c.id).unwrap().title, "Plans");
        assert_eq!(chat::list_messages(&conn, c.id).unwrap().len(), 2);

        // Finishing the answer logs it; while it streamed, nothing was logged.
        let last = log.last().unwrap().rev;
        chat::finish_message(
            &conn,
            streaming.id,
            MessageOutcome {
                content: vec![],
                tool_calls: vec![],
                status: MessageStatus::Complete,
                finish_reason: None,
                error: None,
            },
        )
        .unwrap();
        let after = log_since(&conn, last).unwrap();
        assert!(
            after
                .iter()
                .any(|e| e.kind == Kind::Message && e.id == streaming.id.to_string())
        );
    }

    fn db() -> Connection {
        let conn = crate::open_in_memory(&[]).unwrap();
        enable(&conn).unwrap();
        conn
    }

    #[test]
    fn a_device_that_never_pairs_logs_nothing() {
        let conn = crate::open_in_memory(&[]).unwrap();
        assert!(!enabled(&conn).unwrap());
        let c = chat::create_conversation(&conn, "A").unwrap();
        chat::rename_conversation(&conn, c.id, "B").unwrap();
        let m = chat::add_message(&conn, c.id, NewMessage::user_text("x")).unwrap();
        chat::truncate_from(&conn, c.id, m.id).unwrap();
        crate::project::create_project(&conn, "P").unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM store_sync_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        // Turning sync on logs what's here once; turning it off empties the log.
        enable(&conn).unwrap();
        enable(&conn).unwrap();
        assert_eq!(log_since(&conn, 0).unwrap().len(), 2);
        disable(&conn).unwrap();
        assert!(log_since(&conn, 0).unwrap().is_empty());
        chat::create_conversation(&conn, "C").unwrap();
        assert!(log_since(&conn, 0).unwrap().is_empty());
    }

    #[test]
    fn edits_and_deletes_are_logged_latest_first_and_pruned() {
        let conn = db();
        let c = chat::create_conversation(&conn, "A").unwrap();
        chat::rename_conversation(&conn, c.id, "B").unwrap();
        let log = log_since(&conn, 0).unwrap();
        assert_eq!(log.len(), 1, "one entry per row, the latest");
        let m = chat::add_message(&conn, c.id, NewMessage::user_text("x")).unwrap();
        chat::truncate_from(&conn, c.id, m.id).unwrap();
        let log = log_since(&conn, 0).unwrap();
        let msg = log.iter().find(|e| e.kind == Kind::Message).unwrap();
        assert!(msg.deleted, "a regenerated answer's removal is logged");
        prune_log(&conn, log.last().unwrap().rev).unwrap();
        assert!(log_since(&conn, 0).unwrap().is_empty());
    }

    #[test]
    fn rows_from_another_device_round_trip_and_purge_leaves_a_tombstone() {
        let conn = db();
        let p = ProjectRow {
            id: Id::new().to_string(),
            name: "Trip".into(),
            instructions: "Be brief".into(),
            deleted_at: None,
            created_at: 1,
            updated_at: 2,
        };
        put_project(&conn, &p).unwrap();
        assert_eq!(project(&conn, &p.id).unwrap().unwrap(), p);
        let c = ChatRow {
            id: Id::new().to_string(),
            title: "Plans".into(),
            model: None,
            project_id: Some(p.id.clone()),
            deleted_at: None,
            created_at: 3,
            updated_at: 4,
        };
        put_chat(&conn, &c).unwrap();
        put_chat(
            &conn,
            &ChatRow {
                title: "Plans v2".into(),
                ..c.clone()
            },
        )
        .unwrap();
        assert_eq!(chat(&conn, &c.id).unwrap().unwrap().title, "Plans v2");
        let m = MessageRow {
            id: Id::new().to_string(),
            conversation_id: c.id.clone(),
            role: "user".into(),
            content: "[{\"type\":\"text\",\"text\":\"hi\"}]".into(),
            tool_calls: "[]".into(),
            tool_call_id: None,
            model: None,
            status: "complete".into(),
            finish_reason: None,
            error: None,
            created_at: 5,
            updated_at: 5,
        };
        assert!(insert_message(&conn, &m).unwrap());
        assert!(
            !insert_message(&conn, &m).unwrap(),
            "a message never changes"
        );
        assert_eq!(message(&conn, &m.id).unwrap().unwrap(), m);
        purge_chat(&conn, &c.id, 9).unwrap();
        assert!(chat(&conn, &c.id).unwrap().is_none());
        assert!(message(&conn, &m.id).unwrap().is_none());
        assert!(chat::is_purged(&conn, &c.id).unwrap());
        // A message for a chat that isn't here is refused.
        assert!(insert_message(&conn, &m).is_err());
        purge_project(&conn, &p.id, 10).unwrap();
        assert!(project(&conn, &p.id).unwrap().is_none());
    }

    #[test]
    fn ids_in_another_form_and_messages_this_app_wouldnt_write_are_refused() {
        let conn = db();
        let id = Id::new().to_string();
        for bad in [
            id.to_uppercase(),
            format!("{{{id}}}"),
            id.replace('-', ""),
            format!("urn:uuid:{id}"),
        ] {
            let row = ChatRow {
                id: bad.clone(),
                title: "x".into(),
                model: None,
                project_id: None,
                deleted_at: None,
                created_at: 1,
                updated_at: 1,
            };
            assert!(put_chat(&conn, &row).is_err(), "{bad}");
            assert!(purge_chat(&conn, &bad, 1).is_err(), "{bad}");
        }
        let c = chat::create_conversation(&conn, "c").unwrap();
        let ok = MessageRow {
            id: Id::new().to_string(),
            conversation_id: c.id.to_string(),
            role: "user".into(),
            content: "[{\"type\":\"text\",\"text\":\"hi\"}]".into(),
            tool_calls: "[]".into(),
            tool_call_id: None,
            model: None,
            status: "complete".into(),
            finish_reason: None,
            error: None,
            created_at: 5,
            updated_at: 5,
        };
        for bad in [
            MessageRow {
                role: "root".into(),
                ..ok.clone()
            },
            MessageRow {
                status: "done".into(),
                ..ok.clone()
            },
            MessageRow {
                content: "{\"not\":\"parts\"}".into(),
                ..ok.clone()
            },
            MessageRow {
                tool_calls: "[1]".into(),
                ..ok.clone()
            },
            MessageRow {
                finish_reason: Some("whenever".into()),
                ..ok.clone()
            },
        ] {
            assert!(insert_message(&conn, &bad).is_err(), "{bad:?}");
        }
        assert!(insert_message(&conn, &ok).unwrap());
    }

    #[test]
    fn a_streaming_placeholder_removed_here_isnt_logged() {
        let conn = db();
        let c = chat::create_conversation(&conn, "c").unwrap();
        let rev = log_since(&conn, 0).unwrap().last().unwrap().rev;
        let m = chat::add_message(&conn, c.id, NewMessage::assistant_placeholder(None)).unwrap();
        chat::truncate_from(&conn, c.id, m.id).unwrap();
        assert!(
            log_since(&conn, rev)
                .unwrap()
                .iter()
                .all(|e| e.kind != Kind::Message)
        );
    }

    #[test]
    fn delete_all_empties_the_log_so_a_wipe_is_never_sent() {
        let conn = db();
        chat::create_conversation(&conn, "A").unwrap();
        chat::delete_all(&conn).unwrap();
        assert!(log_since(&conn, 0).unwrap().is_empty());
    }
}
