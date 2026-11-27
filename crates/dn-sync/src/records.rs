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
pub trait Collection: Sync {
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
    /// Records this one refers to and that must reach the other side first (a message's
    /// chat, a chat's project).
    fn refers_to(&self, fields: &Map<String, Value>) -> Vec<(&'static str, String)> {
        self.parent(fields).into_iter().collect()
    }
    /// The children a purge of this record removes too, as (collection, id).
    fn children(&self, _conn: &Connection, _id: &str) -> Result<Vec<(&'static str, String)>> {
        Ok(Vec::new())
    }
    /// The hash kept in `sync_fields` for a field's value. A device compares it with the
    /// value it has, to find local edits. The Node never scans, and keys it so the hash
    /// can't confirm a guess ([`crate::at_rest`]).
    fn field_hash(&self, _id: &str, _field: &str, value: &Value) -> Result<Vec<u8>> {
        Ok(value_hash(value))
    }
    /// A conflict's version not shown (JSON), as `sync_conflicts` keeps it. The Node seals
    /// it ([`crate::at_rest`]).
    fn seal_conflict(&self, _at: &ConflictAt<'_>, json: String) -> Result<String> {
        Ok(json)
    }
    /// The JSON back from what [`Collection::seal_conflict`] kept.
    fn open_conflict(&self, _at: &ConflictAt<'_>, kept: String) -> Result<String> {
        Ok(kept)
    }
}

/// Where a kept conflict version belongs: its record, field and the change that wrote it.
#[derive(Clone, Copy, Debug)]
pub struct ConflictAt<'a> {
    pub id: &'a str,
    pub field: &'a str,
    pub author: DeviceId,
    pub seq: u64,
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
    fn refers_to(&self, fields: &Map<String, Value>) -> Vec<(&'static str, String)> {
        fields
            .get("project_id")
            .and_then(Value::as_str)
            .map(|p| ("project", p.to_owned()))
            .into_iter()
            .collect()
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
    col: &dyn Collection,
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
            col.name(),
            id,
            field,
            col.field_hash(id, field, value)?,
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

/// Most ops in one change, and their rough size: a big history goes out in many changes,
/// each a short transaction (§10.2: a change stays well under 1 MiB).
pub const SCAN_MAX_OPS: usize = 200;
pub const SCAN_MAX_BYTES: usize = 256 * 1024;

/// What one [`scan`] did.
#[derive(Debug, Default)]
pub struct Scanned {
    /// The change made from local edits, if any.
    pub change: Option<Change>,
    /// More local edits wait for the next scan.
    pub more: bool,
}

/// Collects one change's worth of local edits.
struct Batch {
    ops: Vec<Op>,
    bytes: usize,
    /// (collection, id, field, value) to record once the change has its number.
    fields: Vec<(&'static str, String, String, Value)>,
    purges: Vec<(&'static str, String)>,
    emitted: std::collections::HashSet<(&'static str, String)>,
}

impl Batch {
    fn full(&self) -> bool {
        self.ops.len() >= SCAN_MAX_OPS || self.bytes >= SCAN_MAX_BYTES
    }

    fn push(&mut self, op: Op) {
        self.bytes += serde_json::to_vec(&op).map(|v| v.len()).unwrap_or(0);
        self.ops.push(op);
    }
}

fn known(conn: &Connection, col: &str, id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sync_fields WHERE collection = ?1 AND id = ?2 LIMIT 1",
            params![col, id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Adds a record as it is now: its changed fields, or the whole record if appended. Its
/// parents go first when they haven't been sent yet, so a change never names a chat or a
/// project the other side doesn't have.
fn emit(
    tx: &Connection,
    cols: &[&dyn Collection],
    col: &dyn Collection,
    id: &str,
    now: Map<String, Value>,
    b: &mut Batch,
) -> Result<()> {
    let name = col.name();
    if !b.emitted.insert((name, id.to_owned())) {
        return Ok(());
    }
    for (pc, pid) in col.refers_to(&now) {
        let Ok(pcol) = collection(cols, pc) else {
            continue;
        };
        if is_purged(tx, pc, &pid)?
            || known(tx, pc, &pid)?
            || b.emitted.contains(&(pc, pid.clone()))
        {
            continue;
        }
        if let Some(pnow) = pcol.read(tx, &pid)? {
            emit(tx, cols, pcol, &pid, pnow, b)?;
        }
    }
    match col.kind() {
        CollectionKind::Append => {
            if field_write(tx, name, id, "")?.is_some() {
                return Ok(()); // Already sent, or received from another device.
            }
            b.push(Op::Append {
                collection: name.to_owned(),
                id: id.to_owned(),
                value: Value::Object(now),
            });
            b.fields
                .push((name, id.to_owned(), String::new(), Value::Null));
        }
        CollectionKind::Fields(spec) => {
            let mut changed = BTreeMap::new();
            for (field, _) in spec {
                let v = now.get(*field).cloned().unwrap_or(Value::Null);
                let same = match field_write(tx, name, id, field)? {
                    Some(w) => w.hash == col.field_hash(id, field, &v)?,
                    None => false,
                };
                if !same {
                    changed.insert((*field).to_owned(), v.clone());
                    b.fields.push((name, id.to_owned(), (*field).to_owned(), v));
                }
            }
            if !changed.is_empty() {
                b.push(Op::Put {
                    collection: name.to_owned(),
                    id: id.to_owned(),
                    fields: changed,
                });
            }
        }
    }
    Ok(())
}

/// Turns local edits into at most one change ([`SCAN_MAX_OPS`], [`SCAN_MAX_BYTES`]), sealed
/// into the outbox (see the module notes). `more` says another scan has more to do.
pub fn scan(
    conn: &Connection,
    cols: &[&dyn Collection],
    me: &DeviceKeys,
    group: GroupId,
    key: &GroupKey,
    now_ms: u64,
) -> Result<Scanned> {
    let tx = conn.unchecked_transaction()?;
    let since = scan_rev(&tx)?;
    let entries = sync_rows::log_since(&tx, since).map_err(store)?;
    if entries.is_empty() {
        return Ok(Scanned::default());
    }
    let mut b = Batch {
        ops: Vec::new(),
        bytes: 0,
        fields: Vec::new(),
        purges: Vec::new(),
        emitted: Default::default(),
    };
    let mut done_rev = since;
    let mut more = false;
    // In log order; parents are pulled forward by `emit`.
    for e in &entries {
        if b.full() {
            more = true;
            break;
        }
        done_rev = e.rev;
        let Some(col) = cols.iter().copied().find(|c| c.log_kind() == e.kind) else {
            continue;
        };
        let name = col.name();
        if is_purged(&tx, name, &e.id)? {
            continue; // Our own apply of a purge, or a record already deleted for good.
        }
        if e.deleted {
            b.push(Op::Purge {
                collection: name.to_owned(),
                id: e.id.clone(),
            });
            b.purges.push((name, e.id.clone()));
            continue;
        }
        if let Some(now) = col.read(&tx, &e.id)? {
            emit(&tx, cols, col, &e.id, now, &mut b)?;
        }
    }
    let mut out = None;
    if !b.ops.is_empty() {
        let change = inbox::next_change_in(&tx, me.id(), group, key.epoch, b.ops, now_ms)?;
        let envelope = seal(&change, me, key)?;
        tx.execute(
            "INSERT INTO sync_outbox (seq, envelope) VALUES (?1, ?2)",
            params![change.seq as i64, envelope],
        )?;
        tx.execute(
            "INSERT INTO sync_envelopes (author, seq, envelope) VALUES (?1, ?2, ?3)",
            params![change.author.to_hex(), change.seq as i64, envelope],
        )?;
        for (name, id, field, value) in &b.fields {
            let col = collection(cols, name)?;
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
                settle_conflicts(&tx, name, id, field, &change)?;
            }
        }
        for (col, id) in &b.purges {
            record_purge(&tx, col, id, change.author, change.seq, change.hlc)?;
        }
        out = Some(change);
    }
    tx.execute(
        "INSERT INTO sync_scan (id, rev) VALUES (1, ?1) ON CONFLICT (id) DO UPDATE SET rev = excluded.rev",
        [done_rev],
    )?;
    sync_rows::prune_log(&tx, done_rev).map_err(store)?;
    tx.commit()?;
    Ok(Scanned { change: out, more })
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
) -> Result<Vec<(String, String)>> {
    let tx = conn.unchecked_transaction()?;
    let mut purged = Vec::new();
    for op in &change.ops {
        // Ids from other devices must be in the one form this app writes (see
        // `dn_store::sync_rows::canonical_id`), so tombstones and keys always match.
        let (Op::Purge { id, .. } | Op::Append { id, .. } | Op::Put { id, .. }) = op;
        sync_rows::canonical_id(id).map_err(store)?;
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
                purged.push((col.name().to_owned(), id.clone()));
            }
            Op::Append {
                collection: c,
                id,
                value,
            } => {
                let col = collection(cols, c)?;
                if col.kind() != CollectionKind::Append {
                    return Err(SyncError::Protocol(
                        "an append on a collection that isn't append-only",
                    ));
                }
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
                    col,
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
                                let kept = col.seal_conflict(
                                    &ConflictAt {
                                        id,
                                        field,
                                        author: keep_author,
                                        seq: keep_seq,
                                    },
                                    keep_value.to_string(),
                                )?;
                                tx.execute(
                                    "INSERT OR IGNORE INTO sync_conflicts
                                        (collection, id, field, value, author, seq, hlc)
                                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                                    params![
                                        col.name(),
                                        id,
                                        field,
                                        kept,
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
                            col,
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
    Ok(purged)
}

/// What one [`pump_batch`] did.
#[derive(Debug, Default)]
pub struct PumpBatch {
    /// Changes made from local edits.
    pub local: Vec<Change>,
    pub applied: usize,
    /// Records other devices deleted for good, as (collection, id): the app forgets what it
    /// keeps about them elsewhere.
    pub purged: Vec<(String, String)>,
    /// Call again: more local edits or ready changes wait.
    pub more: bool,
}

/// One short step of scan-then-apply (see the module notes): while local edits wait, it
/// records one batch of them and returns; only when none wait does it apply up to
/// `max_apply` ready changes. The caller holds the connection for the call and may release
/// it between calls, so a big first sync never holds it long, and a local edit is always
/// recorded before anything newer is applied.
#[allow(clippy::too_many_arguments)]
pub fn pump_batch(
    conn: &Connection,
    cols: &[&dyn Collection],
    me: &DeviceKeys,
    group: GroupId,
    key: &GroupKey,
    open: impl Fn(&[u8]) -> Result<Opened>,
    max_apply: usize,
    now_ms: u64,
) -> Result<PumpBatch> {
    let mut out = PumpBatch::default();
    let s = scan(conn, cols, me, group, key, now_ms)?;
    out.local.extend(s.change);
    if s.more {
        out.more = true;
        return Ok(out);
    }
    while out.applied < max_apply {
        let Some((author, seq, envelope)) = inbox::next_ready(conn, now_ms)? else {
            return Ok(out);
        };
        let opened = open(&envelope)?;
        if opened.change.author != author || opened.change.seq != seq {
            return Err(SyncError::Protocol(
                "an inbox envelope isn't the change it was stored as",
            ));
        }
        out.purged
            .extend(apply(conn, cols, &opened.change, me.id(), now_ms)?);
        out.applied += 1;
    }
    out.more = inbox::next_ready(conn, now_ms)?.is_some();
    Ok(out)
}

/// [`pump_batch`] until nothing waits (tests, and the Node, where nothing else writes).
pub fn pump(
    conn: &Connection,
    cols: &[&dyn Collection],
    me: &DeviceKeys,
    group: GroupId,
    key: &GroupKey,
    open: impl Fn(&[u8]) -> Result<Opened>,
    now_ms: u64,
) -> Result<PumpBatch> {
    let mut total = PumpBatch::default();
    loop {
        let b = pump_batch(conn, cols, me, group, key, &open, 100, now_ms)?;
        total.local.extend(b.local);
        total.applied += b.applied;
        total.purged.extend(b.purged);
        if !b.more {
            return Ok(total);
        }
    }
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
    fn a_change_naming_a_record_by_another_spelling_of_its_id_is_refused() {
        let (mut a, mut b, key, members) = pair();
        let c = chat::create_conversation(&a.conn, "Secret plans").unwrap();
        let e = a.send(&key, NOW);
        b.take(&e, &key, &members, NOW);
        chat::delete_conversation(&b.conn, c.id).unwrap();
        chat::purge_deleted(&b.conn, i64::MAX).unwrap();
        b.send(&key, NOW + 1);
        // A member crafts a Put for the purged chat under its id in upper case.
        let mut fields = BTreeMap::new();
        for (k, v) in [
            ("title", Value::String("Back again".into())),
            ("model", Value::Null),
            ("project_id", Value::Null),
            ("deleted_at", Value::Null),
            ("created_at", Value::from(1)),
            ("updated_at", Value::from(1)),
        ] {
            fields.insert(k.to_owned(), v);
        }
        let crafted = inbox::next_change(
            &a.conn,
            a.keys.id(),
            GROUP,
            0,
            vec![Op::Put {
                collection: "chat".into(),
                id: c.id.to_string().to_uppercase(),
                fields,
            }],
            NOW + 2,
        )
        .unwrap();
        // b has a's earlier changes; apply the crafted one directly.
        let err = apply(
            &b.conn,
            &store_collections(),
            &crafted,
            b.keys.id(),
            NOW + 3,
        );
        assert!(err.is_err());
        assert!(b.chats().is_empty(), "the purged chat doesn't come back");
        // And an Append on a collection that isn't append-only is refused.
        let append = Change {
            ops: vec![Op::Append {
                collection: "chat".into(),
                id: dn_core::Id::new().to_string(),
                value: Value::Object(Map::new()),
            }],
            ..crafted
        };
        assert!(apply(&b.conn, &store_collections(), &append, b.keys.id(), NOW + 3).is_err());
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
