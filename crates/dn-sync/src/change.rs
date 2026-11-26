//! Changes and version vectors (docs/design/dn-sync.md §4.1, §7.1).
//!
//! - Every device numbers its own changes 1, 2, 3, … with no gaps.
//! - A change carries its author's vector (`deps`): for each device, the highest change
//!   from it that the author had applied.
//! - B comes after A when B's `deps` include A; otherwise they are concurrent.
//!
//! On the wire a change is JSON, and the signature covers the exact bytes (§7.1), so no
//! canonical encoding is needed.

use crate::hlc::Hlc;
use crate::identity::DeviceId;
use crate::{Result, SyncError, b64};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Format version of changes and envelopes.
pub const CHANGE_VERSION: u32 = 1;
/// Largest change accepted, in bytes (§10.2: a Relay message is at most 1 MiB).
pub const MAX_CHANGE_BYTES: usize = 1024 * 1024;

/// A device group's id: 16 random bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GroupId(pub [u8; 16]);

impl GroupId {
    pub fn random() -> Result<Self> {
        let mut b = [0u8; 16];
        aws_lc_rs::rand::fill(&mut b).map_err(|_| SyncError::Crypto("random"))?;
        Ok(Self(b))
    }
}

/// For each device, the highest change from it that is applied (with no gaps).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Vector(pub BTreeMap<DeviceId, u64>);

impl Vector {
    pub fn get(&self, device: DeviceId) -> u64 {
        self.0.get(&device).copied().unwrap_or(0)
    }

    /// Whether change `seq` from `author` is covered.
    pub fn includes(&self, author: DeviceId, seq: u64) -> bool {
        self.get(author) >= seq
    }

    /// Whether everything in `other` is covered.
    pub fn covers(&self, other: &Vector) -> bool {
        other.0.iter().all(|(d, s)| self.get(*d) >= *s)
    }

    pub fn set(&mut self, device: DeviceId, seq: u64) {
        self.0.insert(device, seq);
    }
}

/// One edit inside a change. Collections and fields are the ones in §3.2.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// Sets the named fields of a record.
    Put {
        collection: String,
        id: String,
        fields: BTreeMap<String, serde_json::Value>,
    },
    /// Writes a record that never changes (a finished message, an artifact version).
    Append {
        collection: String,
        id: String,
        value: serde_json::Value,
    },
    /// Deletes a record for good (a tombstone, §5).
    Purge { collection: String, id: String },
}

/// A change, as its author made it.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub group: GroupId,
    pub epoch: u32,
    pub author: DeviceId,
    pub seq: u64,
    pub deps: Vector,
    pub hlc: Hlc,
    pub ops: Vec<Op>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    v: u32,
    group: String,
    epoch: u32,
    author: String,
    seq: u64,
    deps: BTreeMap<String, u64>,
    hlc: u64,
    ops: Vec<Op>,
}

impl Change {
    /// The bytes that are signed and sealed.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let wire = Wire {
            v: CHANGE_VERSION,
            group: b64::encode(&self.group.0),
            epoch: self.epoch,
            author: self.author.to_hex(),
            seq: self.seq,
            deps: self.deps.0.iter().map(|(d, s)| (d.to_hex(), *s)).collect(),
            hlc: self.hlc.0,
            ops: self.ops.clone(),
        };
        let bytes = serde_json::to_vec(&wire).map_err(|_| SyncError::Protocol("encode change"))?;
        if bytes.len() > MAX_CHANGE_BYTES {
            return Err(SyncError::Protocol("change too large"));
        }
        Ok(bytes)
    }

    /// Reads a change. Strict: unknown fields, a newer version or a malformed id is refused.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_CHANGE_BYTES {
            return Err(SyncError::Protocol("change too large"));
        }
        let wire: Wire =
            serde_json::from_slice(bytes).map_err(|_| SyncError::Protocol("unreadable change"))?;
        if wire.v != CHANGE_VERSION {
            return Err(SyncError::Protocol("unknown change version"));
        }
        let group = GroupId(
            b64::decode_array::<16>(&wire.group).ok_or(SyncError::Protocol("bad group id"))?,
        );
        let author = DeviceId::from_hex(&wire.author).ok_or(SyncError::Protocol("bad author"))?;
        if wire.seq == 0 {
            return Err(SyncError::Protocol("change numbers start at 1"));
        }
        let mut deps = Vector::default();
        for (d, s) in wire.deps {
            let d = DeviceId::from_hex(&d).ok_or(SyncError::Protocol("bad dependency"))?;
            deps.set(d, s);
        }
        // A change depends on the author's own previous change, never on a later one.
        if deps.get(author) != wire.seq - 1 {
            return Err(SyncError::Protocol(
                "a change must follow its author's previous one",
            ));
        }
        Ok(Self {
            group,
            epoch: wire.epoch,
            author,
            seq: wire.seq,
            deps,
            hlc: Hlc(wire.hlc),
            ops: wire.ops,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(n: u8) -> DeviceId {
        DeviceId([n; 16])
    }

    fn change(seq: u64) -> Change {
        let mut deps = Vector::default();
        deps.set(dev(1), seq - 1);
        deps.set(dev(2), 4);
        Change {
            group: GroupId([7; 16]),
            epoch: 0,
            author: dev(1),
            seq,
            deps,
            hlc: Hlc::from_parts(1_000, 2),
            ops: vec![
                Op::Put {
                    collection: "chat".into(),
                    id: "c1".into(),
                    fields: BTreeMap::from([("title".into(), serde_json::json!("Plans"))]),
                },
                Op::Purge {
                    collection: "chat".into(),
                    id: "c0".into(),
                },
            ],
        }
    }

    #[test]
    fn a_change_round_trips() {
        let c = change(3);
        assert_eq!(Change::from_bytes(&c.to_bytes().unwrap()).unwrap(), c);
    }

    #[test]
    fn malformed_changes_are_refused() {
        let good = String::from_utf8(change(3).to_bytes().unwrap()).unwrap();
        for bad in [
            good.replace("\"v\":1", "\"v\":2"),
            good.replace("\"seq\":3", "\"seq\":0"),
            good.replace("\"seq\":3", "\"seq\":5"),
            good.replacen('{', "{\"extra\":1,", 1),
            good.replace("\"op\":\"purge\"", "\"op\":\"drop\""),
            "not json".into(),
        ] {
            assert!(Change::from_bytes(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn vectors_compare_by_every_device() {
        let mut a = Vector::default();
        a.set(dev(1), 3);
        a.set(dev(2), 1);
        let mut b = Vector::default();
        b.set(dev(1), 2);
        assert!(a.covers(&b) && !b.covers(&a));
        assert!(a.includes(dev(1), 3) && !a.includes(dev(1), 4));
        assert_eq!(a.get(dev(9)), 0);
    }
}
