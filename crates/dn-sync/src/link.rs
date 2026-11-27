//! On a client (the desktop app): the Node this device is paired with, kept in
//! `sync_node_link` with the Node's keys in `sync_devices`.

use crate::devices::{Device, DeviceKind, DeviceList};
use crate::identity::{DeviceId, PublicIdentity};
use crate::{Result, SyncError};
use rusqlite::{Connection, OptionalExtension, params};

/// The paired Node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeLink {
    pub node: Device,
    /// `host:port`, from the pairing link.
    pub address: String,
    /// Unix milliseconds.
    pub paired_at: i64,
    pub last_reached_at: Option<i64>,
}

/// The paired Node, if any.
pub fn get(conn: &Connection) -> Result<Option<NodeLink>> {
    let row = conn
        .query_row(
            "SELECT node_id, address, paired_at, last_reached_at FROM sync_node_link WHERE id = 1",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((node_id, address, paired_at, last_reached_at)) = row else {
        return Ok(None);
    };
    let id = DeviceId::from_hex(&node_id).ok_or(SyncError::UnknownDevice)?;
    let node = DeviceList::get(conn, id)?.ok_or(SyncError::UnknownDevice)?;
    Ok(Some(NodeLink {
        node,
        address,
        paired_at,
        last_reached_at,
    }))
}

/// Records the Node after it accepted this device. Refuses if one is already paired.
pub fn save(
    conn: &Connection,
    node: &PublicIdentity,
    name: &str,
    address: &str,
    now_ms: i64,
) -> Result<NodeLink> {
    let tx = conn.unchecked_transaction()?;
    if get(&tx)?.is_some() {
        return Err(SyncError::AlreadyPaired);
    }
    DeviceList::add(&tx, node, name, DeviceKind::Node, None, now_ms)?;
    tx.execute(
        "INSERT INTO sync_node_link (id, node_id, address, paired_at, last_reached_at)
         VALUES (1, ?1, ?2, ?3, ?3)",
        params![node.id().to_hex(), address, now_ms],
    )?;
    tx.commit()?;
    get(conn)?.ok_or(SyncError::UnknownDevice)
}

/// Notes that the Node answered, and its current name.
pub fn reached(conn: &Connection, name: &str, now_ms: i64) -> Result<()> {
    let Some(link) = get(conn)? else {
        return Err(SyncError::UnknownDevice);
    };
    if link.node.name != name.trim() {
        DeviceList::rename(conn, link.node.id, name)?;
    }
    conn.execute(
        "UPDATE sync_node_link SET last_reached_at = ?1 WHERE id = 1",
        [now_ms],
    )?;
    Ok(())
}

/// Forgets the Node and the devices this client knew from it (unpairing).
pub fn forget(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM sync_node_link", [])?;
    tx.execute("DELETE FROM sync_devices", [])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceKeys;
    use dn_store::MigrationSet;

    #[test]
    fn a_client_keeps_one_node_and_can_forget_it() {
        let conn =
            dn_store::open_in_memory(&[MigrationSet::new("sync", crate::migrations())]).unwrap();
        assert_eq!(get(&conn).unwrap(), None);
        let node = DeviceKeys::generate().unwrap().public();
        let link = save(&conn, &node, "Node", "nas:7443", 5).unwrap();
        assert_eq!(link.address, "nas:7443");
        assert_eq!(link.node.kind, DeviceKind::Node);
        let other = DeviceKeys::generate().unwrap().public();
        assert!(matches!(
            save(&conn, &other, "N2", "x:1", 6),
            Err(SyncError::AlreadyPaired)
        ));
        reached(&conn, "Home server", 9).unwrap();
        let link = get(&conn).unwrap().unwrap();
        assert_eq!(
            (link.node.name.as_str(), link.last_reached_at),
            ("Home server", Some(9))
        );
        forget(&conn).unwrap();
        assert_eq!(get(&conn).unwrap(), None);
        // The same Node can be paired again after forgetting it.
        save(&conn, &node, "Node", "nas:7443", 10).unwrap();
    }
}
