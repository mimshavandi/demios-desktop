//! The Node's synced records, encrypted at rest (#335, docs/design/dn-sync.md §9.5).
//!
//! The Node applies changes so it can hand new devices a snapshot. It keeps what it applies
//! **sealed**, never in dn-store's readable tables:
//!
//! - **Records:** one row per record in `sync_node_records`. The record's fields are JSON,
//!   sealed with AES-256-GCM under the records key, with a random nonce. The associated
//!   data binds the row to its collection, id and key number, and to the writes
//!   `sync_fields` records for it (field, author, change number, time). A row moved to
//!   another id doesn't open, and neither does an older copy of a row, or a purged one put
//!   back, unless the bookkeeping is rolled back with it (a whole-database rollback, which
//!   this doesn't cover).
//! - **Conflicts:** the version not shown is sealed the same way, bound to its record,
//!   field, author and change number.
//! - **Field hashes:** keyed (HMAC-SHA256 with a key derived from the records key), so a
//!   hash can't confirm a guess about a value. The Node never scans local edits, so it
//!   never compares them.
//!
//! **The records key** is 32 random bytes in the Node's secret store (`device/records.<n>`),
//! next to its identity and group keys. It's separate from the group keys: a removed device
//! knows old group keys and must not be able to read the Node's disk. A key that should be
//! there and isn't is an error that says so; a new one is never made in its place.
//!
//! **What stays readable:** record ids, their collection, a message's chat (`parent`, to
//! purge children), sealed sizes, and the bookkeeping in the other `sync_*` tables
//! (authors, change numbers, times, tombstones, devices).
//!
//! **Rotation:** [`RecordsKeys::rotate`] makes key `n + 1`, which new writes use at once.
//! [`RecordsKeys::reseal_batch`] re-seals what still uses an older key, a batch per
//! transaction, so an interrupted rotation goes on where it stopped. [`RecordsKeys::finish`]
//! then deletes every older key from the secret store and vacuums the database, so no page
//! keeps rows under them. It deletes all older keys each time, so a run that stopped after
//! moving on but before deleting is completed by the next one.

use crate::b64;
use crate::identity::DeviceId;
use crate::records::{self, Collection, CollectionKind, ConflictAt, WriteBy};
use crate::{Result, SyncError};
use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use aws_lc_rs::{hkdf, hmac};
use dn_secrets::{Secret, SecretId, SecretStore};
use dn_store::sync_rows::{self, ChatRow, Kind, MessageRow, ProjectRow};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use zeroize::Zeroizing;

pub(crate) const MIGRATION: &str = "
-- The Node's synced records, sealed under the records key (§9.5).
CREATE TABLE sync_node_records (
    collection TEXT    NOT NULL,
    id         TEXT    NOT NULL,
    parent     TEXT,
    key_n      INTEGER NOT NULL,
    sealed     BLOB    NOT NULL,
    PRIMARY KEY (collection, id)
) WITHOUT ROWID;
CREATE INDEX sync_node_records_parent ON sync_node_records (collection, parent);
CREATE INDEX sync_node_records_key ON sync_node_records (key_n);

-- Which records keys are in use: `current` seals new writes; keys from `oldest` up may
-- still be in use while a rotation runs.
CREATE TABLE sync_records_key (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    current INTEGER NOT NULL,
    oldest  INTEGER NOT NULL
);
";

/// Associated-data label for everything sealed at rest.
const LABEL: &[u8] = b"dn-sync v1 at rest";
/// HKDF info for the field-hash key.
const HASH_INFO: &[u8] = b"dn-sync v1 field hash";
const N_LEN: usize = 4;
const TAG_LEN: usize = 16;

/// The secret-store id of records key `n`.
fn key_id(n: u32) -> SecretId {
    SecretId::device(format!("records.{n}"))
}

struct Key {
    aead: LessSafeKey,
    mac: hmac::Key,
}

impl Key {
    fn new(bytes: &[u8; 32]) -> Result<Self> {
        let aead = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, bytes).map_err(|_| SyncError::Crypto("aead key"))?,
        );
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(bytes);
        let info = [HASH_INFO];
        let mac = hmac::Key::from(
            prk.expand(&info, hmac::HMAC_SHA256)
                .map_err(|_| SyncError::Crypto("hkdf"))?,
        );
        Ok(Self { aead, mac })
    }
}

struct Ring {
    current: u32,
    oldest: u32,
    keys: BTreeMap<u32, Key>,
}

/// The Node's records keys, loaded from its secret store.
pub struct RecordsKeys {
    ring: RwLock<Ring>,
}

/// What a rotation has left to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rotation {
    /// The key new writes use.
    pub current: u32,
    /// Records and conflicts still sealed with an older key.
    pub left: u64,
}

fn load_key(store: &dyn SecretStore, n: u32) -> Result<Key> {
    let secret = store.get(&key_id(n))?.ok_or_else(|| {
        SyncError::Store(format!(
            "the key for the Node's synced records (records.{n}) isn't in the Node's secret \
             store, so they can't be read. Restore the secret store this database was used with"
        ))
    })?;
    let bytes = Zeroizing::new(
        b64::decode_array::<32>(secret.expose()).ok_or(SyncError::DamagedKeys("records key"))?,
    );
    Key::new(&bytes)
}

fn new_key(store: &dyn SecretStore, n: u32) -> Result<Key> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    aws_lc_rs::rand::fill(bytes.as_mut()).map_err(|_| SyncError::Crypto("random"))?;
    let text = Zeroizing::new(b64::encode(bytes.as_ref()));
    store.set(&key_id(n), &Secret::new(text.as_str()))?;
    Key::new(&bytes)
}

/// Length-prefixed parts, so no two different contexts give the same bytes.
fn aad(n: u32, parts: &[&[u8]]) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(LABEL.len() + 4 + parts.iter().map(|p| p.len() + 4).sum::<usize>());
    out.extend_from_slice(LABEL);
    out.extend_from_slice(&n.to_be_bytes());
    for p in parts {
        out.extend_from_slice(&u32::try_from(p.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(p);
    }
    out
}

/// The writes `sync_fields` records for a record, as they will be once `pending` (fields
/// about to be written by one change) is recorded too, in a fixed order.
fn writes(
    conn: &Connection,
    collection: &str,
    id: &str,
    pending: Option<(&mut dyn Iterator<Item = &str>, WriteBy)>,
) -> Result<Vec<u8>> {
    let mut map: BTreeMap<String, (String, i64, i64)> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT field, author, seq, hlc FROM sync_fields WHERE collection = ?1 AND id = ?2",
        )?;
        let rows = stmt.query_map(params![collection, id], |r| {
            Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
        })?;
        for row in rows {
            let (field, w) = row?;
            map.insert(field, w);
        }
    }
    if let Some((fields, by)) = pending {
        let w = (by.author.to_hex(), by.seq as i64, by.hlc.0 as i64);
        for f in fields {
            map.insert(f.to_owned(), w.clone());
        }
    }
    let mut out = Vec::new();
    for (field, (author, seq, hlc)) in map {
        for part in [field.as_bytes(), author.as_bytes()] {
            out.extend_from_slice(&u32::try_from(part.len()).unwrap_or(u32::MAX).to_be_bytes());
            out.extend_from_slice(part);
        }
        out.extend_from_slice(&seq.to_be_bytes());
        out.extend_from_slice(&hlc.to_be_bytes());
    }
    Ok(out)
}

fn record_aad(n: u32, collection: &str, id: &str, writes: &[u8]) -> Vec<u8> {
    aad(
        n,
        &[b"record", collection.as_bytes(), id.as_bytes(), writes],
    )
}

fn conflict_aad(n: u32, collection: &str, at: &ConflictAt<'_>) -> Vec<u8> {
    aad(
        n,
        &[
            b"conflict",
            collection.as_bytes(),
            at.id.as_bytes(),
            at.field.as_bytes(),
            &at.author.0,
            &at.seq.to_be_bytes(),
            &at.hlc.to_be_bytes(),
        ],
    )
}

/// The key number a sealed value names.
fn key_n(sealed: &[u8]) -> Result<u32> {
    let head: [u8; N_LEN] =
        sealed
            .get(..N_LEN)
            .and_then(|h| h.try_into().ok())
            .ok_or(SyncError::Protocol(
                "a sealed record in the Node's database is damaged",
            ))?;
    Ok(u32::from_be_bytes(head))
}

impl RecordsKeys {
    /// Loads the keys in use, or makes the first one on a Node that has none yet.
    pub fn load(conn: &Connection, store: &dyn SecretStore) -> Result<Self> {
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT current, oldest FROM sync_records_key WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let mut keys = BTreeMap::new();
        let (current, oldest) = match row {
            Some((c, o)) => {
                let bad = || SyncError::Protocol("damaged sync_records_key");
                let c = u32::try_from(c).map_err(|_| bad())?;
                let o = u32::try_from(o).map_err(|_| bad())?;
                if o == 0 || o > c {
                    return Err(bad());
                }
                for n in o..=c {
                    keys.insert(n, load_key(store, n)?);
                }
                (c, o)
            }
            None => {
                // The key is kept before the row says it exists, so a row never names a key
                // that wasn't stored. Nothing is sealed before the row exists.
                let empty: i64 =
                    conn.query_row("SELECT COUNT(*) FROM sync_node_records", [], |r| r.get(0))?;
                if empty != 0 {
                    return Err(SyncError::Protocol(
                        "the Node's database has sealed records but no records key",
                    ));
                }
                keys.insert(1, new_key(store, 1)?);
                conn.execute(
                    "INSERT INTO sync_records_key (id, current, oldest) VALUES (1, 1, 1)",
                    [],
                )?;
                (1, 1)
            }
        };
        Ok(Self {
            ring: RwLock::new(Ring {
                current,
                oldest,
                keys,
            }),
        })
    }

    fn ring(&self) -> std::sync::RwLockReadGuard<'_, Ring> {
        self.ring.read().unwrap_or_else(|p| p.into_inner())
    }

    /// The key new writes use.
    pub fn current(&self) -> u32 {
        self.ring().current
    }

    fn seal(&self, aad_for: impl Fn(u32) -> Vec<u8>, plain: &[u8]) -> Result<Vec<u8>> {
        let ring = self.ring();
        let n = ring.current;
        let key = ring.keys.get(&n).ok_or(SyncError::Crypto("records key"))?;
        let mut nonce = [0u8; NONCE_LEN];
        aws_lc_rs::rand::fill(&mut nonce).map_err(|_| SyncError::Crypto("random"))?;
        let mut out = Vec::with_capacity(N_LEN + NONCE_LEN + plain.len() + TAG_LEN);
        out.extend_from_slice(&n.to_be_bytes());
        out.extend_from_slice(&nonce);
        let mut body = plain.to_vec();
        key.aead
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad_for(n)),
                &mut body,
            )
            .map_err(|_| SyncError::Crypto("seal"))?;
        out.extend_from_slice(&body);
        Ok(out)
    }

    fn open(&self, aad_for: impl Fn(u32) -> Vec<u8>, sealed: &[u8]) -> Result<Vec<u8>> {
        let damaged = SyncError::Protocol("a sealed record in the Node's database is damaged");
        let n = key_n(sealed)?;
        let ring = self.ring();
        let key = ring.keys.get(&n).ok_or_else(|| {
            SyncError::Store(format!(
                "a synced record is sealed with records key {n}, which this Node doesn't have"
            ))
        })?;
        let nonce: [u8; NONCE_LEN] = sealed
            .get(N_LEN..N_LEN + NONCE_LEN)
            .and_then(|b| b.try_into().ok())
            .ok_or(SyncError::Protocol(
                "a sealed record in the Node's database is damaged",
            ))?;
        let mut body = sealed[N_LEN + NONCE_LEN..].to_vec();
        let plain = key
            .aead
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad_for(n)),
                &mut body,
            )
            .map_err(|_| damaged)?;
        Ok(plain.to_vec())
    }

    fn hash(&self, parts: &[&[u8]]) -> Result<Vec<u8>> {
        let ring = self.ring();
        let n = ring.current;
        let key = ring.keys.get(&n).ok_or(SyncError::Crypto("records key"))?;
        let mut ctx = hmac::Context::with_key(&key.mac);
        ctx.update(&aad(n, parts));
        let mut out = n.to_be_bytes().to_vec();
        out.extend_from_slice(ctx.sign().as_ref());
        Ok(out)
    }

    /// Starts a rotation: key `current + 1`, which new writes use at once. While one is
    /// running, it returns what is left of it instead of starting another.
    pub fn rotate(&self, conn: &Connection, store: &dyn SecretStore) -> Result<Rotation> {
        let mut ring = self.ring.write().unwrap_or_else(|p| p.into_inner());
        if ring.oldest == ring.current {
            let n = ring
                .current
                .checked_add(1)
                .ok_or(SyncError::Crypto("records key number"))?;
            let key = new_key(store, n)?;
            conn.execute("UPDATE sync_records_key SET current = ?1 WHERE id = 1", [n])?;
            ring.keys.insert(n, key);
            ring.current = n;
        }
        let current = ring.current;
        drop(ring);
        Ok(Rotation {
            current,
            left: self.left(conn)?,
        })
    }

    /// Records and conflicts still sealed with an older key.
    pub fn left(&self, conn: &Connection) -> Result<u64> {
        let current = self.current();
        let records: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sync_node_records WHERE key_n < ?1",
            [current],
            |r| r.get(0),
        )?;
        let conflicts = old_conflicts(conn, current, usize::MAX)?.len();
        Ok(u64::try_from(records).unwrap_or(0) + conflicts as u64)
    }

    /// Re-seals up to `limit` records and conflicts still under an older key, in one
    /// transaction. Returns how many are left.
    pub fn reseal_batch(&self, conn: &Connection, limit: usize) -> Result<u64> {
        let current = self.current();
        let tx = conn.unchecked_transaction()?;
        let rows: Vec<(String, String, Vec<u8>)> = {
            let mut stmt = tx.prepare(
                "SELECT collection, id, sealed FROM sync_node_records WHERE key_n < ?1 LIMIT ?2",
            )?;
            stmt.query_map(
                params![current, i64::try_from(limit).unwrap_or(i64::MAX)],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?
            .collect::<rusqlite::Result<_>>()?
        };
        let room = limit.saturating_sub(rows.len());
        for (col, id, sealed) in rows {
            let w = writes(&tx, &col, &id, None)?;
            let plain = Zeroizing::new(self.open(|n| record_aad(n, &col, &id, &w), &sealed)?);
            let fresh = self.seal(|n| record_aad(n, &col, &id, &w), &plain)?;
            tx.execute(
                "UPDATE sync_node_records SET key_n = ?3, sealed = ?4 WHERE collection = ?1 AND id = ?2",
                params![col, id, current, fresh],
            )?;
        }
        for c in old_conflicts(&tx, current, room)? {
            let at = c.at();
            let plain =
                Zeroizing::new(self.open(|n| conflict_aad(n, &c.collection, &at), &c.sealed)?);
            let fresh = b64::encode(&self.seal(|n| conflict_aad(n, &c.collection, &at), &plain)?);
            tx.execute(
                "UPDATE sync_conflicts SET value = ?6
                 WHERE collection = ?1 AND id = ?2 AND field = ?3 AND author = ?4 AND seq = ?5",
                params![
                    c.collection,
                    c.id,
                    c.field,
                    c.author.to_hex(),
                    c.seq as i64,
                    fresh
                ],
            )?;
        }
        tx.commit()?;
        self.left(conn)
    }

    /// Ends a rotation once nothing uses an older key: deletes the old keys from the secret
    /// store, then vacuums the database and truncates its WAL, so no page keeps rows sealed
    /// with them. Returns `false`, and does nothing, while something still does.
    pub fn finish(&self, conn: &Connection, store: &dyn SecretStore) -> Result<bool> {
        if self.left(conn)? != 0 {
            return Ok(false);
        }
        let mut ring = self.ring.write().unwrap_or_else(|p| p.into_inner());
        let current = ring.current;
        // The row moves first: a key it no longer names is never needed again, so a start
        // never looks for a key already deleted.
        if ring.oldest != current {
            conn.execute(
                "UPDATE sync_records_key SET oldest = ?1 WHERE id = 1",
                [current],
            )?;
            ring.oldest = current;
            ring.keys.retain(|n, _| *n == current);
        }
        drop(ring);
        // Every older key, not only the last rotation's, so a run that stopped after moving
        // the row but before deleting is completed here.
        for n in 1..current {
            store.delete(&key_id(n))?;
        }
        conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(true)
    }
}

struct OldConflict {
    collection: String,
    id: String,
    field: String,
    author: DeviceId,
    seq: u64,
    hlc: u64,
    sealed: Vec<u8>,
}

impl OldConflict {
    fn at(&self) -> ConflictAt<'_> {
        ConflictAt {
            id: &self.id,
            field: &self.field,
            author: self.author,
            seq: self.seq,
            hlc: self.hlc,
        }
    }
}

/// Conflicts sealed with a key older than `current`, at most `limit`.
fn old_conflicts(conn: &Connection, current: u32, limit: usize) -> Result<Vec<OldConflict>> {
    let mut out = Vec::new();
    if limit == 0 {
        return Ok(out);
    }
    let mut stmt =
        conn.prepare("SELECT collection, id, field, author, seq, hlc, value FROM sync_conflicts")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, String>(6)?,
        ))
    })?;
    let damaged = || SyncError::Protocol("a sealed conflict in the Node's database is damaged");
    for row in rows {
        let (collection, id, field, author, seq, hlc, value) = row?;
        let sealed = b64::decode(&value).ok_or_else(damaged)?;
        if key_n(&sealed)? < current {
            out.push(OldConflict {
                collection,
                id,
                field,
                author: DeviceId::from_hex(&author).ok_or_else(damaged)?,
                seq: u64::try_from(seq).map_err(|_| damaged())?,
                hlc: u64::try_from(hlc).map_err(|_| damaged())?,
                sealed,
            });
            if out.len() == limit {
                break;
            }
        }
    }
    Ok(out)
}

/// Which kind of record a sealed collection holds.
#[derive(Clone, Copy)]
enum Spec {
    Projects,
    Chats,
    Messages,
}

/// One of the Node's sealed collections. Merging, refers-to and parents are the same as a
/// device's ([`records::store_collections`]); only where the record is kept differs.
pub struct Sealed {
    keys: Arc<RecordsKeys>,
    spec: Spec,
}

/// The Node's collections, sealed under its records key.
pub struct NodeRecords {
    keys: Arc<RecordsKeys>,
    projects: Sealed,
    chats: Sealed,
    messages: Sealed,
}

impl NodeRecords {
    pub fn new(keys: Arc<RecordsKeys>) -> Self {
        let sealed = |spec| Sealed {
            keys: keys.clone(),
            spec,
        };
        Self {
            projects: sealed(Spec::Projects),
            chats: sealed(Spec::Chats),
            messages: sealed(Spec::Messages),
            keys,
        }
    }

    pub fn keys(&self) -> &Arc<RecordsKeys> {
        &self.keys
    }

    /// Projects, chats and messages, in the order [`records::store_collections`] gives.
    pub fn collections(&self) -> [&dyn Collection; 3] {
        [&self.projects, &self.chats, &self.messages]
    }
}

fn store_err(e: dn_store::StoreError) -> SyncError {
    match e {
        dn_store::StoreError::Sqlite(e) => SyncError::Db(e),
        other => SyncError::Store(other.to_string()),
    }
}

/// The record's fields merged over `current`, checked as the row type a device would write,
/// and returned without the id.
fn checked<T: serde::Serialize + serde::de::DeserializeOwned>(
    current: Option<Map<String, Value>>,
    id: &str,
    fields: &Map<String, Value>,
) -> Result<(T, Map<String, Value>)> {
    let mut m = current.unwrap_or_default();
    for (k, v) in fields {
        m.insert(k.clone(), v.clone());
    }
    m.insert("id".into(), Value::String(id.to_owned()));
    let row: T = serde_json::from_value(Value::Object(m))
        .map_err(|_| SyncError::Protocol("a record is missing fields"))?;
    let Ok(Value::Object(mut m)) = serde_json::to_value(&row) else {
        return Err(SyncError::Protocol("record isn't an object"));
    };
    m.remove("id");
    Ok((row, m))
}

impl Sealed {
    fn device_side(&self) -> &'static dyn Collection {
        match self.spec {
            Spec::Projects => &records::Projects,
            Spec::Chats => &records::Chats,
            Spec::Messages => &records::Messages,
        }
    }

    fn get(&self, conn: &Connection, id: &str) -> Result<Option<Map<String, Value>>> {
        let name = self.name();
        let sealed: Option<Vec<u8>> = conn
            .query_row(
                "SELECT sealed FROM sync_node_records WHERE collection = ?1 AND id = ?2",
                params![name, id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(sealed) = sealed else {
            return Ok(None);
        };
        let w = writes(conn, name, id, None)?;
        let plain = Zeroizing::new(self.keys.open(|n| record_aad(n, name, id, &w), &sealed)?);
        match serde_json::from_slice(&plain) {
            Ok(Value::Object(m)) => Ok(Some(m)),
            _ => Err(SyncError::Protocol(
                "a sealed record in the Node's database is damaged",
            )),
        }
    }

    /// Seals and keeps `fields` (the whole record). `by` names the fields this change
    /// writes and the change, which `sync_fields` records right after; `None` when the
    /// bookkeeping doesn't change (a purge's side effect, a rotation).
    fn put(
        &self,
        conn: &Connection,
        id: &str,
        parent: Option<&str>,
        fields: &Map<String, Value>,
        by: Option<(&mut dyn Iterator<Item = &str>, WriteBy)>,
    ) -> Result<()> {
        let name = self.name();
        let plain = Zeroizing::new(
            serde_json::to_vec(fields).map_err(|_| SyncError::Protocol("record isn't JSON"))?,
        );
        let w = writes(conn, name, id, by)?;
        let sealed = self.keys.seal(|n| record_aad(n, name, id, &w), &plain)?;
        conn.execute(
            "INSERT INTO sync_node_records (collection, id, parent, key_n, sealed)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (collection, id) DO UPDATE SET
                parent = excluded.parent, key_n = excluded.key_n, sealed = excluded.sealed",
            params![name, id, parent, key_n(&sealed)?, sealed],
        )?;
        Ok(())
    }

    fn ids_with_parent(conn: &Connection, collection: &str, parent: &str) -> Result<Vec<String>> {
        let mut stmt = conn.prepare(
            "SELECT id FROM sync_node_records WHERE collection = ?1 AND parent = ?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![collection, parent], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn delete(conn: &Connection, collection: &str, id: &str) -> Result<()> {
        conn.execute(
            "DELETE FROM sync_node_records WHERE collection = ?1 AND id = ?2",
            params![collection, id],
        )?;
        Ok(())
    }
}

impl Collection for Sealed {
    fn name(&self) -> &'static str {
        self.device_side().name()
    }
    fn log_kind(&self) -> Kind {
        self.device_side().log_kind()
    }
    fn kind(&self) -> CollectionKind {
        self.device_side().kind()
    }
    fn read(&self, conn: &Connection, id: &str) -> Result<Option<Map<String, Value>>> {
        self.get(conn, id)
    }
    fn write(
        &self,
        conn: &Connection,
        id: &str,
        fields: &Map<String, Value>,
        by: WriteBy,
    ) -> Result<()> {
        // A field record's changed fields, or `""` for an appended record (as `apply`
        // records them).
        let mut changed = fields.keys().map(String::as_str);
        let mut appended = std::iter::once("");
        match self.spec {
            Spec::Projects => {
                let (row, m): (ProjectRow, _) = checked(self.get(conn, id)?, id, fields)?;
                sync_rows::canonical_id(&row.id).map_err(store_err)?;
                self.put(conn, id, None, &m, Some((&mut changed, by)))
            }
            Spec::Chats => {
                let (row, m): (ChatRow, _) = checked(self.get(conn, id)?, id, fields)?;
                sync_rows::canonical_id(&row.id).map_err(store_err)?;
                if let Some(p) = &row.project_id {
                    sync_rows::canonical_id(p).map_err(store_err)?;
                }
                self.put(
                    conn,
                    id,
                    row.project_id.as_deref(),
                    &m,
                    Some((&mut changed, by)),
                )
            }
            Spec::Messages => {
                let (row, m): (MessageRow, _) = checked(None, id, fields)?;
                sync_rows::check_message(&row).map_err(store_err)?;
                let chat: Option<i64> = conn
                    .query_row(
                        "SELECT 1 FROM sync_node_records WHERE collection = 'chat' AND id = ?1",
                        [&row.conversation_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if chat.is_none() {
                    return Err(SyncError::Store(format!(
                        "not found: conversation {}",
                        row.conversation_id
                    )));
                }
                // A message already here is left as it is (messages never change).
                if self.get(conn, id)?.is_some() {
                    return Ok(());
                }
                self.put(
                    conn,
                    id,
                    Some(&row.conversation_id),
                    &m,
                    Some((&mut appended, by)),
                )
            }
        }
    }
    fn purge(&self, conn: &Connection, id: &str, _at_ms: i64) -> Result<()> {
        match self.spec {
            Spec::Projects => {
                // Its chats stay, out of the project (as dn-store's purge does).
                for chat in Self::ids_with_parent(conn, "chat", id)? {
                    let chats = Sealed {
                        keys: self.keys.clone(),
                        spec: Spec::Chats,
                    };
                    if let Some(mut m) = chats.get(conn, &chat)? {
                        m.insert("project_id".into(), Value::Null);
                        chats.put(conn, &chat, None, &m, None)?;
                    }
                }
                Self::delete(conn, "project", id)
            }
            Spec::Chats => {
                conn.execute(
                    "DELETE FROM sync_node_records WHERE collection = 'message' AND parent = ?1",
                    [id],
                )?;
                Self::delete(conn, "chat", id)
            }
            Spec::Messages => Self::delete(conn, "message", id),
        }
    }
    fn parent(&self, fields: &Map<String, Value>) -> Option<(&'static str, String)> {
        self.device_side().parent(fields)
    }
    fn refers_to(&self, fields: &Map<String, Value>) -> Vec<(&'static str, String)> {
        self.device_side().refers_to(fields)
    }
    fn children(&self, conn: &Connection, id: &str) -> Result<Vec<(&'static str, String)>> {
        match self.spec {
            Spec::Chats => Ok(Self::ids_with_parent(conn, "message", id)?
                .into_iter()
                .map(|m| ("message", m))
                .collect()),
            _ => Ok(Vec::new()),
        }
    }
    fn field_hash(&self, id: &str, field: &str, value: &Value) -> Result<Vec<u8>> {
        let json = serde_json::to_vec(value).unwrap_or_default();
        self.keys.hash(&[
            b"field",
            self.name().as_bytes(),
            id.as_bytes(),
            field.as_bytes(),
            &json,
        ])
    }
    fn seal_conflict(&self, at: &ConflictAt<'_>, json: String) -> Result<String> {
        let name = self.name();
        let plain = Zeroizing::new(json.into_bytes());
        Ok(b64::encode(
            &self.keys.seal(|n| conflict_aad(n, name, at), &plain)?,
        ))
    }
    fn open_conflict(&self, at: &ConflictAt<'_>, kept: String) -> Result<String> {
        let name = self.name();
        let sealed = b64::decode(&kept).ok_or(SyncError::Protocol(
            "a sealed conflict in the Node's database is damaged",
        ))?;
        let plain = self.keys.open(|n| conflict_aad(n, name, at), &sealed)?;
        String::from_utf8(plain)
            .map_err(|_| SyncError::Protocol("a sealed conflict in the Node's database is damaged"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::GroupId;
    use crate::envelope::{GroupKey, open as open_envelope};
    use crate::identity::{DeviceKeys, PublicIdentity};
    use crate::inbox;
    use crate::records::{outbox, pump, scan, store_collections};
    use dn_secrets::MemoryStore;
    use dn_store::MigrationSet;
    use dn_store::chat::{self, NewMessage};
    use dn_store::project;

    const GROUP: GroupId = GroupId([9; 16]);
    const NOW: u64 = 1_800_000_000_000;

    fn db() -> Connection {
        dn_store::open_in_memory(&[MigrationSet::new("sync", crate::migrations())]).unwrap()
    }

    /// A desktop: dn-store's tables, readable.
    struct Dev {
        conn: Connection,
        keys: DeviceKeys,
        sent: u64,
    }

    impl Dev {
        fn new() -> Self {
            let conn = db();
            sync_rows::enable(&conn).unwrap();
            Self {
                conn,
                keys: DeviceKeys::generate().unwrap(),
                sent: 0,
            }
        }

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
    }

    /// A Node: the same apply, into the sealed collections.
    struct Node {
        conn: Connection,
        keys: DeviceKeys,
        store: MemoryStore,
        records: NodeRecords,
    }

    impl Node {
        fn new() -> Self {
            Self::on(db(), MemoryStore::new())
        }

        fn on(conn: Connection, store: MemoryStore) -> Self {
            let records = NodeRecords::new(Arc::new(RecordsKeys::load(&conn, &store).unwrap()));
            Self {
                conn,
                keys: DeviceKeys::generate().unwrap(),
                store,
                records,
            }
        }

        fn take(&self, envs: &[Vec<u8>], key: &GroupKey, members: &[PublicIdentity], now: u64) {
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
                &self.records.collections(),
                &self.keys,
                GROUP,
                key,
                |e| open_envelope(e, key, lookup),
                now,
            )
            .unwrap();
        }

        fn read(&self, col: usize, id: impl ToString) -> Option<Map<String, Value>> {
            self.records.collections()[col]
                .read(&self.conn, &id.to_string())
                .unwrap()
        }
    }

    const PROJECTS: usize = 0;
    const CHATS: usize = 1;
    const MESSAGES: usize = 2;

    /// Every byte the Node's database holds, as text, to look for what must not be there.
    fn dump(conn: &Connection) -> String {
        let mut out = String::new();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for t in tables {
            let mut stmt = conn.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
            let n = stmt.column_count();
            let mut rows = stmt.query([]).unwrap();
            while let Some(r) = rows.next().unwrap() {
                for i in 0..n {
                    match r.get_ref(i).unwrap() {
                        rusqlite::types::ValueRef::Text(b) | rusqlite::types::ValueRef::Blob(b) => {
                            out.push_str(&String::from_utf8_lossy(b));
                            out.push('\n');
                        }
                        _ => {}
                    }
                }
            }
        }
        out
    }

    fn device_and_node() -> (Dev, Node, GroupKey, Vec<PublicIdentity>) {
        let a = Dev::new();
        let node = Node::new();
        let members = vec![a.keys.public(), node.keys.public()];
        (a, node, GroupKey::generate(0).unwrap(), members)
    }

    #[test]
    fn the_node_keeps_what_it_applies_sealed_and_reads_it_back() {
        let (mut a, node, key, members) = device_and_node();
        let p = project::create_project(&a.conn, "Garden plans").unwrap();
        project::update_project(&a.conn, p.id, "Garden plans", "Answer as a gardener").unwrap();
        let c = chat::create_conversation_in(&a.conn, "Tomato blight", Some(p.id)).unwrap();
        let m =
            chat::add_message(&a.conn, c.id, NewMessage::user_text("Spots on the leaves")).unwrap();
        node.take(&a.send(&key, NOW), &key, &members, NOW);

        // The Node reads the same fields a device has.
        let pid = p.id.to_string();
        let want = |col: &dyn Collection, id: &str| col.read(&a.conn, id).unwrap();
        assert_eq!(node.read(PROJECTS, &pid), want(&records::Projects, &pid));
        assert_eq!(
            node.read(CHATS, c.id),
            want(&records::Chats, &c.id.to_string())
        );
        assert_eq!(
            node.read(MESSAGES, m.id),
            want(&records::Messages, &m.id.to_string())
        );
        // None of it is readable in the database, and dn-store's tables stay empty. (On the
        // desktop it is, which shows the check would see it.)
        assert!(dump(&a.conn).contains("Tomato blight"));
        let all = dump(&node.conn);
        for text in [
            "Garden plans",
            "Answer as a gardener",
            "Tomato blight",
            "Spots on the leaves",
        ] {
            assert!(!all.contains(text), "{text} is readable at rest");
        }
        assert!(chat::list_conversations(&node.conn).unwrap().is_empty());
        // Field hashes are keyed: the plain hash of a value isn't there.
        let plain = crate::envelope::change_hash(
            &serde_json::to_vec(&Value::String("Tomato blight".into())).unwrap(),
        );
        let hashes: Vec<Vec<u8>> = node
            .conn
            .prepare("SELECT hash FROM sync_fields")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(!hashes.is_empty());
        assert!(hashes.iter().all(|h| h[N_LEN..] != plain[..]));
        // The message's chat is the one readable link.
        let parent: Option<String> = node
            .conn
            .query_row(
                "SELECT parent FROM sync_node_records WHERE collection = 'message'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent, Some(c.id.to_string()));
    }

    #[test]
    fn the_version_not_shown_is_kept_sealed_on_the_node() {
        let mut a = Dev::new();
        let mut b = Dev::new();
        let node = Node::new();
        let members = vec![a.keys.public(), b.keys.public(), node.keys.public()];
        let key = GroupKey::generate(0).unwrap();
        let c = chat::create_conversation(&a.conn, "Plans").unwrap();
        let first = a.send(&key, NOW);
        node.take(&first, &key, &members, NOW);
        let lookup = |x: DeviceId, _: u32| {
            members
                .iter()
                .find(|m| m.id() == x)
                .copied()
                .ok_or(SyncError::UnknownDevice)
        };
        for e in &first {
            inbox::receive(&b.conn, &open_envelope(e, &key, lookup).unwrap(), NOW).unwrap();
        }
        pump(
            &b.conn,
            &store_collections(),
            &b.keys,
            GROUP,
            &key,
            |e| open_envelope(e, &key, lookup),
            NOW,
        )
        .unwrap();
        chat::rename_conversation(&a.conn, c.id, "Surprise party for Sam").unwrap();
        chat::rename_conversation(&b.conn, c.id, "Quarterly numbers").unwrap();
        let ea = a.send(&key, NOW + 1);
        let eb = b.send(&key, NOW + 2);
        node.take(&ea, &key, &members, NOW + 3);
        node.take(&eb, &key, &members, NOW + 3);

        let shown = node.read(CHATS, c.id).unwrap();
        assert_eq!(shown["title"], Value::String("Quarterly numbers".into()));
        let all = dump(&node.conn);
        assert!(!all.contains("Surprise party"));
        let (field, value, author, seq, hlc): (String, String, String, i64, i64) = node
            .conn
            .query_row(
                "SELECT field, value, author, seq, hlc FROM sync_conflicts",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        let id = c.id.to_string();
        let at = ConflictAt {
            id: &id,
            field: &field,
            author: DeviceId::from_hex(&author).unwrap(),
            seq: seq as u64,
            hlc: hlc as u64,
        };
        let chats = node.records.collections()[CHATS];
        assert_eq!(
            chats.open_conflict(&at, value.clone()).unwrap(),
            "\"Surprise party for Sam\""
        );
        // Bound to its place: the same sealed value under another change doesn't open.
        let elsewhere = ConflictAt {
            seq: at.seq + 1,
            ..at
        };
        assert!(chats.open_conflict(&elsewhere, value).is_err());
    }

    #[test]
    fn a_sealed_row_moved_to_another_record_does_not_open() {
        let (mut a, node, key, members) = device_and_node();
        let c1 = chat::create_conversation(&a.conn, "One").unwrap();
        let c2 = chat::create_conversation(&a.conn, "Two").unwrap();
        node.take(&a.send(&key, NOW), &key, &members, NOW);
        node.conn
            .execute(
                "UPDATE sync_node_records SET sealed =
                    (SELECT sealed FROM sync_node_records WHERE id = ?1)
                 WHERE id = ?2",
                params![c1.id.to_string(), c2.id.to_string()],
            )
            .unwrap();
        let err = node.records.collections()[CHATS]
            .read(&node.conn, &c2.id.to_string())
            .unwrap_err();
        assert!(err.to_string().contains("damaged"), "{err}");
    }

    #[test]
    fn an_older_copy_of_a_row_or_a_purged_one_put_back_does_not_open() {
        let (mut a, node, key, members) = device_and_node();
        let c = chat::create_conversation(&a.conn, "Before").unwrap();
        let m = chat::add_message(&a.conn, c.id, NewMessage::user_text("Hi")).unwrap();
        node.take(&a.send(&key, NOW), &key, &members, NOW);
        let saved = |collection: &str, id: String| -> Vec<u8> {
            node.conn
                .query_row(
                    "SELECT sealed FROM sync_node_records WHERE collection = ?1 AND id = ?2",
                    params![collection, id],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let old_chat = saved("chat", c.id.to_string());
        let old_message = saved("message", m.id.to_string());
        chat::rename_conversation(&a.conn, c.id, "After").unwrap();
        node.take(&a.send(&key, NOW + 1), &key, &members, NOW + 1);
        assert_eq!(node.read(CHATS, c.id).unwrap()["title"], "After");
        // The row from before the rename, put back under the same key: refused.
        node.conn
            .execute(
                "UPDATE sync_node_records SET sealed = ?1 WHERE collection = 'chat'",
                [&old_chat],
            )
            .unwrap();
        assert!(
            node.records.collections()[CHATS]
                .read(&node.conn, &c.id.to_string())
                .is_err()
        );
        // A purged message put back: refused too (its bookkeeping went with it).
        node.records.collections()[MESSAGES]
            .purge(&node.conn, &m.id.to_string(), 0)
            .unwrap();
        node.conn
            .execute("DELETE FROM sync_fields WHERE id = ?1", [m.id.to_string()])
            .unwrap();
        node.conn
            .execute(
                "INSERT INTO sync_node_records (collection, id, parent, key_n, sealed)
                 VALUES ('message', ?1, ?2, 1, ?3)",
                params![m.id.to_string(), c.id.to_string(), old_message],
            )
            .unwrap();
        assert!(
            node.records.collections()[MESSAGES]
                .read(&node.conn, &m.id.to_string())
                .is_err()
        );
    }

    #[test]
    fn a_rotation_that_stopped_before_deleting_is_completed_by_the_next_run() {
        let (mut a, node, key, members) = device_and_node();
        chat::create_conversation(&a.conn, "Plans").unwrap();
        node.take(&a.send(&key, NOW), &key, &members, NOW);
        let keys = node.records.keys();
        keys.rotate(&node.conn, &node.store).unwrap();
        while keys.reseal_batch(&node.conn, 10).unwrap() > 0 {}
        // It stopped after moving on, before deleting key 1.
        node.conn
            .execute("UPDATE sync_records_key SET oldest = 2 WHERE id = 1", [])
            .unwrap();
        let again = RecordsKeys::load(&node.conn, &node.store).unwrap();
        assert!(node.store.get(&key_id(1)).unwrap().is_some());
        // The next run starts key 3 and, when done, deletes every older key.
        again.rotate(&node.conn, &node.store).unwrap();
        while again.reseal_batch(&node.conn, 10).unwrap() > 0 {}
        assert!(again.finish(&node.conn, &node.store).unwrap());
        for n in [1, 2] {
            assert!(node.store.get(&key_id(n)).unwrap().is_none(), "key {n}");
        }
        assert!(node.store.get(&key_id(3)).unwrap().is_some());
    }

    #[test]
    fn purges_on_the_node_match_a_device() {
        let (mut a, node, key, members) = device_and_node();
        let p = project::create_project(&a.conn, "Trip").unwrap();
        let c = chat::create_conversation_in(&a.conn, "Plans", Some(p.id)).unwrap();
        let other = chat::create_conversation(&a.conn, "Other").unwrap();
        let m = chat::add_message(&a.conn, other.id, NewMessage::user_text("Hi")).unwrap();
        node.take(&a.send(&key, NOW), &key, &members, NOW);
        let chats = node.records.collections()[CHATS];
        let projects = node.records.collections()[PROJECTS];
        assert_eq!(
            chats.children(&node.conn, &other.id.to_string()).unwrap(),
            [("message", m.id.to_string())]
        );
        // A project's purge leaves its chats, out of the project.
        projects.purge(&node.conn, &p.id.to_string(), 0).unwrap();
        assert!(node.read(PROJECTS, p.id).is_none());
        assert_eq!(node.read(CHATS, c.id).unwrap()["project_id"], Value::Null);
        // A chat's purge takes its messages.
        chats.purge(&node.conn, &other.id.to_string(), 0).unwrap();
        assert!(node.read(CHATS, other.id).is_none());
        assert!(node.read(MESSAGES, m.id).is_none());
        // A message for a chat that isn't here is refused, as on a device.
        let mut fields = records::Messages
            .read(&a.conn, &m.id.to_string())
            .unwrap()
            .unwrap();
        fields.insert(
            "conversation_id".into(),
            Value::String(other.id.to_string()),
        );
        assert!(
            node.records.collections()[MESSAGES]
                .write(
                    &node.conn,
                    &m.id.to_string(),
                    &fields,
                    WriteBy {
                        author: a.keys.id(),
                        seq: 99,
                        hlc: crate::hlc::Hlc::from_parts(NOW, 0),
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn a_missing_key_is_an_error_and_never_replaced() {
        let (mut a, node, key, members) = device_and_node();
        chat::create_conversation(&a.conn, "Plans").unwrap();
        node.take(&a.send(&key, NOW), &key, &members, NOW);
        let empty = MemoryStore::new();
        let err = RecordsKeys::load(&node.conn, &empty).err().unwrap();
        assert!(err.to_string().contains("records.1"), "{err}");
        assert!(empty.get(&key_id(1)).unwrap().is_none(), "no new key made");
        // The right store opens them.
        assert!(RecordsKeys::load(&node.conn, &node.store).is_ok());
    }

    #[test]
    fn rotation_reseals_everything_then_drops_the_old_key() {
        let mut a = Dev::new();
        let mut b = Dev::new();
        let node = Node::new();
        let members = vec![a.keys.public(), b.keys.public(), node.keys.public()];
        let key = GroupKey::generate(0).unwrap();
        let c = chat::create_conversation(&a.conn, "Plans").unwrap();
        chat::add_message(&a.conn, c.id, NewMessage::user_text("One")).unwrap();
        chat::add_message(&a.conn, c.id, NewMessage::user_text("Two")).unwrap();
        let first = a.send(&key, NOW);
        node.take(&first, &key, &members, NOW);
        // A conflict, so rotation covers conflicts too.
        let lookup = |x: DeviceId, _: u32| {
            members
                .iter()
                .find(|m| m.id() == x)
                .copied()
                .ok_or(SyncError::UnknownDevice)
        };
        for e in &first {
            inbox::receive(&b.conn, &open_envelope(e, &key, lookup).unwrap(), NOW).unwrap();
        }
        pump(
            &b.conn,
            &store_collections(),
            &b.keys,
            GROUP,
            &key,
            |e| open_envelope(e, &key, lookup),
            NOW,
        )
        .unwrap();
        chat::rename_conversation(&a.conn, c.id, "A").unwrap();
        chat::rename_conversation(&b.conn, c.id, "B").unwrap();
        let (ea, eb) = (a.send(&key, NOW + 1), b.send(&key, NOW + 2));
        node.take(&ea, &key, &members, NOW + 3);
        node.take(&eb, &key, &members, NOW + 3);
        let before = node.read(CHATS, c.id).unwrap();

        let keys = node.records.keys();
        let r = keys.rotate(&node.conn, &node.store).unwrap();
        assert_eq!(
            r,
            Rotation {
                current: 2,
                left: 4
            }
        ); // chat, two messages, one conflict
        // Not finished while something uses key 1; asking again doesn't start another.
        assert!(!keys.finish(&node.conn, &node.store).unwrap());
        assert_eq!(keys.rotate(&node.conn, &node.store).unwrap().current, 2);
        // New writes use key 2 straight away; old rows still read.
        assert_eq!(node.read(CHATS, c.id).unwrap(), before);
        let mut left = 4;
        while left > 0 {
            let now = keys.reseal_batch(&node.conn, 1).unwrap();
            assert!(now < left);
            left = now;
        }
        assert!(keys.finish(&node.conn, &node.store).unwrap());
        assert!(node.store.get(&key_id(1)).unwrap().is_none());
        assert!(node.store.get(&key_id(2)).unwrap().is_some());
        // A fresh load, as at the next start, reads everything with key 2 alone.
        let again = NodeRecords::new(Arc::new(
            RecordsKeys::load(&node.conn, &node.store).unwrap(),
        ));
        assert_eq!(again.keys().current(), 2);
        assert_eq!(
            again.collections()[CHATS]
                .read(&node.conn, &c.id.to_string())
                .unwrap(),
            Some(before)
        );
        let n1: i64 = node
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sync_node_records WHERE key_n = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n1, 0);
    }

    #[test]
    fn nothing_readable_reaches_the_database_file_or_its_wal() {
        let dir = std::env::temp_dir().join(format!("dn-sync-at-rest-{}", dn_core::Id::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("node.db");
        let (mut a, _, key, _) = device_and_node();
        {
            let conn =
                dn_store::open(&path, &[MigrationSet::new("sync", crate::migrations())]).unwrap();
            let node = Node::on(conn, MemoryStore::new());
            let members = vec![a.keys.public(), node.keys.public()];
            let c = chat::create_conversation(&a.conn, "Heirloom seed swap").unwrap();
            chat::add_message(
                &a.conn,
                c.id,
                NewMessage::user_text("Brandywine for Cherokee Purple"),
            )
            .unwrap();
            node.take(&a.send(&key, NOW), &key, &members, NOW);
            assert!(node.read(CHATS, c.id).is_some());
            // Bytes in the WAL count too: look before and after the checkpoint.
            for f in [path.clone(), dir.join("node.db-wal")] {
                if let Ok(bytes) = std::fs::read(&f) {
                    let text = String::from_utf8_lossy(&bytes);
                    assert!(!text.contains("Heirloom"), "{}", f.display());
                    assert!(!text.contains("Brandywine"), "{}", f.display());
                }
            }
        }
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("Heirloom") && !text.contains("Brandywine"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
