//! Records, adapters, scan-then-apply, conflicts and tombstones
//! (docs/design/dn-sync.md §3, §5, §6).
//!
//! **Adapters:** a [`Collection`] adapter reads and writes one kind of record through its
//! owning crate's own functions (chats, messages and projects through `dn_store::sync_rows`).
//! This module never touches another crate's tables.
//!
//! **Local edits ([`scan`]):**
//! - Scanning reads dn-store's change log and compares each field with the hash recorded
//!   here (`sync_fields`).
//! - What differs becomes one change. Numbering it, sealing it into the outbox and
//!   recording its fields happen in one transaction.
//!
//! **Incoming changes ([`apply`]):** merged field by field.
//! - A write that **follows** the current one (its `deps` include it) replaces it.
//! - A **concurrent** write, for a scalar: the higher (HLC, author) wins.
//! - A concurrent write, for **text**: the higher (HLC, author) is shown and the other is
//!   kept in `sync_conflicts` until a later write that includes both settles it.
//! - **Messages are append-only.** A copy that is already here is left alone.
//! - **A purge wins:** later or concurrent writes to a purged record, and children of a
//!   purged parent, are dropped. When that drops an edit this device made, a notice says
//!   so (`sync_notices`).
//!
//! **The rule that makes this exact:** scan, then apply ([`pump`]). The caller holds the
//! database connection for the whole pump, so no local write lands between the scan and
//! the apply. A local edit is therefore always recorded as made before the incoming
//! changes it never saw, and is never silently overwritten (§3.6).

use crate::change::{Change, GroupId, Op};
use crate::envelope::{GroupKey, Opened, change_hash, seal};
use crate::hlc::Hlc;
use crate::identity::{DeviceId, DeviceKeys};
use crate::inbox;
use crate::{Result, SyncError};
use dn_store::sync_rows::{self, ChatRow, Kind, MessageRow, ProjectRow};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub(crate) const MIGRATION: &str = "
-- The write behind each synced field's current value: the hash of the value, and the change
-- that wrote it. A field whose value differs from its hash was edited here.
CREATE TABLE sync_fields (
    collection TEXT    NOT NULL,
    id         TEXT    NOT NULL,
    field      TEXT    NOT NULL,
    hash       BLOB    NOT NULL,
    author     TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
    hlc        INTEGER NOT NULL,
    PRIMARY KEY (collection, id, field)
) WITHOUT ROWID;

-- Text written on two devices at once: the versions not shown, until someone picks one.
CREATE TABLE sync_conflicts (
    collection TEXT    NOT NULL,
    id         TEXT    NOT NULL,
    field      TEXT    NOT NULL,
    value      TEXT    NOT NULL,
    author     TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
    hlc        INTEGER NOT NULL,
    PRIMARY KEY (collection, id, field, author, seq)
) WITHOUT ROWID;

-- Records deleted for good, by whom (tombstones are kept for good, §5).
CREATE TABLE sync_purged (
    collection TEXT    NOT NULL,
    id         TEXT    NOT NULL,
    author     TEXT    NOT NULL,
    seq        INTEGER NOT NULL,
    hlc        INTEGER NOT NULL,
    PRIMARY KEY (collection, id)
) WITHOUT ROWID;

-- How far dn-store's change log has been scanned.
CREATE TABLE sync_scan (
    id  INTEGER PRIMARY KEY CHECK (id = 1),
    rev INTEGER NOT NULL
);

-- This device's own envelopes, in order, to send (the log, §9.2).
CREATE TABLE sync_outbox (
    seq      INTEGER PRIMARY KEY NOT NULL,
    envelope BLOB    NOT NULL
);

-- Edits made here that a delete from another device dropped (§5).
CREATE TABLE sync_notices (
    n          INTEGER PRIMARY KEY AUTOINCREMENT,
    at         INTEGER NOT NULL,
    collection TEXT    NOT NULL,
    id         TEXT    NOT NULL,
    text       TEXT    NOT NULL
);
";

/// How a field merges (§6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// Last write wins.
    Scalar,
    /// Concurrent writes keep both versions.
    Text,
}

/// The kind of records a collection holds (§3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionKind {
    Fields(&'static [(&'static str, FieldKind)]),
    /// Written once, never changed.
    Append,
}

/// One kind of synced record, read and written through its owning crate.
pub trait Collection {
    /// The name in changes (`chat`, `message`, `project`).
    fn name(&self) -> &'static str;
    /// The table it reads in dn-store's change log.
    fn log_kind(&self) -> Kind;
    fn kind(&self) -> CollectionKind;
    /// The record's fields as they are now, or `None` when it isn't here.
    fn read(&self, conn: &Connection, id: &str) -> Result<Option<Map<String, Value>>>;
    /// Writes fields from another device: all of them for a new record, the changed ones
    /// otherwise.
    fn write(&self, conn: &Connection, id: &str, fields: &Map<String, Value>) -> Result<()>;
    /// Deletes the record for good.
    fn purge(&self, conn: &Connection, id: &str, at_ms: i64) -> Result<()>;
    /// For a child record: its parent (collection, id), to drop children of a purged parent.
    fn parent(&self, _fields: &Map<String, Value>) -> Option<(&'static str, String)> {
        None
    }
    /// The children a purge of this record removes too, as (collection, id).
    fn children(&self, _conn: &Connection, _id: &str) -> Result<Vec<(&'static str, String)>> {
        Ok(Vec::new())
    }
}

fn store(e: dn_store::StoreError) -> SyncError {
    match e {
        dn_store::StoreError::Sqlite(e) => SyncError::Db(e),
        other => SyncError::Store(other.to_string()),
    }
}

fn to_map<T: serde::Serialize>(row: &T) -> Result<Map<String, Value>> {
    match serde_json::to_value(row) {
        Ok(Value::Object(mut m)) => {
            m.remove("id");
            Ok(m)
        }
        _ => Err(SyncError::Protocol("record isn't an object")),
    }
}

fn merged<T: serde::Serialize + serde::de::DeserializeOwned>(
    current: Option<T>,
    id: &str,
    fields: &Map<String, Value>,
) -> Result<T> {
    let mut m = match current {
        Some(c) => to_map(&c)?,
        None => Map::new(),
    };
    for (k, v) in fields {
        m.insert(k.clone(), v.clone());
    }
    m.insert("id".into(), Value::String(id.to_owned()));
    serde_json::from_value(Value::Object(m))
        .map_err(|_| SyncError::Protocol("a record is missing fields"))
}

/// Chats (`store_conversations`).
pub struct Chats;
/// Finished messages (`store_messages`).
pub struct Messages;
/// Projects (`store_projects`).
pub struct Projects;

const CHAT_FIELDS: &[(&str, FieldKind)] = &[
    ("title", FieldKind::Text),
    ("model", FieldKind::Scalar),
    ("project_id", FieldKind::Scalar),
    ("deleted_at", FieldKind::Scalar),
    ("created_at", FieldKind::Scalar),
    ("updated_at", FieldKind::Scalar),
];

const PROJECT_FIELDS: &[(&str, FieldKind)] = &[
    ("name", FieldKind::Text),
    ("instructions", FieldKind::Text),
    ("deleted_at", FieldKind::Scalar),
    ("created_at", FieldKind::Scalar),
    ("updated_at", FieldKind::Scalar),
];

impl Collection for Chats {
    fn name(&self) -> &'static str {
        "chat"
    }
    fn log_kind(&self) -> Kind {
        Kind::Conversation
    }
    fn kind(&self) -> CollectionKind {
        CollectionKind::Fields(CHAT_FIELDS)
    }
    fn read(&self, conn: &Connection, id: &str) -> Result<Option<Map<String, Value>>> {
        sync_rows::chat(conn, id)
            .map_err(store)?
            .as_ref()
            .map(to_map)
            .transpose()
    }
    fn write(&self, conn: &Connection, id: &str, fields: &Map<String, Value>) -> Result<()> {
        let row: ChatRow = merged(sync_rows::chat(conn, id).map_err(store)?, id, fields)?;
        sync_rows::put_chat(conn, &row).map_err(store)
    }
    fn purge(&self, conn: &Connection, id: &str, at_ms: i64) -> Result<()> {
        sync_rows::purge_chat(conn, id, at_ms).map_err(store)
    }
    fn children(&self, conn: &Connection, id: &str) -> Result<Vec<(&'static str, String)>> {
        Ok(sync_rows::chat_message_ids(conn, id)
            .map_err(store)?
            .into_iter()
            .map(|m| ("message", m))
            .collect())
    }
}

impl Collection for Projects {
    fn name(&self) -> &'static str {
        "project"
    }
    fn log_kind(&self) -> Kind {
        Kind::Project
    }
    fn kind(&self) -> CollectionKind {
        CollectionKind::Fields(PROJECT_FIELDS)
    }
    fn read(&self, conn: &Connection, id: &str) -> Result<Option<Map<String, Value>>> {
        sync_rows::project(conn, id)
            .map_err(store)?
            .as_ref()
            .map(to_map)
            .transpose()
    }
    fn write(&self, conn: &Connection, id: &str, fields: &Map<String, Value>) -> Result<()> {
        let row: ProjectRow = merged(sync_rows::project(conn, id).map_err(store)?, id, fields)?;
        sync_rows::put_project(conn, &row).map_err(store)
    }
    fn purge(&self, conn: &Connection, id: &str, at_ms: i64) -> Result<()> {
        sync_rows::purge_project(conn, id, at_ms).map_err(store)
    }
}

impl Collection for Messages {
    fn name(&self) -> &'static str {
        "message"
    }
    fn log_kind(&self) -> Kind {
        Kind::Message
    }
    fn kind(&self) -> CollectionKind {
        CollectionKind::Append
    }
    fn read(&self, conn: &Connection, id: &str) -> Result<Option<Map<String, Value>>> {
        sync_rows::message(conn, id)
            .map_err(store)?
            .as_ref()
            .map(to_map)
            .transpose()
    }
    fn write(&self, conn: &Connection, id: &str, fields: &Map<String, Value>) -> Result<()> {
        let row: MessageRow = merged(None, id, fields)?;
        sync_rows::insert_message(conn, &row)
            .map_err(store)
            .map(|_| ())
    }
    fn purge(&self, conn: &Connection, id: &str, _at_ms: i64) -> Result<()> {
        sync_rows::delete_message(conn, id).map_err(store)
    }
    fn parent(&self, fields: &Map<String, Value>) -> Option<(&'static str, String)> {
        fields
            .get("conversation_id")
            .and_then(Value::as_str)
            .map(|c| ("chat", c.to_owned()))
    }
}

/// The collections this step syncs.
pub fn store_collections() -> [&'static dyn Collection; 3] {
    [&Projects, &Chats, &Messages]
}

fn value_hash(v: &Value) -> Vec<u8> {
    change_hash(&serde_json::to_vec(v).unwrap_or_default()).to_vec()
}

struct FieldWrite {
    hash: Vec<u8>,
    author: DeviceId,
    seq: u64,
    hlc: Hlc,
}

fn field_write(conn: &Connection, col: &str, id: &str, field: &str) -> Result<Option<FieldWrite>> {
    let row = conn
        .query_row(
            "SELECT hash, author, seq, hlc FROM sync_fields
             WHERE collection = ?1 AND id = ?2 AND field = ?3",
            params![col, id, field],
            |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((hash, author, seq, hlc)) = row else {
        return Ok(None);
    };
    Ok(Some(FieldWrite {
        hash,
        author: DeviceId::from_hex(&author).ok_or(SyncError::Protocol("damaged sync_fields"))?,
        seq: seq as u64,
        hlc: Hlc(hlc as u64),
    }))
}

#[allow(clippy::too_many_arguments)]
fn record_field(
    conn: &Connection,
    col: &str,
    id: &str,
    field: &str,
    value: &Value,
    author: DeviceId,
    seq: u64,
    hlc: Hlc,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_fields (collection, id, field, hash, author, seq, hlc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (collection, id, field) DO UPDATE SET
            hash = excluded.hash, author = excluded.author, seq = excluded.seq, hlc = excluded.hlc",
        params![
            col,
            id,
            field,
            value_hash(value),
            author.to_hex(),
            seq as i64,
            hlc.0 as i64
        ],
    )?;
    Ok(())
}

fn is_purged(conn: &Connection, col: &str, id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sync_purged WHERE collection = ?1 AND id = ?2",
            params![col, id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn record_purge(
    conn: &Connection,
    col: &str,
    id: &str,
    author: DeviceId,
    seq: u64,
    hlc: Hlc,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO sync_purged (collection, id, author, seq, hlc) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![col, id, author.to_hex(), seq as i64, hlc.0 as i64],
    )?;
    conn.execute(
        "DELETE FROM sync_fields WHERE collection = ?1 AND id = ?2",
        params![col, id],
    )?;
    conn.execute(
        "DELETE FROM sync_conflicts WHERE collection = ?1 AND id = ?2",
        params![col, id],
    )?;
    Ok(())
}

/// Conflicts that `change` settles: versions it had seen when it was written.
fn settle_conflicts(
    conn: &Connection,
    col: &str,
    id: &str,
    field: &str,
    change: &Change,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT author, seq FROM sync_conflicts WHERE collection = ?1 AND id = ?2 AND field = ?3",
    )?;
    let rows: Vec<(String, i64)> = stmt
        .query_map(params![col, id, field], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (a, s) in rows {
        let author = DeviceId::from_hex(&a).ok_or(SyncError::Protocol("damaged sync_conflicts"))?;
        let seen = change.deps.includes(author, s as u64)
            || (author == change.author && change.seq > s as u64);
        if seen {
            conn.execute(
                "DELETE FROM sync_conflicts WHERE collection = ?1 AND id = ?2 AND field = ?3
                 AND author = ?4 AND seq = ?5",
                params![col, id, field, a, s],
            )?;
        }
    }
    Ok(())
}

fn collection<'a>(cols: &'a [&'a dyn Collection], name: &str) -> Result<&'a dyn Collection> {
    cols.iter()
        .copied()
        .find(|c| c.name() == name)
        .ok_or(SyncError::Protocol(
            "a change for a collection this app doesn't sync",
        ))
}

fn scan_rev(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row("SELECT rev FROM sync_scan WHERE id = 1", [], |r| r.get(0))
        .optional()?
        .unwrap_or(0))
}

/// Turns local edits into one change, sealed into the outbox (see the module notes).
/// Returns `None` when nothing changed here.
pub fn scan(
    conn: &Connection,
    cols: &[&dyn Collection],
    me: &DeviceKeys,
    group: GroupId,
    key: &GroupKey,
    now_ms: u64,
) -> Result<Option<Change>> {
    let tx = conn.unchecked_transaction()?;
    let since = scan_rev(&tx)?;
    let mut entries = sync_rows::log_since(&tx, since).map_err(store)?;
    let Some(last_rev) = entries.iter().map(|e| e.rev).max() else {
        return Ok(None);
    };
    // Parents before children inside one change (a chat's latest entry can come after its
    // newest message, since a message bumps the chat): in the order of `cols`, then of the log.
    let rank = |k: Kind| {
        cols.iter()
            .position(|c| c.log_kind() == k)
            .unwrap_or(usize::MAX)
    };
    entries.sort_by_key(|e| (rank(e.kind), e.rev));
    let mut ops = Vec::new();
    // What to record once the change has its number: (collection, id, field, value).
    let mut fields: Vec<(&'static str, String, String, Value)> = Vec::new();
    let mut purges: Vec<(&'static str, String)> = Vec::new();
    for e in &entries {
        let Some(col) = cols.iter().copied().find(|c| c.log_kind() == e.kind) else {
            continue;
        };
        let name = col.name();
        if is_purged(&tx, name, &e.id)? {
            continue; // Our own apply of a purge, or a record already deleted for good.
        }
        if e.deleted {
            ops.push(Op::Purge {
                collection: name.to_owned(),
                id: e.id.clone(),
            });
            purges.push((name, e.id.clone()));
            continue;
        }
        let Some(now) = col.read(&tx, &e.id)? else {
            continue;
        };
        match col.kind() {
            CollectionKind::Append => {
                if field_write(&tx, name, &e.id, "")?.is_some() {
                    continue; // Already sent, or received from another device.
                }
                ops.push(Op::Append {
                    collection: name.to_owned(),
                    id: e.id.clone(),
                    value: Value::Object(now),
                });
                fields.push((name, e.id.clone(), String::new(), Value::Null));
            }
            CollectionKind::Fields(spec) => {
                let mut changed = BTreeMap::new();
                for (field, _) in spec {
                    let v = now.get(*field).cloned().unwrap_or(Value::Null);
                    let same = field_write(&tx, name, &e.id, field)?
                        .is_some_and(|w| w.hash == value_hash(&v));
                    if !same {
                        changed.insert((*field).to_owned(), v.clone());
                        fields.push((name, e.id.clone(), (*field).to_owned(), v));
                    }
                }
                if !changed.is_empty() {
                    ops.push(Op::Put {
                        collection: name.to_owned(),
                        id: e.id.clone(),
                        fields: changed,
                    });
                }
            }
        }
    }
    let mut out = None;
    if !ops.is_empty() {
        let change = inbox::next_change_in(&tx, me.id(), group, key.epoch, ops, now_ms)?;
        let envelope = seal(&change, me, key)?;
        tx.execute(
            "INSERT INTO sync_outbox (seq, envelope) VALUES (?1, ?2)",
            params![change.seq as i64, envelope],
        )?;
        for (col, id, field, value) in &fields {
            record_field(
                &tx,
                col,
                id,
                field,
                value,
                change.author,
                change.seq,
                change.hlc,
            )?;
            if !field.is_empty() {
                // An edit made after seeing the other versions settles them.
                settle_conflicts(&tx, col, id, field, &change)?;
            }
        }
        for (col, id) in &purges {
            record_purge(&tx, col, id, change.author, change.seq, change.hlc)?;
        }
        out = Some(change);
    }
    tx.execute(
        "INSERT INTO sync_scan (id, rev) VALUES (1, ?1) ON CONFLICT (id) DO UPDATE SET rev = excluded.rev",
        [last_rev],
    )?;
    sync_rows::prune_log(&tx, last_rev).map_err(store)?;
    tx.commit()?;
    Ok(out)
}

fn notice(conn: &Connection, col: &str, id: &str, text: &str, now_ms: u64) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_notices (at, collection, id, text) VALUES (?1, ?2, ?3, ?4)",
        params![now_ms as i64, col, id, text],
    )?;
    Ok(())
}

/// Merges an incoming change into the local tables and marks it applied (one transaction).
/// `me` is this device, to tell when a purge drops one of its own edits.
pub fn apply(
    conn: &Connection,
    cols: &[&dyn Collection],
    change: &Change,
    me: DeviceId,
    now_ms: u64,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for op in &change.ops {
        match op {
            Op::Purge { collection: c, id } => {
                let col = collection(cols, c)?;
                if is_purged(&tx, col.name(), id)? {
                    continue;
                }
                // An edit made here that the deleting device hadn't seen is dropped: say so.
                let mut stmt = tx.prepare(
                    "SELECT author, seq FROM sync_fields WHERE collection = ?1 AND id = ?2",
                )?;
                let writes: Vec<(String, i64)> = stmt
                    .query_map(params![col.name(), id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                drop(stmt);
                let dropped = writes
                    .iter()
                    .any(|(a, s)| a == &me.to_hex() && !change.deps.includes(me, *s as u64));
                if dropped {
                    notice(
                        &tx,
                        col.name(),
                        id,
                        "It was deleted on another device while this device was changing it.",
                        now_ms,
                    )?;
                }
                // Its children go with it, by the same change (not sent on as deletes of
                // their own).
                for (cc, cid) in col.children(&tx, id)? {
                    record_purge(&tx, cc, &cid, change.author, change.seq, change.hlc)?;
                }
                col.purge(&tx, id, change.hlc.millis() as i64)?;
                record_purge(&tx, col.name(), id, change.author, change.seq, change.hlc)?;
            }
            Op::Append {
                collection: c,
                id,
                value,
            } => {
                let col = collection(cols, c)?;
                let Value::Object(fields) = value else {
                    return Err(SyncError::Protocol("an appended record isn't an object"));
                };
                if is_purged(&tx, col.name(), id)?
                    || field_write(&tx, col.name(), id, "")?.is_some()
                {
                    continue;
                }
                if let Some((pc, pid)) = col.parent(fields)
                    && is_purged(&tx, pc, &pid)?
                {
                    continue; // Its parent was deleted for good: a delete wins.
                }
                col.write(&tx, id, fields)?;
                record_field(
                    &tx,
                    col.name(),
                    id,
                    "",
                    &Value::Null,
                    change.author,
                    change.seq,
                    change.hlc,
                )?;
            }
            Op::Put {
                collection: c,
                id,
                fields,
            } => {
                let col = collection(cols, c)?;
                let CollectionKind::Fields(spec) = col.kind() else {
                    return Err(SyncError::Protocol("a put on an append-only collection"));
                };
                if is_purged(&tx, col.name(), id)? {
                    continue;
                }
                let current = col.read(&tx, id)?;
                let mut winners = Map::new();
                for (field, value) in fields {
                    let kind = spec
                        .iter()
                        .find(|(f, _)| f == field)
                        .map(|(_, k)| *k)
                        .ok_or(SyncError::Protocol("a field this app doesn't sync"))?;
                    let follows = |w: &FieldWrite| {
                        change.deps.includes(w.author, w.seq)
                            || (w.author == change.author && change.seq > w.seq)
                    };
                    // Versions this change had seen are settled, whoever ends up shown, so
                    // every device keeps the same set whatever order changes arrive in.
                    settle_conflicts(&tx, col.name(), id, field, change)?;
                    match field_write(&tx, col.name(), id, field)? {
                        None => {
                            winners.insert(field.clone(), value.clone());
                        }
                        Some(w) if follows(&w) => {
                            winners.insert(field.clone(), value.clone());
                        }
                        Some(w) => {
                            // Concurrent.
                            let incoming_wins = (change.hlc, change.author) > (w.hlc, w.author);
                            if kind == FieldKind::Text {
                                let (keep_value, keep_author, keep_seq, keep_hlc) = if incoming_wins
                                {
                                    let shown = current
                                        .as_ref()
                                        .and_then(|m| m.get(field))
                                        .cloned()
                                        .unwrap_or(Value::Null);
                                    (shown, w.author, w.seq, w.hlc)
                                } else {
                                    (value.clone(), change.author, change.seq, change.hlc)
                                };
                                tx.execute(
                                    "INSERT OR IGNORE INTO sync_conflicts
                                        (collection, id, field, value, author, seq, hlc)
                                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                                    params![
                                        col.name(),
                                        id,
                                        field,
                                        keep_value.to_string(),
                                        keep_author.to_hex(),
                                        keep_seq as i64,
                                        keep_hlc.0 as i64
                                    ],
                                )?;
                            }
                            if incoming_wins {
                                winners.insert(field.clone(), value.clone());
                            }
                        }
                    }
                }
                if !winners.is_empty() {
                    col.write(&tx, id, &winners)?;
                    for (field, value) in &winners {
                        record_field(
                            &tx,
                            col.name(),
                            id,
                            field,
                            value,
                            change.author,
                            change.seq,
                            change.hlc,
                        )?;
                    }
                }
            }
        }
    }
    inbox::applied_in(&tx, change, now_ms)?;
    tx.commit()?;
    Ok(())
}

/// One round: scan local edits first, then apply every change that is ready. The caller
/// holds the connection for the whole round (see the module notes). `open` opens an
/// envelope from the inbox (with the right epoch's key and the membership chain).
pub fn pump(
    conn: &Connection,
    cols: &[&dyn Collection],
    me: &DeviceKeys,
    group: GroupId,
    key: &GroupKey,
    open: impl Fn(&[u8]) -> Result<Opened>,
    now_ms: u64,
) -> Result<(Option<Change>, usize)> {
    let local = scan(conn, cols, me, group, key, now_ms)?;
    let mut applied = 0;
    while let Some((author, seq, envelope)) = inbox::next_ready(conn, now_ms)? {
        let opened = open(&envelope)?;
        if opened.change.author != author || opened.change.seq != seq {
            return Err(SyncError::Protocol(
                "an inbox envelope isn't the change it was stored as",
            ));
        }
        apply(conn, cols, &opened.change, me.id(), now_ms)?;
        applied += 1;
    }
    Ok((local, applied))
}

/// A text field written on two devices at once (§6.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub field: String,
    /// The version not shown, as JSON.
    pub other: Value,
    pub author: DeviceId,
}

pub fn conflicts(conn: &Connection, collection: &str, id: &str) -> Result<Vec<Conflict>> {
    let mut stmt = conn.prepare(
        "SELECT field, value, author FROM sync_conflicts WHERE collection = ?1 AND id = ?2
         ORDER BY field, hlc, author",
    )?;
    let rows = stmt.query_map(params![collection, id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (field, value, author) = row?;
        out.push(Conflict {
            field,
            other: serde_json::from_str(&value)
                .map_err(|_| SyncError::Protocol("damaged conflict"))?,
            author: DeviceId::from_hex(&author).ok_or(SyncError::Protocol("damaged conflict"))?,
        });
    }
    Ok(out)
}

/// Notices about edits a delete dropped, oldest first: (collection, id, text).
pub fn notices(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare("SELECT collection, id, text FROM sync_notices ORDER BY n")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// This device's envelopes from `after_seq` on, to send.
pub fn outbox(conn: &Connection, after_seq: u64) -> Result<Vec<(u64, Vec<u8>)>> {
    let mut stmt =
        conn.prepare("SELECT seq, envelope FROM sync_outbox WHERE seq > ?1 ORDER BY seq")?;
    let rows = stmt.query_map([after_seq as i64], |r| {
        Ok((r.get::<_, i64>(0)? as u64, r.get(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::open as open_envelope;
    use crate::identity::PublicIdentity;
    use dn_store::MigrationSet;
    use dn_store::chat::{self, NewMessage};
    use dn_store::project;

    const GROUP: GroupId = GroupId([9; 16]);
    const NOW: u64 = 1_800_000_000_000;

    struct Dev {
        conn: Connection,
        keys: DeviceKeys,
        sent: u64,
    }

    impl Dev {
        fn new() -> Self {
            let conn = dn_store::open_in_memory(&[MigrationSet::new("sync", crate::migrations())])
                .unwrap();
            dn_store::sync_rows::enable(&conn).unwrap();
            Self {
                conn,
                keys: DeviceKeys::generate().unwrap(),
                sent: 0,
            }
        }

        /// Scans local edits and returns the envelopes not sent yet.
        fn send(&mut self, key: &GroupKey, now: u64) -> Vec<Vec<u8>> {
            scan(
                &self.conn,
                &store_collections(),
                &self.keys,
                GROUP,
                key,
                now,
            )
            .unwrap();
            let out = outbox(&self.conn, self.sent).unwrap();
            if let Some((s, _)) = out.last() {
                self.sent = *s;
            }
            out.into_iter().map(|(_, e)| e).collect()
        }

        /// Receives envelopes, then pumps (scan first, then apply).
        fn take(&mut self, envs: &[Vec<u8>], key: &GroupKey, members: &[PublicIdentity], now: u64) {
            let lookup = |a: DeviceId, _: u32| {
                members
                    .iter()
                    .find(|m| m.id() == a)
                    .copied()
                    .ok_or(SyncError::UnknownDevice)
            };
            for e in envs {
                let o = open_envelope(e, key, lookup).unwrap();
                inbox::receive(&self.conn, &o, now).unwrap();
            }
            pump(
                &self.conn,
                &store_collections(),
                &self.keys,
                GROUP,
                key,
                |e| open_envelope(e, key, lookup),
                now,
            )
            .unwrap();
        }

        fn chats(&self) -> Vec<(String, String, Option<String>)> {
            let mut v: Vec<_> = chat::list_conversations(&self.conn)
                .unwrap()
                .into_iter()
                .map(|c| (c.id.to_string(), c.title, c.model))
                .collect();
            v.sort();
            v
        }

        fn messages(&self, chat_id: dn_core::Id) -> Vec<String> {
            chat::list_messages(&self.conn, chat_id)
                .unwrap_or_default()
                .into_iter()
                .map(|m| m.id.to_string())
                .collect()
        }
    }

    fn pair() -> (Dev, Dev, GroupKey, Vec<PublicIdentity>) {
        let a = Dev::new();
        let b = Dev::new();
        let members = vec![a.keys.public(), b.keys.public()];
        (a, b, GroupKey::generate(0).unwrap(), members)
    }

    #[test]
    fn what_a_device_already_has_reaches_the_other_and_nothing_echoes_back() {
        let (mut a, mut b, key, members) = pair();
        let p = project::create_project(&a.conn, "Trip").unwrap();
        let c = chat::create_conversation_in(&a.conn, "Plans", Some(p.id)).unwrap();
        chat::add_message(&a.conn, c.id, NewMessage::user_text("Book a hotel")).unwrap();
        let envs = a.send(&key, NOW);
        assert_eq!(envs.len(), 1, "one change for everything found");
        b.take(&envs, &key, &members, NOW);
        assert_eq!(b.chats(), a.chats());
        assert_eq!(b.messages(c.id), a.messages(c.id));
        assert_eq!(project::get_project(&b.conn, p.id).unwrap().name, "Trip");
        // B's own scan finds nothing new: applied writes aren't sent back.
        assert!(b.send(&key, NOW).is_empty());
        assert!(a.send(&key, NOW).is_empty());
    }

    #[test]
    fn concurrent_title_edits_keep_both_and_a_later_edit_settles_them() {
        let (mut a, mut b, key, members) = pair();
        let c = chat::create_conversation(&a.conn, "Plans").unwrap();
        let e = a.send(&key, NOW);
        b.take(&e, &key, &members, NOW);
        chat::rename_conversation(&a.conn, c.id, "Plans (A)").unwrap();
        chat::rename_conversation(&b.conn, c.id, "Plans (B)").unwrap();
        let ea = a.send(&key, NOW + 1);
        let eb = b.send(&key, NOW + 2);
        a.take(&eb, &key, &members, NOW + 3);
        b.take(&ea, &key, &members, NOW + 3);
        // Both show the same title (the later write), and both keep the other version.
        assert_eq!(a.chats(), b.chats());
        assert_eq!(a.chats()[0].1, "Plans (B)");
        let ca = conflicts(&a.conn, "chat", &c.id.to_string()).unwrap();
        let cb = conflicts(&b.conn, "chat", &c.id.to_string()).unwrap();
        assert_eq!(ca.len(), 1);
        assert_eq!(ca[0].other, Value::String("Plans (A)".into()));
        assert_eq!(cb[0].other, ca[0].other);
        // A picks a title having seen both: the conflict ends on both devices.
        chat::rename_conversation(&a.conn, c.id, "Plans (both)").unwrap();
        let e = a.send(&key, NOW + 4);
        assert!(
            conflicts(&a.conn, "chat", &c.id.to_string())
                .unwrap()
                .is_empty()
        );
        b.take(&e, &key, &members, NOW + 5);
        assert!(
            conflicts(&b.conn, "chat", &c.id.to_string())
                .unwrap()
                .is_empty()
        );
        assert_eq!(b.chats()[0].1, "Plans (both)");
    }

    #[test]
    fn concurrent_scalars_take_the_later_write_without_a_conflict() {
        let (mut a, mut b, key, members) = pair();
        let c = chat::create_conversation(&a.conn, "Plans").unwrap();
        let e = a.send(&key, NOW);
        b.take(&e, &key, &members, NOW);
        chat::set_conversation_model(&a.conn, c.id, "local:a").unwrap();
        chat::set_conversation_model(&b.conn, c.id, "local:b").unwrap();
        let ea = a.send(&key, NOW + 10);
        let eb = b.send(&key, NOW + 20);
        a.take(&eb, &key, &members, NOW + 30);
        b.take(&ea, &key, &members, NOW + 30);
        assert_eq!(a.chats(), b.chats());
        assert_eq!(a.chats()[0].2.as_deref(), Some("local:b"));
        assert!(
            conflicts(&a.conn, "chat", &c.id.to_string())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_delete_wins_over_a_concurrent_edit_and_the_editor_is_told() {
        let (mut a, mut b, key, members) = pair();
        let c = chat::create_conversation(&a.conn, "Weekly plan").unwrap();
        let e = a.send(&key, NOW);
        b.take(&e, &key, &members, NOW);
        // A deletes it for good while B renames it and adds a message.
        chat::delete_conversation(&a.conn, c.id).unwrap();
        chat::purge_deleted(&a.conn, i64::MAX).unwrap();
        chat::rename_conversation(&b.conn, c.id, "Weekly plan v2").unwrap();
        chat::add_message(&b.conn, c.id, NewMessage::user_text("one more")).unwrap();
        let ea = a.send(&key, NOW + 1);
        let eb = b.send(&key, NOW + 2);
        a.take(&eb, &key, &members, NOW + 3);
        b.take(&ea, &key, &members, NOW + 3);
        assert!(a.chats().is_empty());
        assert!(b.chats().is_empty());
        assert!(b.messages(c.id).is_empty());
        let n = notices(&b.conn).unwrap();
        assert_eq!(n.len(), 1);
        assert!(n[0].2.contains("deleted on another device"));
        assert!(notices(&a.conn).unwrap().is_empty());
        // Nothing comes back to life on either side.
        assert!(a.send(&key, NOW + 4).is_empty());
        assert!(b.send(&key, NOW + 4).is_empty());
    }

    #[test]
    fn a_local_edit_made_before_a_change_arrives_is_never_silently_overwritten() {
        let (mut a, mut b, key, members) = pair();
        let c = chat::create_conversation(&a.conn, "Plans").unwrap();
        let e = a.send(&key, NOW);
        b.take(&e, &key, &members, NOW);
        chat::rename_conversation(&a.conn, c.id, "From A").unwrap();
        let ea = a.send(&key, NOW + 1);
        // B edits but hasn't scanned yet when A's change arrives: pump scans first.
        chat::rename_conversation(&b.conn, c.id, "From B").unwrap();
        b.take(&ea, &key, &members, NOW + 2);
        let kept = conflicts(&b.conn, "chat", &c.id.to_string()).unwrap();
        assert_eq!(kept.len(), 1, "B's edit and A's are both kept");
    }

    #[test]
    fn a_regenerated_answer_is_removed_on_the_other_device() {
        let (mut a, mut b, key, members) = pair();
        let c = chat::create_conversation(&a.conn, "Q").unwrap();
        let m = chat::add_message(&a.conn, c.id, NewMessage::user_text("first")).unwrap();
        let e = a.send(&key, NOW);
        b.take(&e, &key, &members, NOW);
        assert_eq!(b.messages(c.id).len(), 1);
        chat::truncate_from(&a.conn, c.id, m.id).unwrap();
        let e = a.send(&key, NOW + 1);
        b.take(&e, &key, &members, NOW + 2);
        assert!(b.messages(c.id).is_empty());
    }

    #[test]
    fn delete_all_on_one_device_is_never_sent() {
        let (mut a, _b, key, _) = pair();
        chat::create_conversation(&a.conn, "Plans").unwrap();
        a.send(&key, NOW);
        chat::delete_all(&a.conn).unwrap();
        assert!(a.send(&key, NOW + 1).is_empty());
    }

    /// xorshift64*, for repeatable random orderings without a test dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    #[test]
    fn three_devices_editing_chats_converge_in_any_delivery_order() {
        for seed in 1..=25u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let key = GroupKey::generate(0).unwrap();
            let mut devs = [Dev::new(), Dev::new(), Dev::new()];
            let members: Vec<_> = devs.iter().map(|d| d.keys.public()).collect();
            // A shared chat to start from.
            let c = chat::create_conversation(&devs[0].conn, "Start").unwrap();
            let first = devs[0].send(&key, NOW);
            for d in devs.iter_mut().skip(1) {
                d.take(&first, &key, &members, NOW);
            }
            let mut sent: Vec<(usize, Vec<u8>)> = Vec::new();
            for step in 0..24u64 {
                let who = rng.below(3);
                let d = &devs[who];
                match rng.below(3) {
                    0 => chat::rename_conversation(&d.conn, c.id, &format!("t{step}")).unwrap(),
                    1 => chat::set_conversation_model(&d.conn, c.id, &format!("m{step}")).unwrap(),
                    _ => {
                        chat::add_message(&d.conn, c.id, NewMessage::user_text(format!("x{step}")))
                            .unwrap();
                    }
                }
                let envs = devs[who].send(&key, NOW + step);
                sent.extend(envs.into_iter().map(|e| (who, e)));
                if rng.below(2) == 0 {
                    let (from, e) = sent[rng.below(sent.len())].clone();
                    let to = rng.below(3);
                    if to != from {
                        devs[to].take(&[e], &key, &members, NOW + 100);
                    }
                }
            }
            for (r, d) in devs.iter_mut().enumerate() {
                let mut all: Vec<Vec<u8>> = sent
                    .iter()
                    .filter(|(f, _)| *f != r)
                    .map(|(_, e)| e.clone())
                    .collect();
                for i in (1..all.len()).rev() {
                    all.swap(i, rng.below(i + 1));
                }
                for e in all {
                    d.take(&[e], &key, &members, NOW + 1_000);
                }
            }
            // Edits made while taking (none here) would need another round; check equality.
            let base = devs[0].chats();
            for d in &devs[1..] {
                assert_eq!(d.chats(), base, "seed {seed}");
                let mut m1 = d.messages(c.id);
                let mut m0 = devs[0].messages(c.id);
                m1.sort();
                m0.sort();
                assert_eq!(m1, m0, "seed {seed}");
                assert_eq!(
                    conflicts(&d.conn, "chat", &c.id.to_string()).unwrap().len(),
                    conflicts(&devs[0].conn, "chat", &c.id.to_string())
                        .unwrap()
                        .len(),
                    "seed {seed}"
                );
            }
        }
    }
}
