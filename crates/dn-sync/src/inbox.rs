//! Change numbers, the vector, and the inbox (docs/design/dn-sync.md §4, §9.1, §9.3).
//!
//! - [`next_change`] numbers this device's next change and stamps it with the vector and the
//!   HLC.
//! - [`receive`] keeps an opened envelope in `sync_inbox` before anything acknowledges it, so
//!   a crash loses nothing:
//!   - a second copy of a change is ignored;
//!   - two **different** envelopes with one (author, seq) are equivocation (§9.3). The second
//!     is refused, and that author's later changes are held.
//! - [`next_ready`] hands out the next change whose dependencies are applied and whose clock
//!   isn't too far ahead (§4.3). The caller opens and applies it, then calls [`applied`].
//! - [`waiting`] lists what waits and why, for the device list.

use crate::change::{Change, GroupId, Op, Vector};
use crate::envelope::Opened;
use crate::hlc::{Hlc, HlcClock};
use crate::identity::DeviceId;
use crate::{Result, SyncError};
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) const MIGRATION: &str = "
-- For each author, the highest change applied with no gaps (this device's vector).
CREATE TABLE sync_vector (
    author TEXT    PRIMARY KEY NOT NULL,
    seq    INTEGER NOT NULL
) WITHOUT ROWID;

-- Every change received or made here, by (author, seq), with the hash of its signed bytes:
-- duplicates (however they were sealed) are recognised, and a different signed change under
-- the same number is caught.
CREATE TABLE sync_seen (
    author TEXT    NOT NULL,
    seq    INTEGER NOT NULL,
    hash   BLOB    NOT NULL CHECK (length(hash) = 32),
    PRIMARY KEY (author, seq)
) WITHOUT ROWID;

-- Received changes not applied yet, sealed as they arrived.
CREATE TABLE sync_inbox (
    author      TEXT    NOT NULL,
    seq         INTEGER NOT NULL,
    deps        TEXT    NOT NULL,
    hlc         INTEGER NOT NULL,
    envelope    BLOB    NOT NULL,
    received_at INTEGER NOT NULL,
    PRIMARY KEY (author, seq)
) WITHOUT ROWID;

-- Authors whose changes are held (equivocation), until the user decides, with the evidence:
-- the conflicting envelope, which other devices can check against the first one (§9.3).
CREATE TABLE sync_held_authors (
    author   TEXT    PRIMARY KEY NOT NULL,
    reason   TEXT    NOT NULL,
    at       INTEGER NOT NULL,
    seq      INTEGER NOT NULL,
    evidence BLOB    NOT NULL
) WITHOUT ROWID;

-- This device's hybrid logical clock.
CREATE TABLE sync_clock (
    id  INTEGER PRIMARY KEY CHECK (id = 1),
    hlc INTEGER NOT NULL
);
";

fn hex(d: DeviceId) -> String {
    d.to_hex()
}

fn parse_device(text: &str) -> Result<DeviceId> {
    DeviceId::from_hex(text).ok_or(SyncError::Protocol("damaged device id in the sync tables"))
}

/// This device's vector.
pub fn vector(conn: &Connection) -> Result<Vector> {
    let mut stmt = conn.prepare("SELECT author, seq FROM sync_vector")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut v = Vector::default();
    for row in rows {
        let (a, s) = row?;
        v.set(parse_device(&a)?, u64::try_from(s).unwrap_or(0));
    }
    Ok(v)
}

fn set_vector(conn: &Connection, author: DeviceId, seq: u64) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_vector (author, seq) VALUES (?1, ?2)
         ON CONFLICT (author) DO UPDATE SET seq = excluded.seq",
        params![hex(author), i64::try_from(seq).unwrap_or(i64::MAX)],
    )?;
    Ok(())
}

fn clock(conn: &Connection) -> Result<HlcClock> {
    let last: Option<i64> = conn
        .query_row("SELECT hlc FROM sync_clock WHERE id = 1", [], |r| r.get(0))
        .optional()?;
    Ok(HlcClock {
        last: Hlc(last.map_or(0, |v| v as u64)),
    })
}

fn save_clock(conn: &Connection, c: HlcClock) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_clock (id, hlc) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET hlc = excluded.hlc",
        [c.last.0 as i64],
    )?;
    Ok(())
}

/// This device's next change: the next number, the current vector as its dependencies,
/// and a fresh HLC time. It counts as applied here at once (the edit is already local).
pub fn next_change(
    conn: &Connection,
    me: DeviceId,
    group: GroupId,
    epoch: u32,
    ops: Vec<Op>,
    now_ms: u64,
) -> Result<Change> {
    let tx = conn.unchecked_transaction()?;
    let deps = vector(&tx)?;
    let seq = deps.get(me) + 1;
    let mut c = clock(&tx)?;
    let hlc = c.send(now_ms);
    let change = Change {
        group,
        epoch,
        author: me,
        seq,
        deps,
        hlc,
        ops,
    };
    // Recorded like a received change, so a different change signed with this device's key
    // under one of its numbers is caught when it comes back (§9.3).
    let hash = crate::envelope::change_hash(&change.to_bytes()?);
    tx.execute(
        "INSERT INTO sync_seen (author, seq, hash) VALUES (?1, ?2, ?3)",
        params![hex(me), seq as i64, hash.as_slice()],
    )?;
    save_clock(&tx, c)?;
    set_vector(&tx, me, seq)?;
    tx.commit()?;
    Ok(change)
}

/// What [`receive`] did with an envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Received {
    /// Kept in the inbox; safe to acknowledge.
    Stored,
    /// Already received or applied; safe to acknowledge.
    Duplicate,
}

/// Keeps an opened envelope in the inbox. See the module notes.
pub fn receive(conn: &Connection, opened: &Opened, now_ms: u64) -> Result<Received> {
    let envelope = opened.envelope.as_slice();
    let c = &opened.change;
    let tx = conn.unchecked_transaction()?;
    let seen: Option<Vec<u8>> = tx
        .query_row(
            "SELECT hash FROM sync_seen WHERE author = ?1 AND seq = ?2",
            params![hex(c.author), c.seq as i64],
            |r| r.get(0),
        )
        .optional()?;
    match seen {
        Some(h) if h == opened.hash => return Ok(Received::Duplicate),
        Some(_) => {
            tx.execute(
                "INSERT OR IGNORE INTO sync_held_authors (author, reason, at, seq, evidence)
                 VALUES (?1, 'sent conflicting changes', ?2, ?3, ?4)",
                params![hex(c.author), now_ms as i64, c.seq as i64, envelope],
            )?;
            tx.commit()?;
            return Err(SyncError::Equivocation(c.author));
        }
        None => {}
    }
    if vector(&tx)?.includes(c.author, c.seq) {
        // Applied long ago and no longer listed (or it's our own change coming back).
        return Ok(Received::Duplicate);
    }
    let deps = serde_json::to_string(
        &c.deps
            .0
            .iter()
            .map(|(d, s)| (hex(*d), *s))
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .map_err(|_| SyncError::Protocol("encode dependencies"))?;
    tx.execute(
        "INSERT INTO sync_seen (author, seq, hash) VALUES (?1, ?2, ?3)",
        params![hex(c.author), c.seq as i64, opened.hash.as_slice()],
    )?;
    tx.execute(
        "INSERT INTO sync_inbox (author, seq, deps, hlc, envelope, received_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            hex(c.author),
            c.seq as i64,
            deps,
            c.hlc.0 as i64,
            envelope,
            now_ms as i64
        ],
    )?;
    tx.commit()?;
    Ok(Received::Stored)
}

/// [`receive`] for changes from the group: refuses an author that isn't a member, and a
/// removed author's changes after the last one accepted (§8.2), whatever route brought
/// them (direct, Relay, or passed on by another device).
pub fn receive_from_group(
    conn: &Connection,
    chain: &crate::membership::Chain,
    opened: &Opened,
    now_ms: u64,
) -> Result<Received> {
    chain.accepts(&opened.change)?;
    receive(conn, opened, now_ms)
}

struct InboxRow {
    author: DeviceId,
    seq: u64,
    deps: Vector,
    hlc: Hlc,
    envelope: Vec<u8>,
}

fn inbox_rows(conn: &Connection) -> Result<Vec<InboxRow>> {
    let mut stmt = conn.prepare(
        "SELECT author, seq, deps, hlc, envelope FROM sync_inbox ORDER BY hlc, author, seq",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Vec<u8>>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (a, s, d, h, e) = row?;
        let map: std::collections::BTreeMap<String, u64> =
            serde_json::from_str(&d).map_err(|_| SyncError::Protocol("damaged inbox row"))?;
        let mut deps = Vector::default();
        for (k, v) in map {
            deps.set(parse_device(&k)?, v);
        }
        out.push(InboxRow {
            author: parse_device(&a)?,
            seq: u64::try_from(s).unwrap_or(0),
            deps,
            hlc: Hlc(h as u64),
            envelope: e,
        });
    }
    Ok(out)
}

fn held_authors(conn: &Connection) -> Result<Vec<DeviceId>> {
    let mut stmt = conn.prepare("SELECT author FROM sync_held_authors")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.map(|r| parse_device(&r?)).collect()
}

/// The next change that can be applied now: its author's previous change and all its
/// dependencies are applied, its author isn't held, and its clock isn't too far ahead.
/// Returns the sealed envelope; the caller opens it, applies it, then calls [`applied`].
pub fn next_ready(conn: &Connection, now_ms: u64) -> Result<Option<(DeviceId, u64, Vec<u8>)>> {
    let v = vector(conn)?;
    let held = held_authors(conn)?;
    for row in inbox_rows(conn)? {
        if held.contains(&row.author) || row.hlc.too_far_ahead(now_ms) {
            continue;
        }
        if v.get(row.author) + 1 == row.seq && v.covers(&row.deps) {
            return Ok(Some((row.author, row.seq, row.envelope)));
        }
    }
    Ok(None)
}

/// Records that a change was applied: the vector moves on, the inbox row goes, and the
/// clock takes the change's time into account.
pub fn applied(conn: &Connection, change: &Change, now_ms: u64) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let v = vector(&tx)?;
    if v.get(change.author) + 1 != change.seq || !v.covers(&change.deps) {
        return Err(SyncError::Protocol("applied out of order"));
    }
    set_vector(&tx, change.author, change.seq)?;
    tx.execute(
        "DELETE FROM sync_inbox WHERE author = ?1 AND seq = ?2",
        params![hex(change.author), change.seq as i64],
    )?;
    let mut c = clock(&tx)?;
    if c.receive(change.hlc, now_ms).is_err() {
        return Err(SyncError::Protocol("applied a change from the future"));
    }
    save_clock(&tx, c)?;
    tx.commit()?;
    Ok(())
}

/// Why a change waits in the inbox (§9.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    /// Earlier changes it depends on haven't arrived yet.
    Dependencies,
    /// Its author's clock is more than 10 minutes ahead of this device's.
    ClockAhead,
    /// Its author sent conflicting changes; it waits for the user.
    AuthorHeld,
}

/// What waits, per author: how many and why (the most pressing reason first).
pub fn waiting(conn: &Connection, now_ms: u64) -> Result<Vec<(DeviceId, usize, WaitReason)>> {
    let held = held_authors(conn)?;
    let mut out: Vec<(DeviceId, usize, WaitReason)> = Vec::new();
    for row in inbox_rows(conn)? {
        let reason = if held.contains(&row.author) {
            WaitReason::AuthorHeld
        } else if row.hlc.too_far_ahead(now_ms) {
            WaitReason::ClockAhead
        } else {
            WaitReason::Dependencies
        };
        match out.iter_mut().find(|(a, _, _)| *a == row.author) {
            Some((_, n, r)) => {
                *n += 1;
                if reason == WaitReason::AuthorHeld || reason == WaitReason::ClockAhead {
                    *r = reason;
                }
            }
            None => out.push((row.author, 1, reason)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{GroupKey, open, seal};
    use crate::identity::{DeviceKeys, PublicIdentity};
    use dn_store::MigrationSet;
    use std::collections::BTreeMap;

    const GROUP: GroupId = GroupId([5; 16]);
    const NOW: u64 = 1_800_000_000_000;

    fn db() -> Connection {
        dn_store::open_in_memory(&[MigrationSet::new("sync", crate::migrations())]).unwrap()
    }

    /// A device: its database, keys and a toy state (last writer wins per field, ties by
    /// author), enough to check that every replica ends the same.
    struct Replica {
        conn: Connection,
        keys: DeviceKeys,
        state: BTreeMap<(String, String), (Hlc, DeviceId, serde_json::Value)>,
    }

    impl Replica {
        fn new() -> Self {
            Self {
                conn: db(),
                keys: DeviceKeys::generate().unwrap(),
                state: BTreeMap::new(),
            }
        }

        fn apply_ops(&mut self, c: &Change) {
            for op in &c.ops {
                if let Op::Put { id, fields, .. } = op {
                    for (f, v) in fields {
                        let k = (id.clone(), f.clone());
                        let new = (c.hlc, c.author, v.clone());
                        let wins = self
                            .state
                            .get(&k)
                            .is_none_or(|old| (old.0, old.1) < (new.0, new.1));
                        if wins {
                            self.state.insert(k, new);
                        }
                    }
                }
            }
        }

        fn write(&mut self, key: &GroupKey, id: &str, value: u32, now: u64) -> Vec<u8> {
            let ops = vec![Op::Put {
                collection: "chat".into(),
                id: id.into(),
                fields: BTreeMap::from([("title".into(), serde_json::json!(value))]),
            }];
            let c = next_change(&self.conn, self.keys.id(), GROUP, 0, ops, now).unwrap();
            self.apply_ops(&c);
            seal(&c, &self.keys, key).unwrap()
        }

        fn deliver(&mut self, env: &[u8], key: &GroupKey, members: &[PublicIdentity], now: u64) {
            let lookup = |id: DeviceId, _epoch: u32| {
                members
                    .iter()
                    .find(|m| m.id() == id)
                    .copied()
                    .ok_or(SyncError::UnknownDevice)
            };
            let opened = open(env, key, lookup).unwrap();
            receive(&self.conn, &opened, now).unwrap();
            while let Some((_, _, e)) = next_ready(&self.conn, now).unwrap() {
                let c = open(&e, key, lookup).unwrap().change;
                self.apply_ops(&c);
                applied(&self.conn, &c, now).unwrap();
            }
        }
    }

    /// xorshift64*, so the shuffles are random but repeatable without a test dependency.
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
    fn three_replicas_converge_whatever_order_changes_arrive_in() {
        for seed in 1..=60u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let key = GroupKey::generate(0).unwrap();
            let mut reps = [Replica::new(), Replica::new(), Replica::new()];
            let members: Vec<_> = reps.iter().map(|r| r.keys.public()).collect();
            // Each replica writes, sometimes after seeing some of the others' changes.
            let mut sent: Vec<(usize, Vec<u8>)> = Vec::new();
            for step in 0..30u32 {
                let who = rng.below(3);
                let id = ["c1", "c2", "c3"][rng.below(3)];
                let env = reps[who].write(&key, id, step, NOW + u64::from(step));
                sent.push((who, env));
                if rng.below(3) == 0 && !sent.is_empty() {
                    let (from, e) = sent[rng.below(sent.len())].clone();
                    let to = rng.below(3);
                    if to != from {
                        reps[to].deliver(&e, &key, &members, NOW + 100);
                    }
                }
            }
            // Then every replica gets everything, shuffled, with duplicates.
            for (r, rep) in reps.iter_mut().enumerate() {
                let mut all: Vec<&(usize, Vec<u8>)> =
                    sent.iter().filter(|(f, _)| *f != r).collect();
                for i in (1..all.len()).rev() {
                    all.swap(i, rng.below(i + 1));
                }
                let dup = all[rng.below(all.len())];
                all.push(dup);
                for (_, e) in all {
                    rep.deliver(e, &key, &members, NOW + 1_000);
                }
                assert!(next_ready(&rep.conn, NOW + 1_000).unwrap().is_none());
                assert!(
                    waiting(&rep.conn, NOW + 1_000).unwrap().is_empty(),
                    "seed {seed}"
                );
            }
            assert_eq!(reps[0].state, reps[1].state, "seed {seed}");
            assert_eq!(reps[1].state, reps[2].state, "seed {seed}");
            let v = vector(&reps[0].conn).unwrap();
            assert_eq!(v, vector(&reps[1].conn).unwrap());
            assert_eq!(v, vector(&reps[2].conn).unwrap());
        }
    }

    #[test]
    fn a_change_waits_for_what_it_depends_on() {
        let key = GroupKey::generate(0).unwrap();
        let mut a = Replica::new();
        let mut b = Replica::new();
        let members = [a.keys.public(), b.keys.public()];
        let first = a.write(&key, "c1", 1, NOW);
        let second = a.write(&key, "c1", 2, NOW + 1);
        b.deliver(&second, &key, &members, NOW);
        assert_eq!(
            waiting(&b.conn, NOW).unwrap(),
            vec![(a.keys.id(), 1, WaitReason::Dependencies)]
        );
        assert!(b.state.is_empty());
        b.deliver(&first, &key, &members, NOW);
        assert!(waiting(&b.conn, NOW).unwrap().is_empty());
        assert_eq!(b.state, a.state);
    }

    #[test]
    fn a_replayed_envelope_does_nothing() {
        let key = GroupKey::generate(0).unwrap();
        let mut a = Replica::new();
        let b = Replica::new();
        let env = a.write(&key, "c1", 1, NOW);
        let opened = open(&env, &key, |_, _| Ok(a.keys.public())).unwrap();
        assert_eq!(receive(&b.conn, &opened, NOW).unwrap(), Received::Stored);
        assert_eq!(receive(&b.conn, &opened, NOW).unwrap(), Received::Duplicate);
        // The author's own change coming back is a duplicate too.
        assert_eq!(receive(&a.conn, &opened, NOW).unwrap(), Received::Duplicate);
    }

    #[test]
    fn two_histories_under_one_number_are_caught_and_the_author_is_held() {
        let key = GroupKey::generate(0).unwrap();
        let mut a = Replica::new();
        let b = Replica::new();
        let one = a.write(&key, "c1", 1, NOW);
        // `a` signs a different change with the same number (as if its key were misused).
        let fork = Change {
            ops: vec![],
            ..open(&one, &key, |_, _| Ok(a.keys.public())).unwrap().change
        };
        let fork_env = seal(&fork, &a.keys, &key).unwrap();
        let lookup = |_, _| Ok(a.keys.public());
        let o1 = open(&one, &key, lookup).unwrap();
        let o2 = open(&fork_env, &key, lookup).unwrap();
        receive(&b.conn, &o1, NOW).unwrap();
        assert!(matches!(
            receive(&b.conn, &o2, NOW),
            Err(SyncError::Equivocation(d)) if d == a.keys.id()
        ));
        // Its changes are held now, including the first one, until the user decides.
        assert!(next_ready(&b.conn, NOW).unwrap().is_none());
        assert_eq!(
            waiting(&b.conn, NOW).unwrap(),
            vec![(a.keys.id(), 1, WaitReason::AuthorHeld)]
        );
    }

    #[test]
    fn a_change_re_sealed_by_another_member_is_a_duplicate_not_a_conflict() {
        let key = GroupKey::generate(0).unwrap();
        let mut v = Replica::new();
        let b = Replica::new();
        let original = v.write(&key, "c1", 1, NOW);
        let opened = open(&original, &key, |_, _| Ok(v.keys.public())).unwrap();
        receive(&b.conn, &opened, NOW).unwrap();
        // A member with the group key re-seals V's signed change with a fresh salt.
        let resealed = seal(&opened.change, &v.keys, &key).unwrap();
        assert_ne!(resealed, original);
        let again = open(&resealed, &key, |_, _| Ok(v.keys.public())).unwrap();
        assert_eq!(receive(&b.conn, &again, NOW).unwrap(), Received::Duplicate);
        assert!(
            waiting(&b.conn, NOW)
                .unwrap()
                .iter()
                .all(|w| w.2 != WaitReason::AuthorHeld)
        );
    }

    #[test]
    fn a_fork_of_this_devices_own_change_is_caught() {
        let key = GroupKey::generate(0).unwrap();
        let mut a = Replica::new();
        let mine = a.write(&key, "c1", 1, NOW);
        let opened = open(&mine, &key, |_, _| Ok(a.keys.public())).unwrap();
        assert_eq!(receive(&a.conn, &opened, NOW).unwrap(), Received::Duplicate);
        // Someone with this device's key signs a different change under the same number.
        let fork = Change {
            ops: vec![],
            ..opened.change
        };
        let forged = open(&seal(&fork, &a.keys, &key).unwrap(), &key, |_, _| {
            Ok(a.keys.public())
        })
        .unwrap();
        assert!(matches!(
            receive(&a.conn, &forged, NOW),
            Err(SyncError::Equivocation(_))
        ));
    }

    #[test]
    fn a_change_from_the_future_waits_until_the_clock_allows() {
        let key = GroupKey::generate(0).unwrap();
        let mut a = Replica::new();
        let mut b = Replica::new();
        let members = [a.keys.public(), b.keys.public()];
        let ahead = NOW + 2 * 60 * 60 * 1000;
        let env = a.write(&key, "c1", 1, ahead);
        b.deliver(&env, &key, &members, NOW);
        assert!(b.state.is_empty());
        assert_eq!(
            waiting(&b.conn, NOW).unwrap(),
            vec![(a.keys.id(), 1, WaitReason::ClockAhead)]
        );
        // b's clock didn't move to match.
        assert!(clock(&b.conn).unwrap().last.millis() < ahead);
        // Later, b's clock has caught up and the change applies.
        b.deliver(&env, &key, &members, ahead);
        assert!(waiting(&b.conn, ahead).unwrap().is_empty());
        assert_eq!(b.state, a.state);
    }

    #[test]
    fn a_removed_devices_later_changes_are_refused_at_the_inbox() {
        use crate::devices::DeviceKind;
        use crate::membership::NodeChain;
        let node = DeviceKeys::generate().unwrap();
        let mut nc = NodeChain::create(&node, "Home server").unwrap();
        let laptop = Replica::new();
        let mut phone = Replica::new();
        nc.add(
            &node,
            &laptop.keys.public(),
            "Laptop",
            DeviceKind::Desktop,
            None,
        )
        .unwrap();
        nc.add(
            &node,
            &phone.keys.public(),
            "Phone",
            DeviceKind::Phone,
            None,
        )
        .unwrap();
        let group = nc.chain().group();
        let k0 = nc.chain().key_for(&phone.keys, 0).unwrap();
        let write = |p: &mut Replica| {
            let c = next_change(&p.conn, p.keys.id(), group, 0, vec![], NOW).unwrap();
            seal(&c, &p.keys, &k0).unwrap()
        };
        let first = write(&mut phone);
        let second = write(&mut phone);
        // Removed after its first change.
        nc.remove(&node, phone.keys.id(), laptop.keys.id(), 1)
            .unwrap();
        let chain = nc.chain();
        let lookup = |a, _| chain.author_key(a);
        let o1 = open(&first, &k0, lookup).unwrap();
        let o2 = open(&second, &k0, lookup).unwrap();
        assert_eq!(
            receive_from_group(&laptop.conn, chain, &o1, NOW).unwrap(),
            Received::Stored
        );
        assert!(matches!(
            receive_from_group(&laptop.conn, chain, &o2, NOW),
            Err(SyncError::Removed)
        ));
        // A stranger's change is refused too.
        let stranger = Replica::new();
        let c = next_change(&stranger.conn, stranger.keys.id(), group, 0, vec![], NOW).unwrap();
        let env = seal(&c, &stranger.keys, &k0).unwrap();
        let o = open(&env, &k0, |_, _| Ok(stranger.keys.public())).unwrap();
        assert!(matches!(
            receive_from_group(&laptop.conn, chain, &o, NOW),
            Err(SyncError::UnknownDevice)
        ));
    }

    #[test]
    fn this_devices_changes_are_numbered_in_order_with_the_vector() {
        let conn = db();
        let me = DeviceKeys::generate().unwrap().id();
        let c1 = next_change(&conn, me, GROUP, 0, vec![], NOW).unwrap();
        let c2 = next_change(&conn, me, GROUP, 0, vec![], NOW).unwrap();
        assert_eq!((c1.seq, c2.seq), (1, 2));
        assert_eq!(c2.deps.get(me), 1);
        assert!(c2.hlc > c1.hlc);
        assert_eq!(vector(&conn).unwrap().get(me), 2);
    }
}
