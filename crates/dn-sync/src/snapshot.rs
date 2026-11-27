//! Snapshots and compaction (docs/design/dn-sync.md §9.2, §9.4, §10.1; #294).
//!
//! **The Node keeps the records too.** It applies every change it keeps, like any device,
//! so it can hand a device the current state instead of the whole history.
//!
//! **A snapshot** is the Node's applied state at its vector:
//! - every synced record, with the write behind each field (author, seq, HLC), so later
//!   changes merge exactly as they would after a full replay;
//! - the text conflicts still open, and the tombstones;
//! - the Node's vector and clock.
//!
//! It's serialised as JSON, signed by the Node's device key, and sealed with the current
//! group key under its own label, so it can never be opened as an envelope. It goes only
//! over the paired channel, never through a Relay. The Node keeps the latest one
//! (`sync_snapshot`) and builds a new one when its state has moved on.
//!
//! **Installing it** ([`install`]) is a replace, made exact by one rule: a device first
//! records and pushes all its own changes, and the Node applies them, so the snapshot covers
//! everything the device has applied or made. If anything is left (a local edit since the
//! push, a change the Node hasn't applied yet), the install is refused before anything is
//! written, and the next round tries again. It happens in one transaction, so a refused or
//! cut-off download leaves nothing installed.
//!
//! **Compaction** ([`floor`], [`drop_covered`]): each member says what it has applied
//! (`sync_acks`). Envelopes every current member has applied, and the Node too, leave the
//! log; `sync_seen` keeps each change's hash, so a later copy is still recognised. A member
//! not seen for [`STALE_MEMBER_MS`] stops holding the log back, and gets a snapshot when it's
//! back (§9.2). A device that's behind the floor gets a snapshot instead of envelopes.

use crate::b64;
use crate::change::{GroupId, Vector};
use crate::devices::DeviceList;
use crate::envelope::{self, GroupKey, open_blob, seal_blob};
use crate::hlc::Hlc;
use crate::identity::{DeviceId, DeviceKeys, PublicIdentity};
use crate::inbox;
use crate::membership::Chain;
use crate::records::{self, Collection, CollectionKind};
use crate::{Result, SyncError};
use dn_store::sync_rows;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub(crate) const MIGRATION: &str = "
-- What each member said it has applied (author hex -> seq, as JSON), and when: the Node
-- drops envelopes every current member has applied (§9.2).
CREATE TABLE sync_acks (
    device TEXT    PRIMARY KEY NOT NULL,
    vector TEXT    NOT NULL,
    at     INTEGER NOT NULL
) WITHOUT ROWID;

-- Where the Node's log starts: for each author, the highest change dropped from it.
CREATE TABLE sync_log_start (
    author TEXT    PRIMARY KEY NOT NULL,
    seq    INTEGER NOT NULL
) WITHOUT ROWID;

-- The Node's latest snapshot, sealed, and the vector it covers (§9.4).
CREATE TABLE sync_snapshot (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    vector  TEXT    NOT NULL,
    sealed  BLOB    NOT NULL,
    made_at INTEGER NOT NULL
);
";

/// HKDF label and associated data for a sealed snapshot.
const LABEL: &[u8] = b"dn-sync v1 snapshot";

/// The snapshot format's version.
pub const SNAPSHOT_VERSION: u32 = 1;

/// Largest sealed snapshot sent or accepted, in bytes. A snapshot is built and loaded in
/// memory (#327).
pub const MAX_SNAPSHOT_BYTES: usize = 512 * 1024 * 1024;

/// Bytes of sealed snapshot in one `SnapshotChunk` (before base64).
pub const CHUNK_BYTES: usize = 1024 * 1024;

/// A member not seen for this long stops holding the log back (§9.2: 90 days).
pub const STALE_MEMBER_MS: i64 = 90 * 24 * 3600 * 1000;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    v: u32,
    /// Hex.
    group: String,
    /// The Node's device id, hex.
    node: String,
    vector: BTreeMap<String, u64>,
    /// The Node's clock.
    hlc: u64,
    records: Vec<Record>,
    conflicts: Vec<OpenConflict>,
    tombstones: Vec<Tombstone>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    collection: String,
    id: String,
    fields: Map<String, Value>,
    /// The write behind each synced field (`""` for an appended record).
    writes: Vec<Write>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
    field: String,
    author: String,
    seq: u64,
    hlc: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenConflict {
    collection: String,
    id: String,
    field: String,
    /// The version not shown, as JSON text.
    value: String,
    author: String,
    seq: u64,
    hlc: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tombstone {
    collection: String,
    id: String,
    author: String,
    seq: u64,
    hlc: u64,
}

fn hex16(b: &[u8; 16]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn device(text: &str) -> Result<DeviceId> {
    DeviceId::from_hex(text).ok_or(SyncError::Protocol("a bad device id in a snapshot"))
}

/// A vector as it goes on the wire and in the tables (author hex -> seq).
pub fn vector_to_wire(v: &Vector) -> BTreeMap<String, u64> {
    v.0.iter().map(|(d, s)| (d.to_hex(), *s)).collect()
}

pub fn vector_from_wire(v: &BTreeMap<String, u64>) -> Result<Vector> {
    let mut out = Vector::default();
    for (d, s) in v {
        out.set(device(d)?, *s);
    }
    Ok(out)
}

fn vector_text(v: &Vector) -> Result<String> {
    serde_json::to_string(&vector_to_wire(v)).map_err(|_| SyncError::Protocol("encode a vector"))
}

fn vector_parse(text: &str) -> Result<Vector> {
    let map: BTreeMap<String, u64> =
        serde_json::from_str(text).map_err(|_| SyncError::Protocol("a damaged vector"))?;
    vector_from_wire(&map)
}

fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// Builds a snapshot of this runtime's applied state, signed by `signer` (the Node) and
/// sealed with `key`. Returns the vector it covers and the sealed bytes.
pub fn build(
    conn: &Connection,
    cols: &[&dyn Collection],
    signer: &DeviceKeys,
    group: GroupId,
    key: &GroupKey,
) -> Result<(Vector, Vec<u8>)> {
    let tx = conn.unchecked_transaction()?;
    let vector = inbox::vector(&tx)?;
    let hlc = inbox::clock(&tx)?.last.0;

    let mut writes: BTreeMap<(String, String), Vec<Write>> = BTreeMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT collection, id, field, author, seq, hlc FROM sync_fields
             ORDER BY collection, id, field",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        for row in rows {
            let (c, id, field, author, seq, hlc) = row?;
            writes.entry((c, id)).or_default().push(Write {
                field,
                author,
                seq: to_u64(seq),
                hlc: to_u64(hlc),
            });
        }
    }
    let mut out_records = Vec::with_capacity(writes.len());
    for ((c, id), w) in writes {
        let col = records::collection(cols, &c)?;
        let fields = col.read(&tx, &id)?.ok_or_else(|| {
            SyncError::Store(format!("a synced {c} is missing from this runtime's store"))
        })?;
        out_records.push(Record {
            collection: c,
            id,
            fields,
            writes: w,
        });
    }

    let conflicts = {
        let mut stmt = tx.prepare(
            "SELECT collection, id, field, value, author, seq, hlc FROM sync_conflicts
             ORDER BY collection, id, field, author, seq",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(OpenConflict {
                collection: r.get(0)?,
                id: r.get(1)?,
                field: r.get(2)?,
                value: r.get(3)?,
                author: r.get(4)?,
                seq: to_u64(r.get(5)?),
                hlc: to_u64(r.get(6)?),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let tombstones = {
        let mut stmt = tx.prepare(
            "SELECT collection, id, author, seq, hlc FROM sync_purged ORDER BY collection, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Tombstone {
                collection: r.get(0)?,
                id: r.get(1)?,
                author: r.get(2)?,
                seq: to_u64(r.get(3)?),
                hlc: to_u64(r.get(4)?),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    drop(tx);

    let body = Body {
        v: SNAPSHOT_VERSION,
        group: hex16(&group.0),
        node: signer.id().to_hex(),
        vector: vector_to_wire(&vector),
        hlc,
        records: out_records,
        conflicts,
        tombstones,
    };
    let bytes = serde_json::to_vec(&body).map_err(|_| SyncError::Protocol("encode a snapshot"))?;
    let sealed = seal_blob(LABEL, &group, &bytes, signer, key)?;
    if sealed.len() > MAX_SNAPSHOT_BYTES {
        return Err(SyncError::Store(format!(
            "the synced data is too large for a snapshot ({} MB, at most {} MB)",
            sealed.len() / (1024 * 1024),
            MAX_SNAPSHOT_BYTES / (1024 * 1024)
        )));
    }
    Ok((vector, sealed))
}

/// The Node's snapshot of its current state: the stored one if the state hasn't moved on
/// since, otherwise a new one, stored in its place. Returns its vector and size.
pub fn current(
    conn: &Connection,
    cols: &[&dyn Collection],
    signer: &DeviceKeys,
    group: GroupId,
    key: &GroupKey,
    now_ms: u64,
) -> Result<(Vector, u64)> {
    let now_vector = inbox::vector(conn)?;
    let stored: Option<(String, i64, u32)> = conn
        .query_row(
            "SELECT vector, length(sealed), substr(sealed, 18, 4) FROM sync_snapshot WHERE id = 1",
            [],
            |r| {
                let epoch: Vec<u8> = r.get(2)?;
                let e = <[u8; 4]>::try_from(epoch.as_slice())
                    .map(u32::from_be_bytes)
                    .unwrap_or(u32::MAX);
                Ok((r.get(0)?, r.get(1)?, e))
            },
        )
        .optional()?;
    if let Some((v, len, epoch)) = stored
        && epoch == key.epoch
        && vector_parse(&v)? == now_vector
    {
        return Ok((now_vector, to_u64(len)));
    }
    let (vector, sealed) = build(conn, cols, signer, group, key)?;
    conn.execute(
        "INSERT INTO sync_snapshot (id, vector, sealed, made_at) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT (id) DO UPDATE SET vector = excluded.vector, sealed = excluded.sealed,
            made_at = excluded.made_at",
        params![
            vector_text(&vector)?,
            sealed,
            i64::try_from(now_ms).unwrap_or(i64::MAX)
        ],
    )?;
    Ok((vector, sealed.len() as u64))
}

/// Up to [`CHUNK_BYTES`] of the stored snapshot from `offset`, and whether more follow.
/// `None` when the stored snapshot isn't the one for `vector` any more (another device's
/// request replaced it): the device starts over in its next round.
pub fn chunk(conn: &Connection, vector: &Vector, offset: u64) -> Result<Option<(Vec<u8>, bool)>> {
    let start = i64::try_from(offset)
        .map_err(|_| SyncError::Protocol("a bad snapshot offset"))?
        .checked_add(1)
        .ok_or(SyncError::Protocol("a bad snapshot offset"))?;
    let row: Option<(String, Vec<u8>, i64)> = conn
        .query_row(
            "SELECT vector, substr(sealed, ?1, ?2), length(sealed) FROM sync_snapshot WHERE id = 1",
            params![start, CHUNK_BYTES as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((v, data, len)) = row else {
        return Ok(None);
    };
    if vector_parse(&v)? != *vector {
        return Ok(None);
    }
    if offset > to_u64(len) {
        return Err(SyncError::Protocol("a snapshot offset past its end"));
    }
    let more = offset + (data.len() as u64) < to_u64(len);
    Ok(Some((data, more)))
}

/// What [`install`] did.
#[derive(Debug, Default)]
pub struct Installed {
    /// Records the snapshot's tombstones deleted here, as (collection, id).
    pub purged: Vec<(String, String)>,
    /// The vector this device has applied now.
    pub vector: Vector,
}

/// Installs a sealed snapshot from the Node pinned at pairing. See the module notes: it is
/// refused, with nothing written, unless it covers everything this device has applied and
/// no local edit waits to be recorded.
pub fn install(
    conn: &Connection,
    cols: &[&dyn Collection],
    sealed: &[u8],
    key: &GroupKey,
    node: &PublicIdentity,
    group: GroupId,
    now_ms: u64,
) -> Result<Installed> {
    if sealed.len() > MAX_SNAPSHOT_BYTES {
        return Err(SyncError::Protocol("the snapshot is too large"));
    }
    if envelope::header(sealed)?.group != group {
        return Err(SyncError::Protocol("a snapshot from another group"));
    }
    let bytes = open_blob(LABEL, sealed, key, node)?;
    let body: Body =
        serde_json::from_slice(&bytes).map_err(|_| SyncError::Protocol("a damaged snapshot"))?;
    if body.v != SNAPSHOT_VERSION {
        return Err(SyncError::Protocol("a snapshot in another format"));
    }
    if body.group != hex16(&group.0) || body.node != node.id().to_hex() {
        return Err(SyncError::Protocol(
            "the snapshot names another group or Node",
        ));
    }
    let vector = vector_from_wire(&body.vector)?;

    let tx = conn.unchecked_transaction()?;
    if !vector.covers(&inbox::vector(&tx)?) {
        return Err(SyncError::NotYet(
            "the Node hasn't applied every change this device has yet",
        ));
    }
    let rev: i64 = tx
        .query_row("SELECT rev FROM sync_scan WHERE id = 1", [], |r| r.get(0))
        .optional()?
        .unwrap_or(0);
    if !sync_rows::log_since(&tx, rev)
        .map_err(|e| SyncError::Store(e.to_string()))?
        .is_empty()
    {
        return Err(SyncError::NotYet("local edits wait to be sent first"));
    }
    let mut clock = inbox::clock(&tx)?;
    if clock.receive(Hlc(body.hlc), now_ms).is_err() {
        return Err(SyncError::NotYet(
            "the Node's clock is more than 10 minutes ahead of this device's",
        ));
    }

    let mut purged = Vec::new();
    for t in &body.tombstones {
        let col = records::collection(cols, &t.collection)?;
        sync_rows::canonical_id(&t.id).map_err(|e| SyncError::Store(e.to_string()))?;
        if records::is_purged(&tx, col.name(), &t.id)? {
            continue;
        }
        let (author, seq, hlc) = (device(&t.author)?, t.seq, Hlc(t.hlc));
        for (cc, cid) in col.children(&tx, &t.id)? {
            records::record_purge(&tx, cc, &cid, author, seq, hlc)?;
        }
        if col.read(&tx, &t.id)?.is_some() {
            col.purge(&tx, &t.id, hlc.millis() as i64)?;
            purged.push((col.name().to_owned(), t.id.clone()));
        }
        records::record_purge(&tx, col.name(), &t.id, author, seq, hlc)?;
    }

    // The snapshot is the state as of its vector, which covers everything applied here:
    // its versions replace this device's.
    tx.execute("DELETE FROM sync_conflicts", [])?;
    for r in &body.records {
        let col = records::collection(cols, &r.collection)?;
        sync_rows::canonical_id(&r.id).map_err(|e| SyncError::Store(e.to_string()))?;
        if records::is_purged(&tx, col.name(), &r.id)? {
            continue;
        }
        match col.kind() {
            CollectionKind::Append => {
                if records::field_write(&tx, col.name(), &r.id, "")?.is_none() {
                    col.write(&tx, &r.id, &r.fields)?;
                }
            }
            CollectionKind::Fields(spec) => {
                for w in &r.writes {
                    if !spec.iter().any(|(f, _)| *f == w.field) {
                        return Err(SyncError::Protocol(
                            "a snapshot field this app doesn't sync",
                        ));
                    }
                }
                col.write(&tx, &r.id, &r.fields)?;
            }
        }
        for w in &r.writes {
            let value = if w.field.is_empty() {
                Value::Null
            } else {
                r.fields.get(&w.field).cloned().unwrap_or(Value::Null)
            };
            records::record_field(
                &tx,
                col.name(),
                &r.id,
                &w.field,
                &value,
                device(&w.author)?,
                w.seq,
                Hlc(w.hlc),
            )?;
        }
    }
    for c in &body.conflicts {
        records::collection(cols, &c.collection)?;
        serde_json::from_str::<Value>(&c.value)
            .map_err(|_| SyncError::Protocol("a damaged conflict in a snapshot"))?;
        tx.execute(
            "INSERT OR IGNORE INTO sync_conflicts (collection, id, field, value, author, seq, hlc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                c.collection,
                c.id,
                c.field,
                c.value,
                device(&c.author)?.to_hex(),
                i64::try_from(c.seq).unwrap_or(i64::MAX),
                i64::try_from(c.hlc).unwrap_or(i64::MAX)
            ],
        )?;
    }

    let mine = inbox::vector(&tx)?;
    for (a, s) in &vector.0 {
        if *s > mine.get(*a) {
            inbox::set_vector(&tx, *a, *s)?;
        }
        tx.execute(
            "DELETE FROM sync_inbox WHERE author = ?1 AND seq <= ?2",
            params![a.to_hex(), i64::try_from(*s).unwrap_or(i64::MAX)],
        )?;
    }
    inbox::save_clock(&tx, clock)?;
    // The writes above went through the owning crates, which logged them: they are this
    // snapshot, not local edits, and their hashes are recorded, so skip them.
    let done: i64 = sync_rows::log_since(&tx, rev)
        .map_err(|e| SyncError::Store(e.to_string()))?
        .last()
        .map_or(rev, |e| e.rev);
    tx.execute(
        "INSERT INTO sync_scan (id, rev) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET rev = excluded.rev",
        [done],
    )?;
    sync_rows::prune_log(&tx, done).map_err(|e| SyncError::Store(e.to_string()))?;
    let vector = inbox::vector(&tx)?;
    tx.commit()?;
    Ok(Installed { purged, vector })
}

/// Records what member `device` says it has applied.
pub fn record_ack(
    conn: &Connection,
    device: DeviceId,
    applied: &Vector,
    now_ms: u64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_acks (device, vector, at) VALUES (?1, ?2, ?3)
         ON CONFLICT (device) DO UPDATE SET vector = excluded.vector, at = excluded.at",
        params![
            device.to_hex(),
            vector_text(applied)?,
            i64::try_from(now_ms).unwrap_or(i64::MAX)
        ],
    )?;
    Ok(())
}

/// The Node's floor: for each author, the highest change that every current member of
/// `chain` (not the Node, not one gone [`STALE_MEMBER_MS`]) has applied, and the Node too.
/// Empty while a recent member hasn't said what it has applied.
pub fn floor(conn: &Connection, chain: &Chain, now_ms: u64) -> Result<Vector> {
    let now = i64::try_from(now_ms).unwrap_or(i64::MAX);
    let node_id = chain.node().id();
    let mut acks: Vec<Vector> = Vec::new();
    for (id, _) in chain.current_members() {
        if id == node_id {
            continue;
        }
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT vector, at FROM sync_acks WHERE device = ?1",
                [id.to_hex()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        // When it was last seen: its latest round, else when it was paired. A member the
        // device list doesn't know (yet) counts as seen now.
        let listed = DeviceList::get(conn, id)?;
        let seen = row
            .as_ref()
            .map(|(_, at)| *at)
            .or_else(|| listed.as_ref().map(|d| d.added_at))
            .unwrap_or(now)
            .max(listed.and_then(|d| d.last_synced_at).unwrap_or(0));
        if now.saturating_sub(seen) > STALE_MEMBER_MS {
            continue;
        }
        match row {
            Some((v, _)) => acks.push(vector_parse(&v)?),
            None => return Ok(Vector::default()),
        }
    }
    let Some(first) = acks.first() else {
        return Ok(Vector::default());
    };
    let node = inbox::vector(conn)?;
    let mut out = Vector::default();
    for (a, s) in &first.0 {
        let low = acks
            .iter()
            .map(|v| v.get(*a))
            .chain(std::iter::once(node.get(*a)))
            .min()
            .unwrap_or(0)
            .min(*s);
        if low > 0 {
            out.set(*a, low);
        }
    }
    Ok(out)
}

/// The Node's compaction: drops the envelopes at or below [`floor`] and returns where its
/// log starts now (the highest change dropped per author, which never moves back). A
/// device that lacks a change at or below it needs a snapshot.
pub fn compact(conn: &Connection, chain: &Chain, now_ms: u64) -> Result<Vector> {
    let candidate = floor(conn, chain, now_ms)?;
    let tx = conn.unchecked_transaction()?;
    let mut start = log_start(&tx)?;
    for (a, s) in &candidate.0 {
        if *s > start.get(*a) {
            start.set(*a, *s);
            tx.execute(
                "INSERT INTO sync_log_start (author, seq) VALUES (?1, ?2)
                 ON CONFLICT (author) DO UPDATE SET seq = excluded.seq",
                params![a.to_hex(), i64::try_from(*s).unwrap_or(i64::MAX)],
            )?;
        }
    }
    drop_in(&tx, &start, None)?;
    tx.commit()?;
    Ok(start)
}

/// Where the Node's log starts (see [`compact`]).
pub fn log_start(conn: &Connection) -> Result<Vector> {
    let mut stmt = conn.prepare("SELECT author, seq FROM sync_log_start")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut v = Vector::default();
    for row in rows {
        let (a, s) = row?;
        v.set(device(&a)?, to_u64(s));
    }
    Ok(v)
}

/// Drops envelopes at or below `floor` from the log (`sync_envelopes`), and this device's
/// own sent ones from `sync_outbox` when `me` is given. Returns how many envelopes went.
pub fn drop_covered(conn: &Connection, floor: &Vector, me: Option<DeviceId>) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let n = drop_in(&tx, floor, me)?;
    tx.commit()?;
    Ok(n)
}

fn drop_in(tx: &Connection, floor: &Vector, me: Option<DeviceId>) -> Result<usize> {
    let mut n = 0;
    for (a, s) in &floor.0 {
        let s = i64::try_from(*s).unwrap_or(i64::MAX);
        n += tx.execute(
            "DELETE FROM sync_envelopes WHERE author = ?1 AND seq <= ?2",
            params![a.to_hex(), s],
        )?;
        if Some(*a) == me {
            tx.execute("DELETE FROM sync_outbox WHERE seq <= ?1", [s])?;
        }
    }
    Ok(n)
}

/// Whether a device holding `held` needs a snapshot: some author's changes it lacks are
/// below the floor, so they're no longer in the log.
pub fn behind(held: &Vector, floor: &Vector) -> bool {
    floor.0.iter().any(|(a, s)| held.get(*a) < *s)
}

/// Base64 for a chunk on the wire.
pub fn encode_chunk(data: &[u8]) -> String {
    b64::encode(data)
}
