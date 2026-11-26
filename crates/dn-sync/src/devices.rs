//! The device group's list of paired devices (ADR-0014 §2), stored in `sync_devices`.
//!
//! A removed device keeps its row with `removed_at` set, so its keys are refused for good:
//! it can't connect, sign, or be paired again with the same keys. Its local copy isn't
//! touched; the UI says so before removal.

use crate::identity::{DeviceId, PublicIdentity};
use crate::{Result, SyncError};
use rusqlite::{Connection, OptionalExtension, Row, params};

pub(crate) const MIGRATIONS: &[&str] = &["
CREATE TABLE sync_devices (
    id             TEXT    PRIMARY KEY NOT NULL,
    ed25519        BLOB    NOT NULL CHECK (length(ed25519) = 32),
    x25519         BLOB    NOT NULL CHECK (length(x25519) = 32),
    name           TEXT    NOT NULL,
    kind           TEXT    NOT NULL CHECK (kind IN ('desktop', 'node', 'phone')),
    approved_by    TEXT,
    added_at       INTEGER NOT NULL,
    removed_at     INTEGER,
    last_synced_at INTEGER
);
-- One row per signing key, removed rows included, so a removed device can't come back
-- with the same signing key and a new agreement key.
CREATE UNIQUE INDEX sync_devices_ed25519 ON sync_devices (ed25519);
"];

/// Longest device name, in characters.
pub const NAME_MAX: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Desktop,
    Node,
    Phone,
}

impl DeviceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Node => "node",
            Self::Phone => "phone",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "desktop" => Self::Desktop,
            "node" => Self::Node,
            "phone" => Self::Phone,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: DeviceId,
    pub keys: PublicIdentity,
    pub name: String,
    pub kind: DeviceKind,
    /// The device whose user confirmed the check numbers; `None` for the Node itself.
    pub approved_by: Option<DeviceId>,
    /// Unix milliseconds.
    pub added_at: i64,
    pub removed_at: Option<i64>,
    pub last_synced_at: Option<i64>,
}

impl Device {
    pub fn is_member(&self) -> bool {
        self.removed_at.is_none()
    }
}

pub(crate) fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    let len = name.chars().count();
    if len == 0 || len > NAME_MAX || name.chars().any(char::is_control) {
        return Err(SyncError::BadName);
    }
    Ok(name.to_owned())
}

fn blob32(row: &Row<'_>, col: &str) -> rusqlite::Result<[u8; 32]> {
    let v: Vec<u8> = row.get(col)?;
    v.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Blob,
            "key length".into(),
        )
    })
}

fn bad_text(what: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, what.into())
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Device> {
    let id: String = row.get("id")?;
    let kind: String = row.get("kind")?;
    let approved_by: Option<String> = row.get("approved_by")?;
    let approved_by = match approved_by {
        Some(text) => Some(DeviceId::from_hex(&text).ok_or_else(|| bad_text("approved_by"))?),
        None => None,
    };
    Ok(Device {
        id: DeviceId::from_hex(&id).ok_or_else(|| bad_text("id"))?,
        keys: PublicIdentity {
            ed25519: blob32(row, "ed25519")?,
            x25519: blob32(row, "x25519")?,
        },
        name: row.get("name")?,
        kind: DeviceKind::parse(&kind).ok_or_else(|| bad_text("kind"))?,
        approved_by,
        added_at: row.get("added_at")?,
        removed_at: row.get("removed_at")?,
        last_synced_at: row.get("last_synced_at")?,
    })
}

/// Reads and changes the device list. Every call takes the app's connection.
pub struct DeviceList;

impl DeviceList {
    /// Adds a device after pairing. Refuses a device that's already listed, and keys
    /// that were removed before.
    pub fn add(
        conn: &Connection,
        keys: &PublicIdentity,
        name: &str,
        kind: DeviceKind,
        approved_by: Option<DeviceId>,
        now_ms: i64,
    ) -> Result<Device> {
        let name = clean_name(name)?;
        let id = keys.id();
        let same_signing_key = Self::by_signing_key(conn, &keys.ed25519)?;
        if let Some(existing) = same_signing_key.or(Self::get(conn, id)?) {
            return Err(if existing.is_member() {
                SyncError::AlreadyPaired
            } else {
                SyncError::Removed
            });
        }
        conn.execute(
            "INSERT INTO sync_devices (id, ed25519, x25519, name, kind, approved_by, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id.to_hex(),
                keys.ed25519.as_slice(),
                keys.x25519.as_slice(),
                name,
                kind.as_str(),
                approved_by.map(|d| d.to_hex()),
                now_ms
            ],
        )?;
        Self::get(conn, id)?.ok_or(SyncError::UnknownDevice)
    }

    pub fn get(conn: &Connection, id: DeviceId) -> Result<Option<Device>> {
        Ok(conn
            .query_row(
                "SELECT * FROM sync_devices WHERE id = ?1",
                [id.to_hex()],
                from_row,
            )
            .optional()?)
    }

    /// The device with this Ed25519 key, removed or not (the TLS peer key on a connection).
    pub fn by_signing_key(conn: &Connection, ed25519: &[u8; 32]) -> Result<Option<Device>> {
        Ok(conn
            .query_row(
                "SELECT * FROM sync_devices WHERE ed25519 = ?1",
                [ed25519.as_slice()],
                from_row,
            )
            .optional()?)
    }

    /// Current members, oldest first.
    pub fn members(conn: &Connection) -> Result<Vec<Device>> {
        let mut stmt = conn
            .prepare("SELECT * FROM sync_devices WHERE removed_at IS NULL ORDER BY added_at, id")?;
        let rows = stmt.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Removes a device: its keys are refused from now on. Removing it again changes
    /// nothing.
    pub fn remove(conn: &Connection, id: DeviceId, now_ms: i64) -> Result<()> {
        if Self::get(conn, id)?.is_none() {
            return Err(SyncError::UnknownDevice);
        }
        conn.execute(
            "UPDATE sync_devices SET removed_at = ?2 WHERE id = ?1 AND removed_at IS NULL",
            params![id.to_hex(), now_ms],
        )?;
        Ok(())
    }

    pub fn rename(conn: &Connection, id: DeviceId, name: &str) -> Result<()> {
        let name = clean_name(name)?;
        let changed = conn.execute(
            "UPDATE sync_devices SET name = ?2 WHERE id = ?1 AND removed_at IS NULL",
            params![id.to_hex(), name],
        )?;
        if changed == 0 {
            return Err(SyncError::UnknownDevice);
        }
        Ok(())
    }

    pub fn mark_synced(conn: &Connection, id: DeviceId, now_ms: i64) -> Result<()> {
        conn.execute(
            "UPDATE sync_devices SET last_synced_at = ?2 WHERE id = ?1 AND removed_at IS NULL",
            params![id.to_hex(), now_ms],
        )?;
        Ok(())
    }

    /// Checks that `message` was signed by `id` and that `id` is a current member. A
    /// removed or unknown device is refused even with a valid signature.
    pub fn verify_member(
        conn: &Connection,
        id: DeviceId,
        message: &[u8],
        signature: &[u8],
    ) -> Result<Device> {
        let device = Self::get(conn, id)?.ok_or(SyncError::UnknownDevice)?;
        if !device.is_member() {
            return Err(SyncError::Removed);
        }
        device.keys.verify(message, signature)?;
        Ok(device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceKeys;
    use dn_store::MigrationSet;

    fn db() -> Connection {
        dn_store::open_in_memory(&[MigrationSet::new("sync", crate::migrations())]).unwrap()
    }

    #[test]
    fn paired_devices_are_listed_and_can_be_renamed() {
        let conn = db();
        let node = DeviceKeys::generate().unwrap();
        let laptop = DeviceKeys::generate().unwrap();
        DeviceList::add(
            &conn,
            &node.public(),
            "Home server",
            DeviceKind::Node,
            None,
            1,
        )
        .unwrap();
        let added = DeviceList::add(
            &conn,
            &laptop.public(),
            "  MacBook Pro ",
            DeviceKind::Desktop,
            Some(node.id()),
            2,
        )
        .unwrap();
        assert_eq!(added.name, "MacBook Pro");
        assert_eq!(added.approved_by, Some(node.id()));
        assert_eq!(added.keys, laptop.public());
        DeviceList::rename(&conn, laptop.id(), "Work laptop").unwrap();
        DeviceList::mark_synced(&conn, laptop.id(), 9).unwrap();
        let members = DeviceList::members(&conn).unwrap();
        assert_eq!(
            members.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            ["Home server", "Work laptop"]
        );
        assert_eq!(members[1].last_synced_at, Some(9));
    }

    #[test]
    fn a_device_is_paired_once() {
        let conn = db();
        let k = DeviceKeys::generate().unwrap();
        DeviceList::add(&conn, &k.public(), "Phone", DeviceKind::Phone, None, 1).unwrap();
        assert!(matches!(
            DeviceList::add(&conn, &k.public(), "Phone", DeviceKind::Phone, None, 2),
            Err(SyncError::AlreadyPaired)
        ));
    }

    #[test]
    fn a_removed_device_is_refused_for_good() {
        let conn = db();
        let phone = DeviceKeys::generate().unwrap();
        DeviceList::add(
            &conn,
            &phone.public(),
            "Work phone",
            DeviceKind::Phone,
            None,
            1,
        )
        .unwrap();
        let msg = b"change 7";
        let sig = phone.sign(msg);
        DeviceList::verify_member(&conn, phone.id(), msg, &sig).unwrap();

        DeviceList::remove(&conn, phone.id(), 5).unwrap();
        DeviceList::remove(&conn, phone.id(), 6).unwrap();
        assert_eq!(
            DeviceList::get(&conn, phone.id())
                .unwrap()
                .unwrap()
                .removed_at,
            Some(5)
        );
        assert!(DeviceList::members(&conn).unwrap().is_empty());
        // A valid signature from a removed device is refused, and so is re-pairing it.
        assert!(matches!(
            DeviceList::verify_member(&conn, phone.id(), msg, &sig),
            Err(SyncError::Removed)
        ));
        assert!(matches!(
            DeviceList::add(
                &conn,
                &phone.public(),
                "Work phone",
                DeviceKind::Phone,
                None,
                7
            ),
            Err(SyncError::Removed)
        ));
        // Nor can it come back with its signing key and a new agreement key.
        let mut reused = phone.public();
        reused.x25519 = DeviceKeys::generate().unwrap().public().x25519;
        assert!(matches!(
            DeviceList::add(&conn, &reused, "Work phone", DeviceKind::Phone, None, 8),
            Err(SyncError::Removed)
        ));
        assert!(matches!(
            DeviceList::rename(&conn, phone.id(), "x"),
            Err(SyncError::UnknownDevice)
        ));
    }

    #[test]
    fn unknown_devices_and_forged_signatures_are_refused() {
        let conn = db();
        let member = DeviceKeys::generate().unwrap();
        let stranger = DeviceKeys::generate().unwrap();
        DeviceList::add(
            &conn,
            &member.public(),
            "Laptop",
            DeviceKind::Desktop,
            None,
            1,
        )
        .unwrap();
        assert!(matches!(
            DeviceList::verify_member(&conn, stranger.id(), b"m", &stranger.sign(b"m")),
            Err(SyncError::UnknownDevice)
        ));
        // A stranger signing in a member's name.
        assert!(matches!(
            DeviceList::verify_member(&conn, member.id(), b"m", &stranger.sign(b"m")),
            Err(SyncError::BadSignature)
        ));
        assert!(matches!(
            DeviceList::remove(&conn, stranger.id(), 2),
            Err(SyncError::UnknownDevice)
        ));
    }

    #[test]
    fn names_are_checked() {
        let conn = db();
        let k = DeviceKeys::generate().unwrap();
        for bad in ["", "   ", "a\nb", &"x".repeat(NAME_MAX + 1)] {
            assert!(matches!(
                DeviceList::add(&conn, &k.public(), bad, DeviceKind::Phone, None, 1),
                Err(SyncError::BadName)
            ));
        }
        DeviceList::add(
            &conn,
            &k.public(),
            &"é".repeat(NAME_MAX),
            DeviceKind::Phone,
            None,
            1,
        )
        .unwrap();
    }
}
