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
    v.0.iter().map(|(d, s)| (d.to_hex(), *s)).collect()
}

fn from_wire(v: &BTreeMap<String, u64>) -> Result<Vector> {
    let mut out = Vector::default();
    for (d, s) in v {
        out.set(device(d)?, *s);
    }
    Ok(out)
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

/// Envelopes held here that `theirs` lacks, per author in order, about `max_bytes` of
/// them (at least one). `true` when more remain.
pub fn envelopes_after(
    conn: &Connection,
    theirs: &Vector,
    max_bytes: usize,
) -> Result<(Vec<(DeviceId, u64, Vec<u8>)>, bool)> {
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

/// The Node's side of sync messages from a checked member (after
/// [`crate::transport::authorize_member`]).
pub struct NodeSync<'a> {
    pub db: &'a Mutex<Connection>,
    pub store: &'a dyn SecretStore,
    pub group: &'a Mutex<NodeGroup>,
}

impl NodeSync<'_> {
    /// Whether `message` is one [`NodeSync::answer`] handles.
    pub fn handles(message: &Message) -> bool {
        matches!(
            message,
            Message::SyncStart { .. } | Message::Push { .. } | Message::Pull { .. }
        )
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
            Message::SyncStart { chain_len, .. } => {
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
                    if inbox::keep_from_group(&conn, chain, &opened, now_ms)? == Received::Stored {
                        stored += 1;
                    }
                }
                Ok(Message::Pushed { stored })
            }
            Message::Pull { vector } => {
                let (envs, more) = envelopes_after(&conn, &from_wire(&vector)?, BATCH_BYTES)?;
                Ok(Message::Envelopes {
                    envelopes: envs.iter().map(|(_, _, e)| b64::encode(e)).collect(),
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
    let (chain_len, mine) = {
        let conn = lock(dev.db)?;
        let len = ClientGroup::load(&conn, dev.node)?.map_or(0, |g| g.chain().entries().len());
        (len as u64, held(&conn)?)
    };
    let Message::SyncState { entries, vector } = ask(
        ch,
        &Message::SyncStart {
            chain_len,
            vector: to_wire(&mine),
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

    // 5. What this device lacks.
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
    ch.send(&Message::Done).await?;
    Ok(report)
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
    }

    impl Node {
        fn new() -> Self {
            let keys = DeviceKeys::generate().unwrap();
            let conn = dn_store::open_in_memory(&[MigrationSet::new("sync", crate::migrations())])
                .unwrap();
            let store = MemoryStore::new();
            let group = NodeGroup::load_or_create(&conn, &store, &keys, "Home server").unwrap();
            Self {
                keys,
                db: Mutex::new(conn),
                store,
                group: Mutex::new(group),
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
            let sync = NodeSync {
                db: &self.db,
                store: &self.store,
                group: &self.group,
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
        let sync = NodeSync {
            db: &node.db,
            store: &node.store,
            group: &node.group,
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
}
