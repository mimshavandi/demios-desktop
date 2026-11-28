//! A sync round between a paired device and its Node, on the direct route (docs/design/
//! dn-sync.md §9, §14 step 4).
//!
//! Every envelope made or received is kept in `sync_envelopes` by (author, seq). A device
//! and the Node each say what they hold ([`held`]), and each side sends what the other
//! lacks. The Node keeps envelopes to pass on; it holds no records and applies nothing.
//!
//! One round, driven by the device ([`run`]):
//! 1. `SyncStart`: the device says how much of the membership chain it has and what it
//!    holds. The Node answers with the entries it lacks and what the Node holds.
//! 2. The device checks the chain (signed by the Node pinned at pairing, no fork) and stops
//!    if it was removed.
//! 3. It records local edits as changes, in batches.
//! 4. `Push`: it sends the envelopes the Node lacks, about [`BATCH_BYTES`] at a time. The
//!    Node opens each with the group key of its epoch, checks the signature against the
//!    chain and that the author may write, and keeps it.
//! 5. `Pull`: it asks for what it lacks until nothing more comes, keeps each envelope in
//!    the inbox, and applies the ready ones in batches.
//!
//! Snapshots and compaction (#294, [`crate::snapshot`]):
//! - The Node applies what it keeps, so it holds the records too.
//! - `SyncStart` says what the device has applied. The Node records it, drops envelopes
//!   every current member has applied, and says where its log now starts (`floor`).
//! - A device that lacks changes below the floor, after its push, fetches the Node's
//!   snapshot in chunks and installs it, then pulls what came after it.
//!
//! The device lets go of the database between batches, so a first sync of a long history
//! doesn't block the app. Any failure ends the round with an error: nothing is retried here.

use crate::b64;
use crate::change::Vector;
use crate::envelope::{self, GroupKey, Opened};
use crate::group::{ClientGroup, NodeGroup};
use crate::identity::{DeviceId, DeviceKeys, PublicIdentity};
use crate::inbox::{self, Received};
use crate::membership::{Chain, Entry};
use crate::records::{self, Collection};
use crate::snapshot;
use crate::transport::{ChainEntry, Channel, Message};
use crate::{Result, SyncError};
use dn_secrets::SecretStore;
use rusqlite::{Connection, params};
use std::collections::BTreeMap;
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncWrite};

pub(crate) const MIGRATION: &str = "
-- Every envelope made here or received, to pass on to devices that lack it.
CREATE TABLE sync_envelopes (
    author   TEXT    NOT NULL,
    seq      INTEGER NOT NULL,
    envelope BLOB    NOT NULL,
    PRIMARY KEY (author, seq)
) WITHOUT ROWID;
";

/// About how many envelope bytes one `Push` or `Envelopes` message carries (at least one
/// envelope, which is at most about 1 MiB, so a frame stays under the sync frame limit).
pub const BATCH_BYTES: usize = 1024 * 1024;

/// Most envelopes one `Push` may carry.
pub const MAX_PUSH: usize = 4096;

/// How many ready changes the device applies before letting go of the database.
const APPLY_BATCH: usize = 200;

fn lock(db: &Mutex<Connection>) -> Result<std::sync::MutexGuard<'_, Connection>> {
    db.lock()
        .map_err(|_| SyncError::Store("the database lock was poisoned".into()))
}

fn device(text: &str) -> Result<DeviceId> {
    DeviceId::from_hex(text).ok_or(SyncError::Protocol("bad device id"))
}

fn to_wire(v: &Vector) -> BTreeMap<String, u64> {
    snapshot::vector_to_wire(v)
}

fn from_wire(v: &BTreeMap<String, u64>) -> Result<Vector> {
    snapshot::vector_from_wire(v)
}

/// For each author, the highest change this side holds (kept or applied).
pub fn held(conn: &Connection) -> Result<Vector> {
    let mut v = inbox::vector(conn)?;
    let mut stmt = conn.prepare("SELECT author, MAX(seq) FROM sync_envelopes GROUP BY author")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (a, s) = row?;
        let a = device(&a)?;
        let s = u64::try_from(s).unwrap_or(0);
        if s > v.get(a) {
            v.set(a, s);
        }
    }
    Ok(v)
}

/// One envelope held here: (author, seq, sealed bytes).
pub type Held = (DeviceId, u64, Vec<u8>);

/// Envelopes held here that `theirs` lacks, per author in order, about `max_bytes` of
/// them (at least one). `true` when more remain.
pub fn envelopes_after(
    conn: &Connection,
    theirs: &Vector,
    max_bytes: usize,
) -> Result<(Vec<Held>, bool)> {
    let authors: Vec<String> = conn
        .prepare("SELECT DISTINCT author FROM sync_envelopes ORDER BY author")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    let mut bytes = 0usize;
    let mut stmt = conn.prepare(
        "SELECT seq, envelope FROM sync_envelopes WHERE author = ?1 AND seq > ?2 ORDER BY seq",
    )?;
    for a in authors {
        let id = device(&a)?;
        let after = i64::try_from(theirs.get(id)).unwrap_or(i64::MAX);
        let mut rows = stmt.query(params![a, after])?;
        while let Some(row) = rows.next()? {
            let env: Vec<u8> = row.get(1)?;
            if !out.is_empty() && bytes + env.len() > max_bytes {
                return Ok((out, true));
            }
            bytes += env.len();
            out.push((id, u64::try_from(row.get::<_, i64>(0)?).unwrap_or(0), env));
        }
    }
    Ok((out, false))
}

fn entry_to_wire(e: &Entry) -> ChainEntry {
    ChainEntry {
        bytes: b64::encode(&e.bytes),
        signature: b64::encode(&e.signature),
    }
}

fn entry_from_wire(e: &ChainEntry) -> Result<Entry> {
    let bytes = b64::decode(&e.bytes).ok_or(SyncError::Protocol("bad chain entry"))?;
    let sig = b64::decode(&e.signature).ok_or(SyncError::Protocol("bad chain entry"))?;
    let signature: [u8; 64] = sig
        .try_into()
        .map_err(|_| SyncError::Protocol("bad chain entry"))?;
    Ok(Entry { bytes, signature })
}

fn open_with(
    chain: &Chain,
    env: &[u8],
    key: impl FnOnce(u32) -> Result<GroupKey>,
) -> Result<Opened> {
    let head = envelope::header(env)?;
    if head.group != chain.group() {
        return Err(SyncError::Protocol("an envelope from another group"));
    }
    let key = key(head.epoch)?;
    envelope::open(env, &key, |author, _| chain.author_key(author))
}

/// Why a minor-0 device behind the Node's log is refused.
pub const OLD_DEVICE: &str = "update Almend on this device to sync: your Node no longer keeps \
     the older changes this device lacks, and this version can't start from a snapshot";

/// The Node's side of sync messages from a checked member (after
/// [`crate::transport::authorize_member`]).
pub struct NodeSync<'a> {
    pub db: &'a Mutex<Connection>,
    pub store: &'a dyn SecretStore,
    pub group: &'a Mutex<NodeGroup>,
    /// The Node's own keys: it signs snapshots.
    pub me: &'a DeviceKeys,
    /// The collections it applies, the same as the devices'.
    pub collections: &'a [&'a dyn Collection],
    /// The device's protocol minor ([`Channel::peer_minor`]): a minor-0 device can't start
    /// from a snapshot, so one behind the log is refused with a clear message.
    pub peer_minor: u16,
}

impl NodeSync<'_> {
    /// Whether `message` is one [`NodeSync::answer`] handles.
    pub fn handles(message: &Message) -> bool {
        matches!(
            message,
            Message::SyncStart { .. }
                | Message::Push { .. }
                | Message::Pull { .. }
                | Message::SnapshotRequest
                | Message::SnapshotPull { .. }
        )
    }

    /// Applies what the Node keeps and hasn't applied: envelopes kept before the Node held
    /// records, and any that wait. Call at start.
    pub fn catch_up(&self, now_ms: u64) -> Result<usize> {
        let group = self
            .group
            .lock()
            .map_err(|_| SyncError::Store("the group lock was poisoned".into()))?;
        let conn = lock(self.db)?;
        let chain = group.chain();
        inbox::queue_kept(
            &conn,
            |env| open_with(chain, env, |e| group.key(self.store, e)),
            now_ms,
        )?;
        self.apply(&conn, &group, now_ms)
    }

    /// Applies every ready change (the Node makes no edits of its own).
    fn apply(&self, conn: &Connection, group: &NodeGroup, now_ms: u64) -> Result<usize> {
        let chain = group.chain();
        let current = group.key(self.store, chain.epoch())?;
        let done = records::pump(
            conn,
            self.collections,
            self.me,
            chain.group(),
            &current,
            |env| open_with(chain, env, |e| group.key(self.store, e)),
            now_ms,
        )?;
        Ok(done.applied)
    }

    /// The answer to one sync message from `from`.
    pub fn answer(&self, from: DeviceId, message: Message, now_ms: u64) -> Result<Message> {
        let group = self
            .group
            .lock()
            .map_err(|_| SyncError::Store("the group lock was poisoned".into()))?;
        let conn = lock(self.db)?;
        let chain = group.chain();
        if !chain.member(from).is_some_and(|m| m.removed.is_none()) {
            return Err(SyncError::Removed);
        }
        match message {
            Message::SyncStart {
                chain_len,
                vector,
                applied,
            } => {
                if self.peer_minor >= 1 {
                    snapshot::record_ack(&conn, from, &from_wire(&applied)?, now_ms)?;
                }
                let floor = snapshot::compact(&conn, chain, now_ms)?;
                if self.peer_minor < 1 && snapshot::behind(&from_wire(&vector)?, &floor) {
                    return Err(SyncError::Refused(OLD_DEVICE.into()));
                }
                let have = usize::try_from(chain_len).unwrap_or(usize::MAX);
                let entries = chain
                    .entries()
                    .get(have..)
                    .ok_or(SyncError::ForkedChain)?
                    .iter()
                    .map(entry_to_wire)
                    .collect();
                Ok(Message::SyncState {
                    entries,
                    vector: to_wire(&held(&conn)?),
                    floor: to_wire(&floor),
                })
            }
            Message::Push { envelopes } => {
                if envelopes.len() > MAX_PUSH {
                    return Err(SyncError::Protocol("too many envelopes in one push"));
                }
                let mut stored = 0u32;
                for text in envelopes {
                    let env = b64::decode(&text).ok_or(SyncError::Protocol("bad envelope"))?;
                    let opened = open_with(chain, &env, |e| group.key(self.store, e))?;
                    if inbox::receive_from_group(&conn, chain, &opened, now_ms)? == Received::Stored
                    {
                        stored += 1;
                    }
                }
                self.apply(&conn, &group, now_ms)?;
                Ok(Message::Pushed { stored })
            }
            Message::Pull { vector } => {
                let (envs, more) = envelopes_after(&conn, &from_wire(&vector)?, BATCH_BYTES)?;
                Ok(Message::Envelopes {
                    envelopes: envs.iter().map(|(_, _, e)| b64::encode(e)).collect(),
                    more,
                })
            }
            Message::SnapshotRequest => {
                let key = group.key(self.store, chain.epoch())?;
                let (vector, size) = snapshot::current(
                    &conn,
                    self.collections,
                    self.me,
                    chain.group(),
                    &key,
                    now_ms,
                )?;
                Ok(Message::Snapshot {
                    vector: to_wire(&vector),
                    size,
                })
            }
            Message::SnapshotPull { vector, offset } => {
                // Empty and no more: the snapshot was replaced; the device starts over.
                let (data, more) =
                    snapshot::chunk(&conn, &from_wire(&vector)?, offset)?.unwrap_or_default();
                Ok(Message::SnapshotChunk {
                    data: b64::encode(&data),
                    more,
                })
            }
            _ => Err(SyncError::Protocol("not a sync message")),
        }
    }
}

/// What one round did, for Settings › Node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Local edits recorded as changes.
    pub recorded: usize,
    /// Envelopes the Node didn't have.
    pub pushed: usize,
    /// Envelopes received from the Node.
    pub received: usize,
    /// Changes from other devices applied here.
    pub applied: usize,
    /// Records another device deleted for good, as (collection, id).
    pub purged: Vec<(String, String)>,
    /// This device started from the Node's snapshot (it was behind the Node's log).
    pub snapshot: bool,
    /// Envelopes this device dropped from its log, because every member has them.
    pub compacted: usize,
}

/// What a device needs for a round.
pub struct Device<'a> {
    pub db: &'a Mutex<Connection>,
    pub store: &'a dyn SecretStore,
    pub me: &'a DeviceKeys,
    /// The Node pinned at pairing.
    pub node: &'a PublicIdentity,
    pub collections: &'a [&'a dyn Collection],
    pub now_ms: &'a (dyn Fn() -> u64 + Sync),
}

impl Device<'_> {
    /// Records local edits and applies ready changes, in batches, letting go of the
    /// database in between.
    async fn pump(&self, group: &ClientGroup, report: &mut Report) -> Result<()> {
        loop {
            let b = {
                let conn = lock(self.db)?;
                let chain = group.chain();
                let current = group.key(self.store, self.me, chain.epoch())?;
                records::pump_batch(
                    &conn,
                    self.collections,
                    self.me,
                    chain.group(),
                    &current,
                    |env| open_with(chain, env, |e| group.key(self.store, self.me, e)),
                    APPLY_BATCH,
                    (self.now_ms)(),
                )?
            };
            report.recorded += b.local.len();
            report.applied += b.applied;
            report.purged.extend(b.purged);
            if !b.more {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
    }
}

async fn ask<S: AsyncRead + AsyncWrite + Unpin>(
    ch: &mut Channel<S>,
    message: &Message,
) -> Result<Message> {
    ch.send(message).await?;
    match ch.recv().await? {
        Message::Refused { reason } => Err(SyncError::Refused(reason)),
        other => Ok(other),
    }
}

/// One sync round with the Node on `ch` (connected with [`crate::transport::connect`]).
pub async fn run<S: AsyncRead + AsyncWrite + Unpin>(
    ch: &mut Channel<S>,
    dev: &Device<'_>,
) -> Result<Report> {
    ch.allow_sync_frames();
    let mut report = Report::default();

    // 1–2. The chain, checked, and what the Node holds.
    let (chain_len, mine, applied) = {
        let conn = lock(dev.db)?;
        let len = ClientGroup::load(&conn, dev.node)?.map_or(0, |g| g.chain().entries().len());
        (len as u64, held(&conn)?, inbox::vector(&conn)?)
    };
    let Message::SyncState {
        entries,
        vector,
        floor,
    } = ask(
        ch,
        &Message::SyncStart {
            chain_len,
            vector: to_wire(&mine),
            applied: to_wire(&applied),
        },
    )
    .await?
    else {
        return Err(SyncError::Protocol("expected the sync state"));
    };
    let entries = entries
        .iter()
        .map(entry_from_wire)
        .collect::<Result<Vec<_>>>()?;
    let group = {
        let conn = lock(dev.db)?;
        match ClientGroup::load(&conn, dev.node)? {
            Some(mut g) => {
                g.extend(&conn, entries)?;
                g
            }
            None => ClientGroup::from_entries(&conn, dev.node, &entries)?,
        }
    };
    if !group.is_member(dev.me.id()) {
        return Err(SyncError::Removed);
    }

    // 3. Local edits.
    dev.pump(&group, &mut report).await?;

    // 4. What the Node lacks.
    let mut theirs = from_wire(&vector)?;
    loop {
        let (batch, more) = {
            let conn = lock(dev.db)?;
            envelopes_after(&conn, &theirs, BATCH_BYTES)?
        };
        if batch.is_empty() {
            break;
        }
        let Message::Pushed { .. } = ask(
            ch,
            &Message::Push {
                envelopes: batch.iter().map(|(_, _, e)| b64::encode(e)).collect(),
            },
        )
        .await?
        else {
            return Err(SyncError::Protocol("expected the push answer"));
        };
        report.pushed += batch.len();
        for (a, s, _) in &batch {
            if *s > theirs.get(*a) {
                theirs.set(*a, *s);
            }
        }
        if !more {
            break;
        }
    }

    // 5. Behind the Node's log: start from its snapshot.
    let floor = from_wire(&floor)?;
    let behind = {
        let conn = lock(dev.db)?;
        snapshot::behind(&held(&conn)?, &floor)
    };
    if behind {
        let purged = fetch_snapshot(ch, dev, &group).await?;
        report.purged.extend(purged);
        report.snapshot = true;
    }

    // 6. What this device lacks.
    loop {
        let mine = {
            let conn = lock(dev.db)?;
            held(&conn)?
        };
        let Message::Envelopes { envelopes, more } = ask(
            ch,
            &Message::Pull {
                vector: to_wire(&mine),
            },
        )
        .await?
        else {
            return Err(SyncError::Protocol("expected envelopes"));
        };
        if envelopes.is_empty() && more {
            return Err(SyncError::Protocol("an empty batch that says more follow"));
        }
        {
            let conn = lock(dev.db)?;
            let chain = group.chain();
            for text in &envelopes {
                let env = b64::decode(text).ok_or(SyncError::Protocol("bad envelope"))?;
                let opened = open_with(chain, &env, |e| group.key(dev.store, dev.me, e))?;
                inbox::receive_from_group(&conn, chain, &opened, (dev.now_ms)())?;
                report.received += 1;
            }
        }
        dev.pump(&group, &mut report).await?;
        if !more {
            break;
        }
    }
    {
        let conn = lock(dev.db)?;
        report.compacted = snapshot::drop_covered(&conn, &floor, Some(dev.me.id()))?;
    }
    ch.send(&Message::Done).await?;
    Ok(report)
}

/// Downloads the Node's snapshot in chunks and installs it (see [`crate::snapshot`]).
async fn fetch_snapshot<S: AsyncRead + AsyncWrite + Unpin>(
    ch: &mut Channel<S>,
    dev: &Device<'_>,
    group: &ClientGroup,
) -> Result<Vec<(String, String)>> {
    let Message::Snapshot { vector, size } = ask(ch, &Message::SnapshotRequest).await? else {
        return Err(SyncError::Protocol("expected the snapshot"));
    };
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    if size > snapshot::MAX_SNAPSHOT_BYTES {
        return Err(SyncError::Protocol("the snapshot is too large"));
    }
    let mut sealed = Vec::with_capacity(size);
    loop {
        let Message::SnapshotChunk { data, more } = ask(
            ch,
            &Message::SnapshotPull {
                vector: vector.clone(),
                offset: sealed.len() as u64,
            },
        )
        .await?
        else {
            return Err(SyncError::Protocol("expected a snapshot chunk"));
        };
        let data = b64::decode(&data).ok_or(SyncError::Protocol("a bad snapshot chunk"))?;
        if data.is_empty() && sealed.len() < size {
            return Err(SyncError::NotYet(
                "the Node's snapshot changed while it downloaded",
            ));
        }
        if data.len() > snapshot::CHUNK_BYTES || sealed.len() + data.len() > size {
            return Err(SyncError::Protocol("a snapshot chunk past its size"));
        }
        sealed.extend_from_slice(&data);
        if !more {
            break;
        }
        tokio::task::yield_now().await;
    }
    if sealed.len() != size {
        return Err(SyncError::Protocol("a snapshot shorter than its size"));
    }
    let conn = lock(dev.db)?;
    let chain = group.chain();
    let key = group.key(dev.store, dev.me, crate::envelope::header(&sealed)?.epoch)?;
    let done = snapshot::install(
        &conn,
        dev.collections,
        &sealed,
        &key,
        dev.node,
        chain.group(),
        (dev.now_ms)(),
    )?;
    Ok(done.purged)
}

/// Ends sync on a device that leaves its Node (unpair, forget, Delete all): deletes the
/// group keys this device holds for `node`, and everything sync kept, so nothing of the
/// old group is sent or applied again. The records themselves stay.
pub fn reset(conn: &Connection, store: &dyn SecretStore, node: &PublicIdentity) -> Result<()> {
    if let Some(chain) = crate::membership::load(conn, node)? {
        for epoch in 0..=chain.epoch() {
            crate::membership::delete_key(store, &chain.group(), epoch)?;
        }
    }
    conn.execute_batch(
        "SAVEPOINT sync_reset;
         DELETE FROM sync_vector; DELETE FROM sync_seen; DELETE FROM sync_inbox;
         DELETE FROM sync_held_authors; DELETE FROM sync_clock; DELETE FROM sync_fields;
         DELETE FROM sync_conflicts; DELETE FROM sync_purged; DELETE FROM sync_scan;
         DELETE FROM sync_outbox; DELETE FROM sync_notices; DELETE FROM sync_envelopes;
         DELETE FROM sync_membership;
         RELEASE sync_reset;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::DeviceKind;
    use crate::records::store_collections;
    use crate::transport::{accept, connect};
    use dn_secrets::MemoryStore;
    use dn_store::MigrationSet;
    use dn_store::chat::{self, NewMessage};
    use dn_store::project;

    const NOW: u64 = 1_800_000_000_000;

    fn now() -> u64 {
        NOW
    }

    struct Node {
        keys: DeviceKeys,
        db: Mutex<Connection>,
        store: MemoryStore,
        group: Mutex<NodeGroup>,
        /// Sealed at rest, as the real Node keeps them (#335).
        records: crate::at_rest::NodeRecords,
    }

    impl Node {
        fn new() -> Self {
            let keys = DeviceKeys::generate().unwrap();
            let conn = dn_store::open_in_memory(&[MigrationSet::new("sync", crate::migrations())])
                .unwrap();
            let store = MemoryStore::new();
            let group = NodeGroup::load_or_create(&conn, &store, &keys, "Home server").unwrap();
            let records = crate::at_rest::NodeRecords::new(std::sync::Arc::new(
                crate::at_rest::RecordsKeys::load(&conn, &store).unwrap(),
            ));
            Self {
                keys,
                db: Mutex::new(conn),
                store,
                group: Mutex::new(group),
                records,
            }
        }

        fn add(&self, dev: &Dev) {
            let conn = self.db.lock().unwrap();
            self.group
                .lock()
                .unwrap()
                .add(
                    &conn,
                    &self.store,
                    &self.keys,
                    &dev.keys.public(),
                    "Device",
                    DeviceKind::Desktop,
                    None,
                )
                .unwrap();
        }

        fn remove(&self, dev: &Dev) {
            let conn = self.db.lock().unwrap();
            self.group
                .lock()
                .unwrap()
                .remove(
                    &conn,
                    &self.store,
                    &self.keys,
                    dev.keys.id(),
                    self.keys.id(),
                )
                .unwrap();
        }

        /// One connection: answers sync messages until `Done`, or refuses.
        async fn serve(&self, io: tokio::io::DuplexStream, from: DeviceId) {
            let Ok(mut ch) = accept(io, &self.keys).await else {
                return;
            };
            ch.allow_sync_frames();
            let cols = self.records.collections();
            let sync = NodeSync {
                db: &self.db,
                store: &self.store,
                group: &self.group,
                me: &self.keys,
                collections: &cols,
                peer_minor: ch.peer_minor(),
            };
            while let Ok(m) = ch.recv().await {
                if m == Message::Done {
                    return;
                }
                let reply = sync
                    .answer(from, m, NOW)
                    .unwrap_or_else(|e| Message::Refused {
                        reason: e.to_string(),
                    });
                let refused = matches!(reply, Message::Refused { .. });
                if ch.send(&reply).await.is_err() || refused {
                    return;
                }
            }
        }
    }

    struct Dev {
        keys: DeviceKeys,
        db: Mutex<Connection>,
        store: MemoryStore,
    }

    impl Dev {
        fn new() -> Self {
            let conn = dn_store::open_in_memory(&[MigrationSet::new("sync", crate::migrations())])
                .unwrap();
            dn_store::sync_rows::enable(&conn).unwrap();
            Self {
                keys: DeviceKeys::generate().unwrap(),
                db: Mutex::new(conn),
                store: MemoryStore::new(),
            }
        }

        async fn sync(&self, node: &Node) -> Result<Report> {
            let (a, b) = tokio::io::duplex(64 * 1024);
            let cols = store_collections();
            let node_id = node.keys.public();
            let dev = Device {
                db: &self.db,
                store: &self.store,
                me: &self.keys,
                node: &node_id,
                collections: &cols,
                now_ms: &now,
            };
            let node_key = node.keys.public().to_bytes();
            let mut ed = [0u8; 32];
            ed.copy_from_slice(&node_key[..32]);
            let client = async {
                let mut ch = connect(a, &self.keys, &ed).await?;
                run(&mut ch, &dev).await
            };
            let (r, ()) = tokio::join!(client, node.serve(b, self.keys.id()));
            r
        }

        fn titles(&self) -> Vec<String> {
            let conn = self.db.lock().unwrap();
            let mut v: Vec<_> = chat::list_conversations(&conn)
                .unwrap()
                .into_iter()
                .map(|c| c.title)
                .collect();
            v.sort();
            v
        }
    }

    #[tokio::test]
    async fn edits_reach_every_device_through_the_node_and_nothing_comes_back_twice() {
        let node = Node::new();
        let (laptop, phone) = (Dev::new(), Dev::new());
        node.add(&laptop);
        node.add(&phone);
        let chat_id = {
            let conn = laptop.db.lock().unwrap();
            let p = project::create_project(&conn, "Trip").unwrap();
            let c = chat::create_conversation_in(&conn, "Plans", Some(p.id)).unwrap();
            chat::add_message(&conn, c.id, NewMessage::user_text("Book a hotel")).unwrap();
            c.id
        };
        let r = laptop.sync(&node).await.unwrap();
        assert_eq!((r.recorded > 0, r.pushed > 0, r.received), (true, true, 0));
        let r = phone.sync(&node).await.unwrap();
        assert!(r.received > 0 && r.applied == r.received);
        assert_eq!(phone.titles(), ["Plans"]);
        {
            let conn = phone.db.lock().unwrap();
            assert_eq!(chat::list_messages(&conn, chat_id).unwrap().len(), 1);
            chat::rename_conversation(&conn, chat_id, "Plans for May").unwrap();
        }
        phone.sync(&node).await.unwrap();
        let r = laptop.sync(&node).await.unwrap();
        assert_eq!(laptop.titles(), ["Plans for May"]);
        assert_eq!(
            r.recorded, 0,
            "a change applied here isn't sent back as a new one"
        );
        // A second round with nothing new moves nothing.
        let r = laptop.sync(&node).await.unwrap();
        assert_eq!(r, Report::default());
    }

    #[tokio::test]
    async fn a_long_history_syncs_in_batches() {
        let node = Node::new();
        let (laptop, phone) = (Dev::new(), Dev::new());
        node.add(&laptop);
        node.add(&phone);
        {
            let conn = laptop.db.lock().unwrap();
            for i in 0..30 {
                let c = chat::create_conversation(&conn, &format!("Chat {i}")).unwrap();
                for j in 0..40 {
                    let text = format!("{j} {}", "x".repeat(2_000));
                    chat::add_message(&conn, c.id, NewMessage::user_text(text)).unwrap();
                }
            }
        }
        let r = laptop.sync(&node).await.unwrap();
        assert!(r.recorded > 1, "recorded in more than one change");
        let r = phone.sync(&node).await.unwrap();
        assert_eq!(r.applied, r.received);
        assert_eq!(phone.titles().len(), 30);
        let conn = phone.db.lock().unwrap();
        let total: usize = chat::list_conversations(&conn)
            .unwrap()
            .iter()
            .map(|c| chat::list_messages(&conn, c.id).unwrap().len())
            .sum();
        assert_eq!(total, 1_200);
    }

    #[tokio::test]
    async fn a_removed_device_is_refused_and_a_device_added_later_reads_the_whole_history() {
        let node = Node::new();
        let (laptop, phone, tablet) = (Dev::new(), Dev::new(), Dev::new());
        node.add(&laptop);
        node.add(&phone);
        {
            let conn = laptop.db.lock().unwrap();
            chat::create_conversation(&conn, "Before").unwrap();
        }
        laptop.sync(&node).await.unwrap();
        phone.sync(&node).await.unwrap();
        node.remove(&phone);
        // The phone can't sync any more, and can't push what it wrote since.
        {
            let conn = phone.db.lock().unwrap();
            chat::create_conversation(&conn, "From the removed phone").unwrap();
        }
        let err = phone.sync(&node).await.unwrap_err();
        assert!(matches!(err, SyncError::Refused(_)), "{err}");
        // The laptop moves to the new epoch.
        {
            let conn = laptop.db.lock().unwrap();
            chat::create_conversation(&conn, "After").unwrap();
        }
        laptop.sync(&node).await.unwrap();
        // A device added now reads changes sealed under both epochs.
        node.add(&tablet);
        tablet.sync(&node).await.unwrap();
        assert_eq!(tablet.titles(), ["After", "Before"]);
    }

    #[tokio::test]
    async fn the_node_refuses_replays_strangers_and_other_groups() {
        let node = Node::new();
        let other = Node::new();
        let (laptop, stranger) = (Dev::new(), Dev::new());
        node.add(&laptop);
        other.add(&stranger);
        {
            let conn = laptop.db.lock().unwrap();
            chat::create_conversation(&conn, "Mine").unwrap();
        }
        laptop.sync(&node).await.unwrap();
        let cols = node.records.collections();
        let sync = NodeSync {
            db: &node.db,
            store: &node.store,
            group: &node.group,
            me: &node.keys,
            collections: &cols,
            peer_minor: crate::transport::PROTOCOL_MINOR,
        };
        let envs: Vec<String> = {
            let conn = laptop.db.lock().unwrap();
            envelopes_after(&conn, &Vector::default(), BATCH_BYTES)
                .unwrap()
                .0
                .iter()
                .map(|(_, _, e)| b64::encode(e))
                .collect()
        };
        // A replay is kept once.
        let again = sync
            .answer(
                laptop.keys.id(),
                Message::Push {
                    envelopes: envs.clone(),
                },
                NOW,
            )
            .unwrap();
        assert_eq!(again, Message::Pushed { stored: 0 });
        // A device that isn't a member is refused.
        assert!(matches!(
            sync.answer(
                stranger.keys.id(),
                Message::Pull {
                    vector: BTreeMap::new()
                },
                NOW
            ),
            Err(SyncError::Removed)
        ));
        // An envelope of another group is refused.
        {
            let conn = stranger.db.lock().unwrap();
            chat::create_conversation(&conn, "Elsewhere").unwrap();
        }
        stranger.sync(&other).await.unwrap();
        let foreign: Vec<String> = {
            let conn = stranger.db.lock().unwrap();
            envelopes_after(&conn, &Vector::default(), BATCH_BYTES)
                .unwrap()
                .0
                .iter()
                .map(|(_, _, e)| b64::encode(e))
                .collect()
        };
        assert!(
            sync.answer(laptop.keys.id(), Message::Push { envelopes: foreign }, NOW)
                .is_err()
        );
    }

    #[tokio::test]
    async fn leaving_the_node_deletes_the_group_keys_and_what_sync_kept() {
        let node = Node::new();
        let laptop = Dev::new();
        node.add(&laptop);
        {
            let conn = laptop.db.lock().unwrap();
            chat::create_conversation(&conn, "Kept").unwrap();
        }
        laptop.sync(&node).await.unwrap();
        let conn = laptop.db.lock().unwrap();
        let group = node.group.lock().unwrap().chain().group();
        assert!(
            crate::membership::load_key(&laptop.store, &group, 0)
                .unwrap()
                .is_some()
        );
        reset(&conn, &laptop.store, &node.keys.public()).unwrap();
        assert!(
            crate::membership::load_key(&laptop.store, &group, 0)
                .unwrap()
                .is_none()
        );
        assert!(
            crate::membership::load(&conn, &node.keys.public())
                .unwrap()
                .is_none()
        );
        assert_eq!(held(&conn).unwrap(), Vector::default());
        drop(conn);
        assert_eq!(laptop.titles(), ["Kept"], "the records stay");
    }

    /// Everything synced on a device, to compare devices: chats with their titles and
    /// messages, open conflicts, and tombstones.
    fn state(dev: &Dev) -> String {
        let conn = dev.db.lock().unwrap();
        let mut chats: Vec<_> = chat::list_conversations(&conn).unwrap();
        chats.sort_by_key(|c| c.id.to_string());
        let mut out = String::new();
        for c in chats {
            let msgs: Vec<String> = chat::list_messages(&conn, c.id)
                .unwrap()
                .into_iter()
                .map(|m| m.id.to_string())
                .collect();
            let conflicts: Vec<String> = records::conflicts(&conn, "chat", &c.id.to_string())
                .unwrap()
                .into_iter()
                .map(|x| format!("{}={}", x.field, x.other))
                .collect();
            out += &format!("{} {:?} {msgs:?} {conflicts:?}\n", c.id, c.title);
        }
        let mut stmt = conn
            .prepare("SELECT collection, id FROM sync_purged ORDER BY collection, id")
            .unwrap();
        let purged: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        out += &format!("purged {purged:?}");
        out
    }

    fn log_len(db: &Mutex<Connection>) -> i64 {
        db.lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM sync_envelopes", [], |r| r.get(0))
            .unwrap()
    }

    #[tokio::test]
    async fn a_late_device_starts_from_the_snapshot_and_ends_like_the_others() {
        let node = Node::new();
        let (a, b) = (Dev::new(), Dev::new());
        node.add(&a);
        node.add(&b);
        let (trip, old) = {
            let conn = a.db.lock().unwrap();
            let trip = chat::create_conversation(&conn, "Trip").unwrap();
            chat::add_message(&conn, trip.id, NewMessage::user_text("Book the train")).unwrap();
            let old = chat::create_conversation(&conn, "Old notes").unwrap();
            chat::add_message(&conn, old.id, NewMessage::user_text("Nothing")).unwrap();
            (trip, old)
        };
        a.sync(&node).await.unwrap();
        b.sync(&node).await.unwrap();
        // Renamed on both at once: both versions are kept.
        chat::rename_conversation(&a.db.lock().unwrap(), trip.id, "Trip to Rome").unwrap();
        chat::rename_conversation(&b.db.lock().unwrap(), trip.id, "Trip in May").unwrap();
        // Deleted for good on A.
        {
            let conn = a.db.lock().unwrap();
            chat::delete_conversation(&conn, old.id).unwrap();
            chat::purge_deleted(&conn, i64::MAX).unwrap();
        }
        for _ in 0..2 {
            a.sync(&node).await.unwrap();
            b.sync(&node).await.unwrap();
        }
        // Every member has applied everything: the Node's log is compacted, and so are the
        // devices'.
        let before = log_len(&node.db);
        a.sync(&node).await.unwrap();
        assert!(log_len(&node.db) < before, "the Node dropped what all have");
        assert!(
            !snapshot::log_start(&node.db.lock().unwrap())
                .unwrap()
                .0
                .is_empty()
        );
        assert!(log_len(&a.db) < before);

        // A new device, with a chat of its own from before pairing.
        let c = Dev::new();
        chat::create_conversation(&c.db.lock().unwrap(), "Made on C").unwrap();
        node.add(&c);
        let r = c.sync(&node).await.unwrap();
        assert!(
            r.snapshot,
            "behind the Node's log: it starts from the snapshot"
        );
        a.sync(&node).await.unwrap();
        b.sync(&node).await.unwrap();
        assert_eq!(state(&c), state(&a));
        assert_eq!(state(&a), state(&b));
        assert!(state(&c).contains("Made on C"));
        assert!(
            !state(&c).contains("Old notes"),
            "a tombstone keeps it gone"
        );
        assert_eq!(
            records::conflicts(&c.db.lock().unwrap(), "chat", &trip.id.to_string())
                .unwrap()
                .len(),
            1,
            "the open conflict came with the snapshot"
        );
        // The Node had all of it, sealed: nothing in dn-store's readable tables, and the
        // conflict text isn't in its database as written.
        {
            let conn = node.db.lock().unwrap();
            assert!(chat::list_conversations(&conn).unwrap().is_empty());
            let kept: Vec<String> = conn
                .prepare("SELECT value FROM sync_conflicts")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert!(!kept.is_empty());
            assert!(kept.iter().all(|v| !v.contains("Trip")));
        }

        // From now on C syncs as usual, and its edits merge like anyone's.
        chat::rename_conversation(&c.db.lock().unwrap(), trip.id, "Trip to Rome in May").unwrap();
        let r = c.sync(&node).await.unwrap();
        assert!(!r.snapshot);
        a.sync(&node).await.unwrap();
        b.sync(&node).await.unwrap();
        assert_eq!(state(&a), state(&c));
        assert_eq!(state(&b), state(&c));
        assert!(state(&a).contains("Trip to Rome in May"));
    }

    #[tokio::test]
    async fn a_snapshot_bigger_than_one_chunk_arrives_whole() {
        let node = Node::new();
        let a = Dev::new();
        node.add(&a);
        {
            let conn = a.db.lock().unwrap();
            let c = chat::create_conversation(&conn, "Long").unwrap();
            for i in 0..600 {
                let text = format!("{i} {}", "x".repeat(4000));
                chat::add_message(&conn, c.id, NewMessage::user_text(text)).unwrap();
            }
        }
        a.sync(&node).await.unwrap();
        a.sync(&node).await.unwrap();
        assert_eq!(log_len(&node.db), 0, "the only member has everything");
        let b = Dev::new();
        node.add(&b);
        let r = b.sync(&node).await.unwrap();
        assert!(r.snapshot);
        let size: i64 = node
            .db
            .lock()
            .unwrap()
            .query_row("SELECT length(sealed) FROM sync_snapshot", [], |r| r.get(0))
            .unwrap();
        assert!(size as usize > snapshot::CHUNK_BYTES * 2, "{size}");
        assert_eq!(state(&a), state(&b));
    }

    #[tokio::test]
    async fn a_snapshot_is_refused_unless_the_pinned_node_sealed_it_for_this_group() {
        let node = Node::new();
        let other = Node::new();
        let a = Dev::new();
        node.add(&a);
        chat::create_conversation(&a.db.lock().unwrap(), "Private").unwrap();
        a.sync(&node).await.unwrap();
        let cols = store_collections();
        let (group, key) = {
            let g = node.group.lock().unwrap();
            let chain = g.chain();
            (chain.group(), g.key(&node.store, chain.epoch()).unwrap())
        };
        let (_, sealed) = snapshot::build(
            &node.db.lock().unwrap(),
            &node.records.collections(),
            &node.keys,
            group,
            &key,
        )
        .unwrap();
        let fresh = Dev::new();
        let install = |sealed: &[u8], key: &GroupKey, signer: &PublicIdentity, g| {
            snapshot::install(
                &fresh.db.lock().unwrap(),
                &cols,
                sealed,
                key,
                signer,
                g,
                NOW,
            )
        };
        // Signed by a member, not the Node pinned at pairing.
        let (_, by_member) = snapshot::build(
            &node.db.lock().unwrap(),
            &node.records.collections(),
            &a.keys,
            group,
            &key,
        )
        .unwrap();
        assert!(matches!(
            install(&by_member, &key, &node.keys.public(), group),
            Err(SyncError::BadSignature)
        ));
        assert!(matches!(
            install(&sealed, &key, &a.keys.public(), group),
            Err(SyncError::BadSignature)
        ));
        // Changed on the way.
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(install(&tampered, &key, &node.keys.public(), group).is_err());
        // Another group, or another group's key.
        let other_group = other.group.lock().unwrap().chain().group();
        assert!(install(&sealed, &key, &node.keys.public(), other_group).is_err());
        let other_key = {
            let g = other.group.lock().unwrap();
            g.key(&other.store, 0).unwrap()
        };
        assert!(install(&sealed, &other_key, &node.keys.public(), group).is_err());
        // An envelope is never a snapshot.
        let env = envelopes_after(&a.db.lock().unwrap(), &Vector::default(), BATCH_BYTES)
            .unwrap()
            .0[0]
            .2
            .clone();
        assert!(install(&env, &key, &node.keys.public(), group).is_err());
        assert!(fresh.titles().is_empty(), "nothing was installed");
        // The real one installs.
        install(&sealed, &key, &node.keys.public(), group).unwrap();
        assert_eq!(fresh.titles(), ["Private"]);
    }

    #[tokio::test]
    async fn a_snapshot_never_overwrites_edits_the_node_has_not_seen() {
        let node = Node::new();
        let a = Dev::new();
        node.add(&a);
        let c = chat::create_conversation(&a.db.lock().unwrap(), "Draft").unwrap();
        a.sync(&node).await.unwrap();
        let cols = store_collections();
        let (group, key) = {
            let g = node.group.lock().unwrap();
            let chain = g.chain();
            (chain.group(), g.key(&node.store, chain.epoch()).unwrap())
        };
        let (_, sealed) = snapshot::build(
            &node.db.lock().unwrap(),
            &node.records.collections(),
            &node.keys,
            group,
            &key,
        )
        .unwrap();
        // An edit not recorded yet.
        chat::rename_conversation(&a.db.lock().unwrap(), c.id, "Final").unwrap();
        let r = snapshot::install(
            &a.db.lock().unwrap(),
            &cols,
            &sealed,
            &key,
            &node.keys.public(),
            group,
            NOW,
        );
        assert!(matches!(r, Err(SyncError::NotYet(_))), "{r:?}");
        // An edit recorded but not applied on the Node.
        let key_a = key_for(&a, &node);
        records::scan(&a.db.lock().unwrap(), &cols, &a.keys, group, &key_a, NOW).unwrap();
        let r = snapshot::install(
            &a.db.lock().unwrap(),
            &cols,
            &sealed,
            &key,
            &node.keys.public(),
            group,
            NOW,
        );
        assert!(matches!(r, Err(SyncError::NotYet(_))), "{r:?}");
        assert_eq!(a.titles(), ["Final"]);
    }

    #[tokio::test]
    async fn a_removed_device_or_an_old_key_never_gets_the_snapshot() {
        let node = Node::new();
        let (a, gone) = (Dev::new(), Dev::new());
        node.add(&a);
        node.add(&gone);
        chat::create_conversation(&a.db.lock().unwrap(), "After the removal").unwrap();
        gone.sync(&node).await.unwrap();
        node.remove(&gone);
        a.sync(&node).await.unwrap();
        let node_cols = node.records.collections();
        let cols = store_collections();
        let sync = NodeSync {
            db: &node.db,
            store: &node.store,
            group: &node.group,
            me: &node.keys,
            collections: &node_cols,
            peer_minor: crate::transport::PROTOCOL_MINOR,
        };
        // Asked directly, on a channel it still had: refused.
        assert!(matches!(
            sync.answer(gone.keys.id(), Message::SnapshotRequest, NOW),
            Err(SyncError::Removed)
        ));
        let Message::Snapshot { vector, size } = sync
            .answer(a.keys.id(), Message::SnapshotRequest, NOW)
            .unwrap()
        else {
            panic!("expected the snapshot");
        };
        assert!(size > 0);
        assert!(matches!(
            sync.answer(
                gone.keys.id(),
                Message::SnapshotPull { vector, offset: 0 },
                NOW
            ),
            Err(SyncError::Removed)
        ));
        assert!(gone.sync(&node).await.is_err());
        // The snapshot is sealed with the key of the epoch after the removal: the old key
        // the removed device holds doesn't open it.
        let (group, old_key, sealed) = {
            let g = node.group.lock().unwrap();
            let chain = g.chain();
            assert!(chain.epoch() >= 1);
            let old = g.key(&node.store, chain.epoch() - 1).unwrap();
            let cur = g.key(&node.store, chain.epoch()).unwrap();
            let (_, sealed) = snapshot::build(
                &node.db.lock().unwrap(),
                &node_cols,
                &node.keys,
                chain.group(),
                &cur,
            )
            .unwrap();
            (chain.group(), old, sealed)
        };
        let fresh = Dev::new();
        assert!(
            snapshot::install(
                &fresh.db.lock().unwrap(),
                &cols,
                &sealed,
                &old_key,
                &node.keys.public(),
                group,
                NOW
            )
            .is_err()
        );
        assert!(fresh.titles().is_empty());
    }

    #[tokio::test]
    async fn a_device_of_the_older_version_behind_the_log_is_told_to_update() {
        let node = Node::new();
        let a = Dev::new();
        node.add(&a);
        chat::create_conversation(&a.db.lock().unwrap(), "Old history").unwrap();
        a.sync(&node).await.unwrap();
        a.sync(&node).await.unwrap();
        assert_eq!(log_len(&node.db), 0);
        let late = Dev::new();
        node.add(&late);
        let cols = node.records.collections();
        let sync = |minor| NodeSync {
            db: &node.db,
            store: &node.store,
            group: &node.group,
            me: &node.keys,
            collections: &cols,
            peer_minor: minor,
        };
        let start = |v: Vector| Message::SyncStart {
            chain_len: 0,
            vector: to_wire(&v),
            applied: to_wire(&v),
        };
        // Minor 0 and behind the log: a clear refusal, not a sync that never completes.
        match sync(0).answer(late.keys.id(), start(Vector::default()), NOW) {
            Err(SyncError::Refused(reason)) => {
                assert_eq!(reason, OLD_DEVICE);
                assert!(reason.starts_with("update Almend on this device"));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        // Minor 0 and not behind: syncs as before.
        let held_a = held(&a.db.lock().unwrap()).unwrap();
        assert!(matches!(
            sync(0).answer(a.keys.id(), start(held_a), NOW),
            Ok(Message::SyncState { .. })
        ));
        // Minor 1: told where the log starts, and fetches the snapshot.
        let Ok(Message::SyncState { floor, .. }) =
            sync(1).answer(late.keys.id(), start(Vector::default()), NOW)
        else {
            panic!("expected the sync state");
        };
        assert!(!floor.is_empty());
        assert!(late.sync(&node).await.unwrap().snapshot);
        assert_eq!(late.titles(), ["Old history"]);
    }

    fn key_for(dev: &Dev, node: &Node) -> GroupKey {
        let conn = dev.db.lock().unwrap();
        let g = ClientGroup::load(&conn, &node.keys.public())
            .unwrap()
            .unwrap();
        g.key(&dev.store, &dev.keys, g.chain().epoch()).unwrap()
    }
}
