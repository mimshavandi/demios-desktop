//! The device group's membership chain (docs/design/dn-sync.md §8).
//!
//! **The chain:**
//! - Membership is a list of events that the Node signs and chains by hash: each entry names
//!   the hash of the one before it. Every device keeps the whole chain and checks each
//!   entry's signature and link.
//! - Two entries naming the same previous entry (a **fork**) can only come from a misused
//!   Node key. The device stops and says so ([`SyncError::ForkedChain`]).
//!
//! **Group keys:**
//! - `Create` starts epoch 0.
//! - `Add` carries the current group key wrapped to the new device.
//! - `Remove` names the last change accepted from the removed device. It's always followed
//!   by `Rotate`, which carries a new key wrapped to each remaining device.
//! - Keys are wrapped with HPKE base mode (RFC 9180: X25519, HKDF-SHA256, AES-256-GCM), with
//!   the group and epoch in the HPKE `info` and the recipient's id as associated data.
//!
//! [`Chain::accepts`] is the rule the inbox applies to every change: its author was added,
//! and if removed, the change isn't after the last accepted one.

use crate::b64;
use crate::change::{Change, GroupId};
use crate::devices::DeviceKind;
use crate::envelope::GroupKey;
use crate::identity::{DeviceId, DeviceKeys, PublicIdentity};
use crate::{Result, SyncError};
use aws_lc_rs::digest::{SHA256, digest};
use hpke::aead::AesGcm256;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable, OpModeR, OpModeS, Serializable};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

const CHAIN_VERSION: u32 = 1;
const WRAP_INFO: &[u8] = b"dn-sync v1 group key";
/// Most entries a chain may hold, to bound what a device keeps and checks.
const MAX_ENTRIES: usize = 100_000;

type Kem = X25519HkdfSha256;

/// A group key wrapped to one device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wrapped {
    pub device: String,
    pub enc: String,
    pub ciphertext: String,
}

/// One membership event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Create {
        group: String,
        /// The Node's two public keys, Ed25519 then X25519.
        node: String,
        name: String,
    },
    Add {
        keys: String,
        name: String,
        kind: String,
        approved_by: Option<String>,
        /// The current epoch and its key, wrapped to the new device.
        epoch: u32,
        key: Wrapped,
        /// Every earlier epoch's key, wrapped to the new device (index = epoch), so it can
        /// read the group's history.
        earlier: Vec<Wrapped>,
    },
    Remove {
        device: String,
        requested_by: String,
        /// The last change from it that is accepted.
        last_seq: u64,
    },
    Rotate {
        epoch: u32,
        keys: Vec<Wrapped>,
    },
    Rename {
        device: String,
        name: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryWire {
    v: u32,
    n: u64,
    prev: String,
    event: Event,
}

/// One signed entry, as it travels and is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The exact signed bytes.
    pub bytes: Vec<u8>,
    pub signature: [u8; 64],
}

impl Entry {
    fn hash(&self) -> [u8; 32] {
        let mut input = self.bytes.clone();
        input.extend_from_slice(&self.signature);
        let mut h = [0u8; 32];
        h.copy_from_slice(digest(&SHA256, &input).as_ref());
        h
    }
}

/// What the chain says about one device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub keys: PublicIdentity,
    pub name: String,
    pub kind: DeviceKind,
    /// `Some(last accepted seq)` once removed.
    pub removed: Option<u64>,
}

/// A checked membership chain.
#[derive(Clone, Debug)]
pub struct Chain {
    entries: Vec<Entry>,
    hashes: Vec<[u8; 32]>,
    group: GroupId,
    node: PublicIdentity,
    members: BTreeMap<DeviceId, Member>,
    epoch: u32,
}

fn parse_identity(text: &str) -> Result<PublicIdentity> {
    let b = b64::decode_array::<64>(text).ok_or(SyncError::Protocol("bad device keys"))?;
    Ok(PublicIdentity::from_bytes(&b))
}

fn parse_kind(text: &str) -> Result<DeviceKind> {
    match text {
        "desktop" => Ok(DeviceKind::Desktop),
        "phone" => Ok(DeviceKind::Phone),
        "node" => Ok(DeviceKind::Node),
        _ => Err(SyncError::Protocol("bad device kind")),
    }
}

fn device(text: &str) -> Result<DeviceId> {
    DeviceId::from_hex(text).ok_or(SyncError::Protocol("bad device id"))
}

fn wrap_info(group: &GroupId, epoch: u32) -> Vec<u8> {
    let mut info = WRAP_INFO.to_vec();
    info.extend_from_slice(&group.0);
    info.extend_from_slice(&epoch.to_be_bytes());
    info
}

/// Wraps `key` (of `epoch`) to `to` with HPKE.
fn wrap(group: &GroupId, key: &GroupKey, to: &PublicIdentity) -> Result<Wrapped> {
    let pk = <Kem as hpke::Kem>::PublicKey::from_bytes(&to.x25519)
        .map_err(|_| SyncError::Crypto("hpke public key"))?;
    let id = to.id();
    let (enc, ct) = hpke::single_shot_seal::<AesGcm256, HkdfSha256, Kem>(
        &OpModeS::Base,
        &pk,
        &wrap_info(group, key.epoch),
        key.bytes(),
        &id.0,
    )
    .map_err(|_| SyncError::Crypto("hpke seal"))?;
    Ok(Wrapped {
        device: id.to_hex(),
        enc: b64::encode(&enc.to_bytes()),
        ciphertext: b64::encode(&ct),
    })
}

/// Unwraps a key wrapped to `me`.
fn unwrap(group: &GroupId, epoch: u32, w: &Wrapped, me: &DeviceKeys) -> Result<GroupKey> {
    if device(&w.device)? != me.id() {
        return Err(SyncError::Protocol("that key isn't wrapped to this device"));
    }
    let secret = me.agreement_secret()?;
    let sk = <Kem as hpke::Kem>::PrivateKey::from_bytes(secret.as_ref())
        .map_err(|_| SyncError::Crypto("hpke private key"))?;
    let enc = <Kem as hpke::Kem>::EncappedKey::from_bytes(
        &b64::decode(&w.enc).ok_or(SyncError::Protocol("bad wrapped key"))?,
    )
    .map_err(|_| SyncError::Protocol("bad wrapped key"))?;
    let ct = b64::decode(&w.ciphertext).ok_or(SyncError::Protocol("bad wrapped key"))?;
    let plain = Zeroizing::new(
        hpke::single_shot_open::<AesGcm256, HkdfSha256, Kem>(
            &OpModeR::Base,
            &sk,
            &enc,
            &wrap_info(group, epoch),
            &ct,
            &me.id().0,
        )
        .map_err(|_| SyncError::Protocol("the wrapped key doesn't open"))?,
    );
    let bytes: [u8; 32] = plain
        .as_slice()
        .try_into()
        .map_err(|_| SyncError::Protocol("bad wrapped key"))?;
    Ok(GroupKey::from_bytes(epoch, &bytes))
}

impl Chain {
    /// Checks a whole chain from its first entry. `node` is the Node this device pinned when
    /// it paired (from the pairing link): a chain started by any other key is refused, so a
    /// whole chain made up elsewhere can't take over.
    pub fn from_entries(entries: &[Entry], expected: &PublicIdentity) -> Result<Self> {
        let first = entries.first().ok_or(SyncError::Protocol("empty chain"))?;
        let wire: EntryWire = serde_json::from_slice(&first.bytes)
            .map_err(|_| SyncError::Protocol("unreadable chain entry"))?;
        let Event::Create {
            group,
            node,
            name: _,
        } = &wire.event
        else {
            return Err(SyncError::Protocol("a chain starts with Create"));
        };
        if wire.v != CHAIN_VERSION || wire.n != 0 || !wire.prev.is_empty() {
            return Err(SyncError::Protocol("bad first chain entry"));
        }
        let created_by = parse_identity(node)?;
        if created_by != *expected {
            return Err(SyncError::NodeKeyMismatch);
        }
        let node = created_by;
        node.verify(&first.bytes, &first.signature)?;
        let group =
            GroupId(b64::decode_array::<16>(group).ok_or(SyncError::Protocol("bad group id"))?);
        let mut chain = Self {
            entries: Vec::new(),
            hashes: Vec::new(),
            group,
            node,
            members: BTreeMap::new(),
            epoch: 0,
        };
        for entry in entries {
            chain.append(entry.clone())?;
        }
        Ok(chain)
    }

    /// Adds the next entry from the Node. An entry already held is a no-op; a different
    /// entry at a position already held is a fork.
    pub fn append(&mut self, entry: Entry) -> Result<()> {
        let wire: EntryWire = serde_json::from_slice(&entry.bytes)
            .map_err(|_| SyncError::Protocol("unreadable chain entry"))?;
        if wire.v != CHAIN_VERSION {
            return Err(SyncError::Protocol("unknown chain version"));
        }
        let n = usize::try_from(wire.n).map_err(|_| SyncError::Protocol("bad chain index"))?;
        self.node.verify(&entry.bytes, &entry.signature)?;
        if n < self.entries.len() {
            return if self.entries[n] == entry {
                Ok(())
            } else {
                Err(SyncError::ForkedChain)
            };
        }
        if n != self.entries.len() || n >= MAX_ENTRIES {
            return Err(SyncError::Protocol("chain entry out of order"));
        }
        let prev = if n == 0 {
            String::new()
        } else {
            b64::encode(&self.hashes[n - 1])
        };
        if wire.prev != prev {
            return Err(SyncError::ForkedChain);
        }
        self.apply(&wire.event, n == 0)?;
        self.hashes.push(entry.hash());
        self.entries.push(entry);
        Ok(())
    }

    fn apply(&mut self, event: &Event, first: bool) -> Result<()> {
        match event {
            Event::Create { node, name, .. } => {
                if !first {
                    return Err(SyncError::Protocol("Create only starts a chain"));
                }
                let keys = parse_identity(node)?;
                self.members.insert(
                    keys.id(),
                    Member {
                        keys,
                        name: name.clone(),
                        kind: DeviceKind::Node,
                        removed: None,
                    },
                );
            }
            _ if first => return Err(SyncError::Protocol("a chain starts with Create")),
            Event::Add {
                keys,
                name,
                kind,
                epoch,
                key,
                earlier,
                ..
            } => {
                let keys = parse_identity(keys)?;
                if self.members.contains_key(&keys.id())
                    || self
                        .members
                        .values()
                        .any(|m| m.keys.ed25519 == keys.ed25519)
                {
                    return Err(SyncError::AlreadyPaired);
                }
                if *epoch != self.epoch || device(&key.device)? != keys.id() {
                    return Err(SyncError::Protocol(
                        "Add must carry the current key for that device",
                    ));
                }
                if earlier.len() != self.epoch as usize
                    || earlier.iter().any(|w| w.device != key.device)
                {
                    return Err(SyncError::Protocol(
                        "Add must carry every earlier epoch's key for that device",
                    ));
                }
                self.members.insert(
                    keys.id(),
                    Member {
                        keys,
                        name: name.clone(),
                        kind: parse_kind(kind)?,
                        removed: None,
                    },
                );
            }
            Event::Remove {
                device: d,
                last_seq,
                ..
            } => {
                let id = device(d)?;
                if id == self.node.id() {
                    return Err(SyncError::Protocol("the Node can't be removed"));
                }
                let m = self.members.get_mut(&id).ok_or(SyncError::UnknownDevice)?;
                if m.removed.is_some() {
                    return Err(SyncError::Removed);
                }
                m.removed = Some(*last_seq);
            }
            Event::Rotate { epoch, keys } => {
                if *epoch != self.epoch + 1 {
                    return Err(SyncError::Protocol("epochs go up by one"));
                }
                // Exactly the current members get the new key, and nobody else.
                let mut want: Vec<String> = self
                    .members
                    .iter()
                    .filter(|(_, m)| m.removed.is_none())
                    .map(|(id, _)| id.to_hex())
                    .collect();
                let mut got: Vec<String> = keys.iter().map(|w| w.device.clone()).collect();
                want.sort();
                got.sort();
                if want != got {
                    return Err(SyncError::Protocol(
                        "a rotation must cover exactly the members",
                    ));
                }
                self.epoch = *epoch;
            }
            Event::Rename { device: d, name } => {
                let m = self
                    .members
                    .get_mut(&device(d)?)
                    .ok_or(SyncError::UnknownDevice)?;
                name.clone_into(&mut m.name);
            }
        }
        Ok(())
    }

    pub fn group(&self) -> GroupId {
        self.group
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn node(&self) -> PublicIdentity {
        self.node
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn member(&self, id: DeviceId) -> Option<&Member> {
        self.members.get(&id)
    }

    /// The devices that are members now (not removed), the Node included.
    pub fn current_members(&self) -> impl Iterator<Item = (DeviceId, &Member)> {
        self.members
            .iter()
            .filter(|(_, m)| m.removed.is_none())
            .map(|(d, m)| (*d, m))
    }

    /// The key of an author for checking a signature: any device ever added (a removed
    /// one's earlier changes still verify; [`Chain::accepts`] decides the rest).
    pub fn author_key(&self, author: DeviceId) -> Result<PublicIdentity> {
        self.members
            .get(&author)
            .map(|m| m.keys)
            .ok_or(SyncError::UnknownDevice)
    }

    /// Whether a change from its author is accepted: the author was added, and if it was
    /// removed, this change isn't after the last one accepted from it (§8.2).
    pub fn accepts(&self, change: &Change) -> Result<()> {
        if change.group != self.group {
            return Err(SyncError::Protocol("a change from another group"));
        }
        let m = self
            .members
            .get(&change.author)
            .ok_or(SyncError::UnknownDevice)?;
        match m.removed {
            Some(last) if change.seq > last => Err(SyncError::Removed),
            _ => Ok(()),
        }
    }

    /// This device's group key for `epoch`, from the entry that gave it to this device.
    pub fn key_for(&self, me: &DeviceKeys, epoch: u32) -> Result<GroupKey> {
        let my = me.id().to_hex();
        for entry in &self.entries {
            let wire: EntryWire = serde_json::from_slice(&entry.bytes)
                .map_err(|_| SyncError::Protocol("unreadable chain entry"))?;
            match &wire.event {
                Event::Add {
                    epoch: e,
                    key,
                    earlier,
                    ..
                } if key.device == my && *e >= epoch => {
                    let w = if *e == epoch {
                        key
                    } else {
                        &earlier[epoch as usize]
                    };
                    return unwrap(&self.group, epoch, w, me);
                }
                Event::Rotate { epoch: e, keys } if *e == epoch => {
                    if let Some(w) = keys.iter().find(|w| w.device == my) {
                        return unwrap(&self.group, epoch, w, me);
                    }
                }
                _ => {}
            }
        }
        Err(SyncError::Protocol("this device has no key for that epoch"))
    }
}

/// The Node's side: writes and signs entries.
pub struct NodeChain {
    chain: Chain,
    /// The current group key (the Node keeps every epoch's key in its secret store).
    key: GroupKey,
}

impl NodeChain {
    fn sign(&self, node: &DeviceKeys, event: Event) -> Result<Entry> {
        let n = self.chain.entries.len() as u64;
        let prev = self
            .chain
            .hashes
            .last()
            .map(|h| b64::encode(h))
            .unwrap_or_default();
        let bytes = serde_json::to_vec(&EntryWire {
            v: CHAIN_VERSION,
            n,
            prev,
            event,
        })
        .map_err(|_| SyncError::Protocol("encode chain entry"))?;
        Ok(Entry {
            signature: node.sign(&bytes),
            bytes,
        })
    }

    /// A new group with the Node as its first member, and epoch 0's key.
    pub fn create(node: &DeviceKeys, name: &str) -> Result<Self> {
        let group = GroupId::random()?;
        let event = Event::Create {
            group: b64::encode(&group.0),
            node: b64::encode(&node.public().to_bytes()),
            name: name.to_owned(),
        };
        let bytes = serde_json::to_vec(&EntryWire {
            v: CHAIN_VERSION,
            n: 0,
            prev: String::new(),
            event,
        })
        .map_err(|_| SyncError::Protocol("encode chain entry"))?;
        let entry = Entry {
            signature: node.sign(&bytes),
            bytes,
        };
        Ok(Self {
            chain: Chain::from_entries(&[entry], &node.public())?,
            key: GroupKey::generate(0)?,
        })
    }

    /// A Node's chain as loaded, with its current group key.
    pub fn from_parts(chain: Chain, key: GroupKey) -> Result<Self> {
        if key.epoch != chain.epoch() {
            return Err(SyncError::Protocol(
                "the group key isn't the chain's current epoch",
            ));
        }
        Ok(Self { chain, key })
    }

    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    pub fn key(&self) -> &GroupKey {
        &self.key
    }

    pub(crate) fn key_bytes(&self) -> &[u8; 32] {
        self.key.bytes()
    }

    fn push(&mut self, node: &DeviceKeys, event: Event) -> Result<Entry> {
        let entry = self.sign(node, event)?;
        self.chain.append(entry.clone())?;
        Ok(entry)
    }

    /// Adds a paired device, giving it the current key.
    pub fn add(
        &mut self,
        node: &DeviceKeys,
        keys: &PublicIdentity,
        name: &str,
        kind: DeviceKind,
        approved_by: Option<DeviceId>,
        earlier_keys: &[GroupKey],
    ) -> Result<Entry> {
        let key = wrap(&self.chain.group, &self.key, keys)?;
        if earlier_keys.len() != self.key.epoch as usize
            || earlier_keys
                .iter()
                .enumerate()
                .any(|(i, k)| k.epoch != i as u32)
        {
            return Err(SyncError::Protocol(
                "every earlier epoch's key is needed to add a device",
            ));
        }
        let earlier = earlier_keys
            .iter()
            .map(|k| wrap(&self.chain.group, k, keys))
            .collect::<Result<Vec<_>>>()?;
        self.push(
            node,
            Event::Add {
                keys: b64::encode(&keys.to_bytes()),
                name: name.to_owned(),
                kind: crate::transport::kind_name(kind).to_owned(),
                approved_by: approved_by.map(|d| d.to_hex()),
                epoch: self.key.epoch,
                key,
                earlier,
            },
        )
    }

    /// Removes a device and rotates the key: two entries. Changes from it after `last_seq`
    /// are refused from now on, and the new key isn't wrapped to it.
    pub fn remove(
        &mut self,
        node: &DeviceKeys,
        device: DeviceId,
        requested_by: DeviceId,
        last_seq: u64,
    ) -> Result<[Entry; 2]> {
        let removed = self.push(
            node,
            Event::Remove {
                device: device.to_hex(),
                requested_by: requested_by.to_hex(),
                last_seq,
            },
        )?;
        let next = GroupKey::generate(self.key.epoch + 1)?;
        let mut keys = Vec::new();
        for m in self.chain.members.values().filter(|m| m.removed.is_none()) {
            keys.push(wrap(&self.chain.group, &next, &m.keys)?);
        }
        let rotated = self.push(
            node,
            Event::Rotate {
                epoch: next.epoch,
                keys,
            },
        )?;
        self.key = next;
        Ok([removed, rotated])
    }
}

pub(crate) const MIGRATION: &str = "
-- The membership chain as the Node signed it, in order (docs/design/dn-sync.md §8.2).
CREATE TABLE sync_membership (
    n         INTEGER PRIMARY KEY NOT NULL,
    bytes     BLOB    NOT NULL,
    signature BLOB    NOT NULL CHECK (length(signature) = 64)
);
";

/// Stores the entries of `chain` not stored yet. Entries are append-only.
///
/// Runs in a savepoint, so it joins the caller's transaction when there is one.
pub fn save(conn: &rusqlite::Connection, chain: &Chain) -> Result<()> {
    conn.execute_batch("SAVEPOINT sync_membership_save")?;
    match save_entries(conn, chain) {
        Ok(()) => {
            conn.execute_batch("RELEASE sync_membership_save")?;
            Ok(())
        }
        Err(e) => {
            conn.execute_batch("ROLLBACK TO sync_membership_save; RELEASE sync_membership_save")?;
            Err(e)
        }
    }
}

fn save_entries(tx: &rusqlite::Connection, chain: &Chain) -> Result<()> {
    let mut stmt = tx.prepare("SELECT n, bytes, signature FROM sync_membership ORDER BY n")?;
    let stored = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    // What's stored must be the start of this chain; anything else is refused, not overwritten.
    for (i, (n, bytes, sig)) in stored.iter().enumerate() {
        let same = chain.entries().get(i).is_some_and(|e| {
            *n == i as i64 && e.bytes == *bytes && e.signature.as_slice() == sig.as_slice()
        });
        if !same {
            return Err(SyncError::ForkedChain);
        }
    }
    for (n, e) in chain.entries().iter().enumerate().skip(stored.len()) {
        tx.execute(
            "INSERT INTO sync_membership (n, bytes, signature) VALUES (?1, ?2, ?3)",
            rusqlite::params![n as i64, e.bytes, e.signature.as_slice()],
        )?;
    }
    Ok(())
}

/// Loads and checks the stored chain, if any, against the paired Node's key.
pub fn load(conn: &rusqlite::Connection, node: &PublicIdentity) -> Result<Option<Chain>> {
    let mut stmt = conn.prepare("SELECT bytes, signature FROM sync_membership ORDER BY n")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut entries = Vec::new();
    for row in rows {
        let (bytes, sig) = row?;
        let signature: [u8; 64] = sig
            .try_into()
            .map_err(|_| SyncError::Protocol("damaged membership entry"))?;
        entries.push(Entry { bytes, signature });
    }
    if entries.is_empty() {
        return Ok(None);
    }
    Chain::from_entries(&entries, node).map(Some)
}

/// The secret-store id of a group key: `device/group.<group>.<epoch>`. Group keys live in
/// the device's secret store, never in SQLite or logs (§8.3).
fn key_id(group: &GroupId, epoch: u32) -> dn_secrets::SecretId {
    dn_secrets::SecretId::device(format!("group.{}.{epoch}", b64::encode(&group.0)))
}

/// Keeps a group key in the secret store.
pub fn store_key(
    store: &dyn dn_secrets::SecretStore,
    group: &GroupId,
    key: &GroupKey,
) -> Result<()> {
    let text = Zeroizing::new(b64::encode(key.bytes()));
    store.set(
        &key_id(group, key.epoch),
        &dn_secrets::Secret::new(text.as_str()),
    )?;
    Ok(())
}

/// A group key from the secret store. A store that fails is an error (#200 shows it); a
/// missing key is `None` (the device unwraps it from the chain again).
pub fn load_key(
    store: &dyn dn_secrets::SecretStore,
    group: &GroupId,
    epoch: u32,
) -> Result<Option<GroupKey>> {
    let Some(secret) = store.get(&key_id(group, epoch))? else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(
        b64::decode_array::<32>(secret.expose()).ok_or(SyncError::DamagedKeys("group key"))?,
    );
    Ok(Some(GroupKey::from_bytes(epoch, &bytes)))
}

/// Deletes an old epoch's key once no envelope sealed with it is kept (§8.3).
pub fn delete_key(store: &dyn dn_secrets::SecretStore, group: &GroupId, epoch: u32) -> Result<()> {
    store.delete(&key_id(group, epoch))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::{Op, Vector};
    use crate::envelope::{open, seal};
    use crate::hlc::Hlc;

    fn change(chain: &Chain, author: &DeviceKeys, seq: u64, epoch: u32) -> Change {
        let mut deps = Vector::default();
        deps.set(author.id(), seq - 1);
        Change {
            group: chain.group(),
            epoch,
            author: author.id(),
            seq,
            deps,
            hlc: Hlc::from_parts(1_000 + seq, 0),
            ops: vec![Op::Purge {
                collection: "chat".into(),
                id: format!("c{seq}"),
            }],
        }
    }

    struct Group {
        node: DeviceKeys,
        nc: NodeChain,
        laptop: DeviceKeys,
        phone: DeviceKeys,
    }

    fn group() -> Group {
        let node = DeviceKeys::generate().unwrap();
        let mut nc = NodeChain::create(&node, "Home server").unwrap();
        let laptop = DeviceKeys::generate().unwrap();
        let phone = DeviceKeys::generate().unwrap();
        nc.add(
            &node,
            &laptop.public(),
            "Laptop",
            DeviceKind::Desktop,
            Some(node.id()),
            &[],
        )
        .unwrap();
        nc.add(
            &node,
            &phone.public(),
            "Work phone",
            DeviceKind::Phone,
            Some(node.id()),
            &[],
        )
        .unwrap();
        Group {
            node,
            nc,
            laptop,
            phone,
        }
    }

    #[test]
    fn every_device_checks_the_chain_and_gets_the_groups_key() {
        let g = group();
        let copy = Chain::from_entries(g.nc.chain().entries(), &g.node.public()).unwrap();
        assert_eq!(copy.group(), g.nc.chain().group());
        assert_eq!(copy.member(g.phone.id()).unwrap().name, "Work phone");
        let k_laptop = copy.key_for(&g.laptop, 0).unwrap();
        let k_phone = copy.key_for(&g.phone, 0).unwrap();
        assert_eq!(k_laptop.bytes(), g.nc.key().bytes());
        assert_eq!(k_phone.bytes(), g.nc.key().bytes());
        // A device that isn't in the group gets nothing.
        let stranger = DeviceKeys::generate().unwrap();
        assert!(copy.key_for(&stranger, 0).is_err());
    }

    #[test]
    fn a_removed_device_is_refused_after_its_last_change_and_cant_read_the_new_epoch() {
        let mut g = group();
        let before = Chain::from_entries(g.nc.chain().entries(), &g.node.public()).unwrap();
        let old_key = before.key_for(&g.phone, 0).unwrap();
        g.nc.remove(&g.node, g.phone.id(), g.laptop.id(), 3)
            .unwrap();
        let chain = Chain::from_entries(g.nc.chain().entries(), &g.node.public()).unwrap();
        assert_eq!(chain.epoch(), 1);
        // Its changes up to #3 still count; #4 on is refused, through any route.
        assert!(chain.accepts(&change(&chain, &g.phone, 3, 0)).is_ok());
        assert!(matches!(
            chain.accepts(&change(&chain, &g.phone, 4, 0)),
            Err(SyncError::Removed)
        ));
        // The new key goes to the others only.
        assert!(chain.key_for(&g.phone, 1).is_err());
        let new_key = chain.key_for(&g.laptop, 1).unwrap();
        assert_eq!(new_key.bytes(), g.nc.key().bytes());
        // An envelope sealed in the new epoch can't be opened with the removed device's key.
        let c = change(&chain, &g.laptop, 1, 1);
        let env = seal(&c, &g.laptop, &new_key).unwrap();
        assert!(open(&env, &old_key, |_, _| Ok(g.laptop.public())).is_err());
        assert!(
            open(&env, &GroupKey::from_bytes(1, old_key.bytes()), |_, _| Ok(
                g.laptop.public()
            ))
            .is_err()
        );
    }

    #[test]
    fn an_old_epoch_envelope_from_a_member_still_in_flight_is_accepted() {
        let mut g = group();
        let chain0 = Chain::from_entries(g.nc.chain().entries(), &g.node.public()).unwrap();
        let k0 = chain0.key_for(&g.laptop, 0).unwrap();
        let in_flight = seal(&change(&chain0, &g.laptop, 1, 0), &g.laptop, &k0).unwrap();
        g.nc.remove(&g.node, g.phone.id(), g.node.id(), 0).unwrap();
        let chain = Chain::from_entries(g.nc.chain().entries(), &g.node.public()).unwrap();
        // The Node (or any member) still has epoch 0's key and the laptop is still a member.
        let k0_again = chain.key_for(&g.laptop, 0).unwrap();
        let opened = open(&in_flight, &k0_again, |a, _| chain.author_key(a)).unwrap();
        chain.accepts(&opened.change).unwrap();
    }

    #[test]
    fn a_device_added_after_a_rotation_gets_every_epochs_key() {
        let mut g = group();
        let k0 = GroupKey::from_bytes(0, g.nc.key().bytes());
        g.nc.remove(&g.node, g.phone.id(), g.node.id(), 0).unwrap();
        let tablet = DeviceKeys::generate().unwrap();
        // Without the earlier keys the Node refuses to add it.
        assert!(
            g.nc.add(
                &g.node,
                &tablet.public(),
                "Tablet",
                DeviceKind::Phone,
                None,
                &[]
            )
            .is_err()
        );
        g.nc.add(
            &g.node,
            &tablet.public(),
            "Tablet",
            DeviceKind::Phone,
            None,
            std::slice::from_ref(&k0),
        )
        .unwrap();
        let chain = Chain::from_entries(g.nc.chain().entries(), &g.node.public()).unwrap();
        assert_eq!(chain.key_for(&tablet, 0).unwrap().bytes(), k0.bytes());
        assert_eq!(
            chain.key_for(&tablet, 1).unwrap().bytes(),
            g.nc.key().bytes()
        );
        assert!(chain.key_for(&tablet, 2).is_err());
    }

    #[test]
    fn a_forked_chain_is_detected() {
        let g = group();
        let mut chain = Chain::from_entries(g.nc.chain().entries(), &g.node.public()).unwrap();
        // Two Node branches from the same point: one removes the phone, the other renames it.
        let mut a = NodeChain {
            chain: chain.clone(),
            key: GroupKey::from_bytes(0, g.nc.key().bytes()),
        };
        let mut b = NodeChain {
            chain: chain.clone(),
            key: GroupKey::from_bytes(0, g.nc.key().bytes()),
        };
        let [ra, _] = a.remove(&g.node, g.phone.id(), g.node.id(), 0).unwrap();
        let rb = b
            .push(
                &g.node,
                Event::Rename {
                    device: g.phone.id().to_hex(),
                    name: "Old phone".into(),
                },
            )
            .unwrap();
        chain.append(ra.clone()).unwrap();
        assert!(matches!(chain.append(rb), Err(SyncError::ForkedChain)));
        // The same entry again is fine.
        chain.append(ra).unwrap();
    }

    #[test]
    fn a_chain_started_by_another_node_is_refused() {
        let g = group();
        let other = DeviceKeys::generate().unwrap();
        let theirs = NodeChain::create(&other, "Not your Node").unwrap();
        assert!(matches!(
            Chain::from_entries(theirs.chain().entries(), &g.node.public()),
            Err(SyncError::NodeKeyMismatch)
        ));
        // And a stored chain that differs from the one being saved isn't overwritten.
        let conn =
            dn_store::open_in_memory(&[dn_store::MigrationSet::new("sync", crate::migrations())])
                .unwrap();
        save(&conn, g.nc.chain()).unwrap();
        assert!(matches!(
            save(&conn, theirs.chain()),
            Err(SyncError::ForkedChain)
        ));
    }

    #[test]
    fn entries_not_signed_by_the_node_are_refused() {
        let g = group();
        let mut chain = Chain::from_entries(g.nc.chain().entries(), &g.node.public()).unwrap();
        // The laptop tries to write membership itself.
        let forged = NodeChain {
            chain: chain.clone(),
            key: GroupKey::from_bytes(0, g.nc.key().bytes()),
        };
        let attacker = DeviceKeys::generate().unwrap();
        let entry = forged
            .sign(
                &g.laptop,
                Event::Add {
                    keys: b64::encode(&attacker.public().to_bytes()),
                    name: "Not mine".into(),
                    kind: "desktop".into(),
                    approved_by: None,
                    epoch: 0,
                    key: wrap(&chain.group(), g.nc.key(), &attacker.public()).unwrap(),
                    earlier: vec![],
                },
            )
            .unwrap();
        assert!(matches!(chain.append(entry), Err(SyncError::BadSignature)));
        // And an entry with a wrong link, even signed by the Node, is a fork.
        let mut tampered = forged
            .sign(
                &g.node,
                Event::Rename {
                    device: g.laptop.id().to_hex(),
                    name: "X".into(),
                },
            )
            .unwrap();
        let mut wire: EntryWire = serde_json::from_slice(&tampered.bytes).unwrap();
        wire.prev = b64::encode(&[0u8; 32]);
        tampered.bytes = serde_json::to_vec(&wire).unwrap();
        tampered.signature = g.node.sign(&tampered.bytes);
        assert!(matches!(
            chain.append(tampered),
            Err(SyncError::ForkedChain)
        ));
    }

    #[test]
    fn the_node_cant_be_removed_and_a_rotation_must_cover_the_members() {
        let mut g = group();
        assert!(g.nc.remove(&g.node, g.node.id(), g.laptop.id(), 0).is_err());
        // A Node that leaves someone out of a rotation writes an entry every device refuses.
        let mut copy = Chain::from_entries(g.nc.chain().entries(), &g.node.public()).unwrap();
        let partial = NodeChain {
            chain: copy.clone(),
            key: GroupKey::from_bytes(0, g.nc.key().bytes()),
        };
        let next = GroupKey::generate(1).unwrap();
        let only_laptop = vec![wrap(&copy.group(), &next, &g.laptop.public()).unwrap()];
        let entry = partial.sign(
            &g.node,
            Event::Rotate {
                epoch: 1,
                keys: only_laptop,
            },
        );
        assert!(copy.append(entry.unwrap()).is_err());
    }

    #[test]
    fn the_chain_and_keys_are_kept_and_checked_again_on_load() {
        let mut g = group();
        let conn =
            dn_store::open_in_memory(&[dn_store::MigrationSet::new("sync", crate::migrations())])
                .unwrap();
        assert!(load(&conn, &g.node.public()).unwrap().is_none());
        save(&conn, g.nc.chain()).unwrap();
        g.nc.remove(&g.node, g.phone.id(), g.node.id(), 0).unwrap();
        save(&conn, g.nc.chain()).unwrap();
        let loaded = load(&conn, &g.node.public()).unwrap().unwrap();
        assert_eq!(loaded.entries(), g.nc.chain().entries());
        assert_eq!(loaded.epoch(), 1);
        // A changed stored entry fails the check on load.
        conn.execute(
            "UPDATE sync_membership SET bytes = bytes || ' ' WHERE n = 1",
            [],
        )
        .unwrap();
        assert!(load(&conn, &g.node.public()).is_err());

        let store = dn_secrets::MemoryStore::new();
        let key = loaded.key_for(&g.laptop, 1).unwrap();
        store_key(&store, &loaded.group(), &key).unwrap();
        let back = load_key(&store, &loaded.group(), 1).unwrap().unwrap();
        assert_eq!(back.bytes(), key.bytes());
        assert!(load_key(&store, &loaded.group(), 0).unwrap().is_none());
        delete_key(&store, &loaded.group(), 1).unwrap();
        assert!(load_key(&store, &loaded.group(), 1).unwrap().is_none());
    }

    #[test]
    fn a_wrapped_key_only_opens_for_its_device_epoch_and_group() {
        let g = group();
        let chain = g.nc.chain();
        let w = wrap(&chain.group(), g.nc.key(), &g.laptop.public()).unwrap();
        assert!(unwrap(&chain.group(), 0, &w, &g.laptop).is_ok());
        assert!(unwrap(&chain.group(), 1, &w, &g.laptop).is_err());
        assert!(unwrap(&GroupId([0; 16]), 0, &w, &g.laptop).is_err());
        assert!(unwrap(&chain.group(), 0, &w, &g.phone).is_err());
    }
}
