//! Conversations and messages. Ids are UUIDv7 ([`dn_core::Id`]); every row carries
//! `created_at` / `updated_at` in Unix milliseconds so selected tables can sync later
//! (ADR-0008). Message content is stored as `dn_core::chat::ContentPart` JSON so the
//! store never lags behind the contract.

use crate::{Result, StoreError, now_ms};
use dn_core::Id;
use dn_core::chat::{ChatMessage, ContentPart, FinishReason, Role, ToolCall};
use rusqlite::{Connection, OptionalExtension, Row, params};

use serde::{Deserialize, Serialize};

pub(crate) const MIGRATIONS: &[&str] = &[
    "
CREATE TABLE store_conversations (
    id         TEXT    PRIMARY KEY NOT NULL,
    title      TEXT    NOT NULL,
    model      TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER
);
CREATE INDEX store_conversations_updated ON store_conversations (updated_at DESC);

CREATE TABLE store_messages (
    id              TEXT    PRIMARY KEY NOT NULL,
    conversation_id TEXT    NOT NULL REFERENCES store_conversations (id) ON DELETE CASCADE,
    role            TEXT    NOT NULL,
    content         TEXT    NOT NULL,
    tool_calls      TEXT    NOT NULL DEFAULT '[]',
    tool_call_id    TEXT,
    model           TEXT,
    status          TEXT    NOT NULL,
    finish_reason   TEXT,
    error           TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX store_messages_conversation ON store_messages (conversation_id, created_at, id);
",
    "
ALTER TABLE store_conversations ADD COLUMN purged_at INTEGER;
",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: Id,
    pub title: String,
    /// `ModelRef` string last used in this conversation.
    pub model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Lifecycle of a stored message. Assistant messages start `Streaming` and end in one of
/// the other states; user messages are written `Complete`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Streaming,
    Complete,
    Cancelled,
    Error,
}

impl MessageStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: Id,
    pub conversation_id: Id,
    pub role: Role,
    pub content: Vec<ContentPart>,
    pub tool_calls: Vec<ToolCall>,
    pub tool_call_id: Option<String>,
    pub model: Option<String>,
    pub status: MessageStatus,
    pub finish_reason: Option<FinishReason>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl StoredMessage {
    /// The plain text of all text parts, concatenated.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::Image { .. } => None,
            })
            .collect()
    }

    /// The `dn-core` message to send back to a model.
    pub fn to_chat_message(&self) -> ChatMessage {
        ChatMessage {
            role: self.role,
            content: self.content.clone(),
            tool_calls: self.tool_calls.clone(),
            tool_call_id: self.tool_call_id.clone(),
        }
    }
}

/// What to write for a new message.
#[derive(Clone, Debug)]
pub struct NewMessage {
    pub role: Role,
    pub content: Vec<ContentPart>,
    /// Required when `role == Tool`: the call this message answers.
    pub tool_call_id: Option<String>,
    pub model: Option<String>,
    pub status: MessageStatus,
}

impl NewMessage {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentPart::Text { text: text.into() }],
            tool_call_id: None,
            model: None,
            status: MessageStatus::Complete,
        }
    }

    /// A tool result answering `tool_call_id`.
    pub fn tool_result(tool_call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentPart::Text { text: text.into() }],
            tool_call_id: Some(tool_call_id.into()),
            model: None,
            status: MessageStatus::Complete,
        }
    }

    /// An empty assistant message that a stream will fill.
    pub fn assistant_placeholder(model: Option<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![],
            tool_call_id: None,
            model,
            status: MessageStatus::Streaming,
        }
    }
}

/// Final state written when a stream ends.
#[derive(Clone, Debug)]
pub struct MessageOutcome {
    pub content: Vec<ContentPart>,
    pub tool_calls: Vec<ToolCall>,
    pub status: MessageStatus,
    pub finish_reason: Option<FinishReason>,
    pub error: Option<String>,
}

fn enum_str<T: Serialize>(v: &T) -> Result<String> {
    match serde_json::to_value(v)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(StoreError::Invalid(format!("not a unit enum: {other}"))),
    }
}

fn enum_from<T: for<'de> Deserialize<'de>>(s: &str) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(
        s.to_string(),
    ))?)
}

fn parse_id(s: &str) -> Result<Id> {
    uuid::Uuid::parse_str(s)
        .map(Id)
        .map_err(|e| StoreError::Invalid(format!("id `{s}`: {e}")))
}

fn conversation_from_row(r: &Row<'_>) -> Result<Conversation> {
    Ok(Conversation {
        id: parse_id(&r.get::<_, String>(0)?)?,
        title: r.get(1)?,
        model: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
    })
}

const CONVERSATION_COLS: &str = "id, title, model, created_at, updated_at";

pub fn create_conversation(conn: &Connection, title: &str) -> Result<Conversation> {
    let now = now_ms();
    let c = Conversation {
        id: Id::new(),
        title: title.to_string(),
        model: None,
        created_at: now,
        updated_at: now,
    };
    conn.execute(
        "INSERT INTO store_conversations (id, title, model, created_at, updated_at)
         VALUES (?1, ?2, NULL, ?3, ?3)",
        params![c.id.to_string(), c.title, now],
    )?;
    Ok(c)
}

/// Live (not deleted) conversations, most recently updated first.
pub fn list_conversations(conn: &Connection) -> Result<Vec<Conversation>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CONVERSATION_COLS} FROM store_conversations
         WHERE deleted_at IS NULL ORDER BY updated_at DESC, id DESC"
    ))?;
    let mut rows = stmt.query([])?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        out.push(conversation_from_row(r)?);
    }
    Ok(out)
}

pub fn get_conversation(conn: &Connection, id: Id) -> Result<Conversation> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CONVERSATION_COLS} FROM store_conversations
         WHERE id = ?1 AND deleted_at IS NULL"
    ))?;
    let mut rows = stmt.query([id.to_string()])?;
    match rows.next()? {
        Some(r) => conversation_from_row(r),
        None => Err(StoreError::NotFound(format!("conversation {id}"))),
    }
}

pub fn rename_conversation(conn: &Connection, id: Id, title: &str) -> Result<()> {
    touch(conn, id, "title = ?2", title)
}

pub fn set_conversation_model(conn: &Connection, id: Id, model: &str) -> Result<()> {
    touch(conn, id, "model = ?2", model)
}

fn touch(conn: &Connection, id: Id, set: &str, value: &str) -> Result<()> {
    let n = conn.execute(
        &format!(
            "UPDATE store_conversations SET {set}, updated_at = ?3
             WHERE id = ?1 AND deleted_at IS NULL"
        ),
        params![id.to_string(), value, now_ms()],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("conversation {id}")));
    }
    Ok(())
}

/// Soft-deletes a conversation: it disappears from lists and can be brought back with
/// [`restore_conversation`] until [`purge_deleted`] removes its messages. The row stays as
/// a tombstone sync can carry (ADR-0008).
pub fn delete_conversation(conn: &Connection, id: Id) -> Result<()> {
    let now = now_ms();
    let n = conn.execute(
        "UPDATE store_conversations SET deleted_at = ?2, updated_at = ?2
         WHERE id = ?1 AND deleted_at IS NULL",
        params![id.to_string(), now],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("conversation {id}")));
    }
    Ok(())
}

/// Undoes [`delete_conversation`] if the messages have not been purged yet.
pub fn restore_conversation(conn: &Connection, id: Id) -> Result<()> {
    let n = conn.execute(
        "UPDATE store_conversations SET deleted_at = NULL, updated_at = ?2
         WHERE id = ?1 AND deleted_at IS NOT NULL AND purged_at IS NULL",
        params![id.to_string(), now_ms()],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("deleted conversation {id}")));
    }
    Ok(())
}

/// Permanently removes the messages of conversations deleted at or before `before_ms`
/// (the tombstone row stays). Returns how many conversations were purged.
pub fn purge_deleted(conn: &Connection, before_ms: i64) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM store_messages WHERE conversation_id IN (
             SELECT id FROM store_conversations
             WHERE deleted_at IS NOT NULL AND deleted_at <= ?1 AND purged_at IS NULL)",
        [before_ms],
    )?;
    let n = tx.execute(
        "UPDATE store_conversations SET purged_at = ?2
         WHERE deleted_at IS NOT NULL AND deleted_at <= ?1 AND purged_at IS NULL",
        params![before_ms, now_ms()],
    )?;
    tx.commit()?;
    Ok(n)
}

pub fn add_message(conn: &Connection, conversation_id: Id, m: NewMessage) -> Result<StoredMessage> {
    let tx = conn.unchecked_transaction()?;
    // Fail with NotFound rather than a foreign-key error.
    get_conversation(&tx, conversation_id)?;
    let now = now_ms();
    let msg = StoredMessage {
        id: Id::new(),
        conversation_id,
        role: m.role,
        content: m.content,
        tool_calls: vec![],
        tool_call_id: m.tool_call_id,
        model: m.model,
        status: m.status,
        finish_reason: None,
        error: None,
        created_at: now,
        updated_at: now,
    };
    // DELIBERATELY BROKEN (T-SHELL-05 e2e proof, reverted in the next commit):
    /* tx.execute(
        "INSERT INTO store_messages
           (id, conversation_id, role, content, tool_calls, tool_call_id, model, status,
            finish_reason, error, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, '[]', ?5, ?6, ?7, NULL, NULL, ?8, ?8)",
        params![
            msg.id.to_string(),
            conversation_id.to_string(),
            enum_str(&msg.role)?,
            serde_json::to_string(&msg.content)?,
            msg.tool_call_id,
            msg.model,
            msg.status.as_str(),
            now,
        ],
    )?; */
    bump_conversation(&tx, conversation_id, now)?;
    tx.commit()?;
    Ok(msg)
}

/// Writes the final content and status of a (streamed) message.
pub fn finish_message(conn: &Connection, id: Id, o: MessageOutcome) -> Result<()> {
    let now = now_ms();
    let finish = o.finish_reason.as_ref().map(enum_str).transpose()?;
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "UPDATE store_messages
         SET content = ?2, tool_calls = ?3, status = ?4, finish_reason = ?5, error = ?6,
             updated_at = ?7
         WHERE id = ?1",
        params![
            id.to_string(),
            serde_json::to_string(&o.content)?,
            serde_json::to_string(&o.tool_calls)?,
            o.status.as_str(),
            finish,
            o.error,
            now,
        ],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("message {id}")));
    }
    let conv: String = tx.query_row(
        "SELECT conversation_id FROM store_messages WHERE id = ?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    bump_conversation(&tx, parse_id(&conv)?, now)?;
    tx.commit()?;
    Ok(())
}

fn bump_conversation(conn: &Connection, id: Id, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE store_conversations SET updated_at = ?2 WHERE id = ?1",
        params![id.to_string(), now],
    )?;
    Ok(())
}

/// Messages of a conversation in order (UUIDv7 ids sort by creation time).
pub fn list_messages(conn: &Connection, conversation_id: Id) -> Result<Vec<StoredMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, role, content, tool_calls, tool_call_id, model, status,
                finish_reason, error, created_at, updated_at
         FROM store_messages WHERE conversation_id = ?1 ORDER BY created_at, id",
    )?;
    let mut rows = stmt.query([conversation_id.to_string()])?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        let role: String = r.get(2)?;
        let content: String = r.get(3)?;
        let tool_calls: String = r.get(4)?;
        let status: String = r.get(7)?;
        let finish: Option<String> = r.get(8)?;
        out.push(StoredMessage {
            id: parse_id(&r.get::<_, String>(0)?)?,
            conversation_id: parse_id(&r.get::<_, String>(1)?)?,
            role: enum_from(&role)?,
            content: serde_json::from_str(&content)?,
            tool_calls: serde_json::from_str(&tool_calls)?,
            tool_call_id: r.get(5)?,
            model: r.get(6)?,
            status: enum_from(&status)?,
            finish_reason: finish.as_deref().map(enum_from).transpose()?,
            error: r.get(9)?,
            created_at: r.get(10)?,
            updated_at: r.get(11)?,
        });
    }
    Ok(out)
}

/// Deletes `from` and every later message in its conversation (used by regenerate and
/// edit-and-resend). Returns how many rows were removed.
pub fn truncate_from(conn: &Connection, conversation_id: Id, from: Id) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let pos: Option<(i64, String)> = tx
        .query_row(
            "SELECT created_at, id FROM store_messages WHERE id = ?1 AND conversation_id = ?2",
            params![from.to_string(), conversation_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (created_at, id) = pos.ok_or_else(|| {
        StoreError::NotFound(format!("message {from} in conversation {conversation_id}"))
    })?;
    let n = tx.execute(
        "DELETE FROM store_messages
         WHERE conversation_id = ?1 AND (created_at, id) >= (?2, ?3)",
        params![conversation_id.to_string(), created_at, id],
    )?;
    bump_conversation(&tx, conversation_id, now_ms())?;
    tx.commit()?;
    Ok(n)
}

/// Marks assistant messages left `streaming` by a crash or forced quit as cancelled.
/// Call once at startup.
pub fn recover_interrupted(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE store_messages SET status = 'cancelled', finish_reason = 'cancelled',
                updated_at = ?1
         WHERE status = 'streaming'",
        [now_ms()],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    fn text(t: &str) -> Vec<ContentPart> {
        vec![ContentPart::Text { text: t.into() }]
    }

    #[test]
    fn conversation_round_trip() {
        let c = open_in_memory(&[]).unwrap();
        let a = create_conversation(&c, "First").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = create_conversation(&c, "Second").unwrap();
        assert_eq!(a.id.0.get_version_num(), 7);
        let list = list_conversations(&c).unwrap();
        assert_eq!(list.iter().map(|c| c.id).collect::<Vec<_>>(), [b.id, a.id]);

        std::thread::sleep(std::time::Duration::from_millis(2));
        rename_conversation(&c, a.id, "Renamed").unwrap();
        set_conversation_model(&c, a.id, "local:fake").unwrap();
        let got = get_conversation(&c, a.id).unwrap();
        assert_eq!(got.title, "Renamed");
        assert_eq!(got.model.as_deref(), Some("local:fake"));
        assert!(got.updated_at > a.updated_at);
        assert_eq!(list_conversations(&c).unwrap()[0].id, a.id);

        delete_conversation(&c, a.id).unwrap();
        assert!(matches!(
            get_conversation(&c, a.id),
            Err(StoreError::NotFound(_))
        ));
        assert_eq!(list_conversations(&c).unwrap().len(), 1);
    }

    #[test]
    fn streamed_message_lifecycle() {
        let c = open_in_memory(&[]).unwrap();
        let conv = create_conversation(&c, "t").unwrap();
        let u = add_message(&c, conv.id, NewMessage::user_text("hi")).unwrap();
        let a = add_message(
            &c,
            conv.id,
            NewMessage::assistant_placeholder(Some("local:fake".into())),
        )
        .unwrap();
        assert_eq!(a.status, MessageStatus::Streaming);
        finish_message(
            &c,
            a.id,
            MessageOutcome {
                content: text("hello **there**"),
                tool_calls: vec![],
                status: MessageStatus::Complete,
                finish_reason: Some(FinishReason::Stop),
                error: None,
            },
        )
        .unwrap();
        let msgs = list_messages(&c, conv.id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, u.id);
        assert_eq!(msgs[0].text(), "hi");
        assert_eq!(msgs[1].text(), "hello **there**");
        assert_eq!(msgs[1].finish_reason, Some(FinishReason::Stop));
        assert_eq!(msgs[1].to_chat_message().role, Role::Assistant);

        assert_eq!(truncate_from(&c, conv.id, a.id).unwrap(), 1);
        assert_eq!(list_messages(&c, conv.id).unwrap().len(), 1);
    }

    #[test]
    fn recovers_interrupted_streams() {
        let c = open_in_memory(&[]).unwrap();
        let conv = create_conversation(&c, "t").unwrap();
        add_message(&c, conv.id, NewMessage::assistant_placeholder(None)).unwrap();
        assert_eq!(recover_interrupted(&c).unwrap(), 1);
        let m = &list_messages(&c, conv.id).unwrap()[0];
        assert_eq!(m.status, MessageStatus::Cancelled);
        assert_eq!(m.finish_reason, Some(FinishReason::Cancelled));
    }

    #[test]
    fn add_message_to_missing_conversation_is_not_found() {
        let c = open_in_memory(&[]).unwrap();
        let err = add_message(&c, Id::new(), NewMessage::user_text("x")).unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));
    }

    #[test]
    fn delete_restore_and_purge() {
        let c = open_in_memory(&[]).unwrap();
        let conv = create_conversation(&c, "t").unwrap();
        add_message(&c, conv.id, NewMessage::user_text("keep")).unwrap();
        delete_conversation(&c, conv.id).unwrap();
        assert!(list_conversations(&c).unwrap().is_empty());
        assert!(matches!(
            delete_conversation(&c, conv.id),
            Err(StoreError::NotFound(_))
        ));
        restore_conversation(&c, conv.id).unwrap();
        assert_eq!(list_messages(&c, conv.id).unwrap()[0].text(), "keep");

        delete_conversation(&c, conv.id).unwrap();
        assert_eq!(purge_deleted(&c, now_ms()).unwrap(), 1);
        assert!(list_messages(&c, conv.id).unwrap().is_empty());
        assert!(restore_conversation(&c, conv.id).is_err());
        assert_eq!(purge_deleted(&c, now_ms()).unwrap(), 0);
    }

    #[test]
    fn tool_call_id_round_trips() {
        let c = open_in_memory(&[]).unwrap();
        let conv = create_conversation(&c, "t").unwrap();
        add_message(&c, conv.id, NewMessage::tool_result("call_1", "42")).unwrap();
        let m = &list_messages(&c, conv.id).unwrap()[0];
        assert_eq!(m.tool_call_id.as_deref(), Some("call_1"));
        let cm = m.to_chat_message();
        assert_eq!(cm.role, Role::Tool);
        assert_eq!(cm.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn truncate_rejects_message_from_another_conversation() {
        let c = open_in_memory(&[]).unwrap();
        let a = create_conversation(&c, "a").unwrap();
        let b = create_conversation(&c, "b").unwrap();
        add_message(&c, a.id, NewMessage::user_text("a1")).unwrap();
        let b1 = add_message(&c, b.id, NewMessage::user_text("b1")).unwrap();
        add_message(&c, a.id, NewMessage::user_text("a2")).unwrap();
        assert!(matches!(
            truncate_from(&c, a.id, b1.id),
            Err(StoreError::NotFound(_))
        ));
        assert_eq!(list_messages(&c, a.id).unwrap().len(), 2);
        assert_eq!(list_messages(&c, b.id).unwrap().len(), 1);
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let c = open_in_memory(&[]).unwrap();
        let err = c.execute(
            "INSERT INTO store_messages (id, conversation_id, role, content, status, created_at, updated_at)
             VALUES ('x', 'missing', 'user', '[]', 'complete', 0, 0)",
            [],
        );
        assert!(err.is_err());
    }

    /// Removes the directory even when the test fails.
    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn history_persists_across_reopen() {
        let dir = TempDir(std::env::temp_dir().join(format!("dn-store-test-{}", Id::new())));
        let path = dir.0.join("db.sqlite3");
        let id = {
            let c = crate::open(&path, &[]).unwrap();
            let conv = create_conversation(&c, "kept").unwrap();
            add_message(&c, conv.id, NewMessage::user_text("remember me")).unwrap();
            conv.id
        };
        let c = crate::open(&path, &[]).unwrap();
        assert_eq!(get_conversation(&c, id).unwrap().title, "kept");
        assert_eq!(list_messages(&c, id).unwrap()[0].text(), "remember me");
    }

    #[test]
    fn composes_other_crates_migrations() {
        const OTHER: &[&str] = &["CREATE TABLE artifact_things (id TEXT PRIMARY KEY)"];
        let c = open_in_memory(&[crate::MigrationSet::new("artifact", OTHER)]).unwrap();
        assert_eq!(crate::migrate::applied_version(&c, "artifact").unwrap(), 1);
        assert_eq!(crate::migrate::applied_version(&c, "store").unwrap(), 2);
    }
}
