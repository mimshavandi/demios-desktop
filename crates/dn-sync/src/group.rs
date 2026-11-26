//! The device group as each side keeps it (docs/design/dn-sync.md §8).
//!
//! - [`NodeGroup`], on the Node:
//!   - it creates the group on first start and keeps the chain in `sync_membership`;
//!   - it keeps every epoch's group key in the Node's secret store;
//!   - it adds a device when pairing is confirmed;
//!   - removing a device names the last change applied from it and rotates the key.
//! - [`ClientGroup`], on a paired device:
//!   - it keeps the Node's chain, checked against the Node pinned at pairing;
//!   - it gets each epoch's key from the chain, unwrapped with this device's key, and keeps
//!     it in the device's secret store.

use crate::devices::DeviceKind;
use crate::envelope::GroupKey;
use crate::identity::{DeviceId, DeviceKeys, PublicIdentity};
use crate::membership::{self, Chain, Entry, NodeChain};
use crate::{Result, SyncError};
use dn_secrets::SecretStore;
use rusqlite::Connection;

/// The group on the Node.
pub struct NodeGroup {
    nc: NodeChain,
}

impl NodeGroup {
    /// Loads the Node's group, or creates it (with epoch 0's key) on first start.
    pub fn load_or_create(
        conn: &Connection,
        store: &dyn SecretStore,
        me: &DeviceKeys,
        name: &str,
    ) -> Result<Self> {
        if let Some(chain) = membership::load(conn, &me.public())? {
            let key = membership::load_key(store, &chain.group(), chain.epoch())?
                .ok_or(SyncError::DamagedKeys("the Node's group key is missing"))?;
            return Ok(Self {
                nc: NodeChain::from_parts(chain, key)?,
            });
        }
        let nc = NodeChain::create(me, name)?;
        membership::store_key(store, &nc.chain().group(), nc.key())?;
        membership::save(conn, nc.chain())?;
        Ok(Self { nc })
    }

    pub fn chain(&self) -> &Chain {
        self.nc.chain()
    }

    /// The group key of `epoch` (the Node keeps all of them).
    pub fn key(&self, store: &dyn SecretStore, epoch: u32) -> Result<GroupKey> {
        if epoch == self.nc.key().epoch {
            return Ok(GroupKey::from_bytes(epoch, self.nc.key_bytes()));
        }
        membership::load_key(store, &self.chain().group(), epoch)?
            .ok_or(SyncError::Protocol("no group key for that epoch"))
    }

    /// Adds a paired device, giving it every epoch's key (so it can read the group's
    /// history), and saves the chain.
    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        conn: &Connection,
        store: &dyn SecretStore,
        me: &DeviceKeys,
        keys: &PublicIdentity,
        name: &str,
        kind: DeviceKind,
        approved_by: Option<DeviceId>,
    ) -> Result<()> {
        let earlier = (0..self.nc.key().epoch)
            .map(|e| self.key(store, e))
            .collect::<Result<Vec<_>>>()?;
        self.nc.add(me, keys, name, kind, approved_by, &earlier)?;
        membership::save(conn, self.nc.chain())
    }

    /// Removes a device: changes from it after the last one this Node holds are refused
    /// from now on, and the group key rotates to the remaining members. Saves both.
    pub fn remove(
        &mut self,
        conn: &Connection,
        store: &dyn SecretStore,
        me: &DeviceKeys,
        device: DeviceId,
        requested_by: DeviceId,
    ) -> Result<()> {
        let last = crate::session::held(conn)?.get(device);
        self.nc.remove(me, device, requested_by, last)?;
        membership::store_key(store, &self.chain().group(), self.nc.key())?;
        membership::save(conn, self.nc.chain())
    }
}

/// The group on a paired device.
pub struct ClientGroup {
    chain: Chain,
}

impl ClientGroup {
    /// Loads the chain this device holds for the Node `node`, if any.
    pub fn load(conn: &Connection, node: &PublicIdentity) -> Result<Option<Self>> {
        Ok(membership::load(conn, node)?.map(|chain| Self { chain }))
    }

    /// Starts from the Node's first entries (right after pairing).
    pub fn from_entries(
        conn: &Connection,
        node: &PublicIdentity,
        entries: &[Entry],
    ) -> Result<Self> {
        let chain = Chain::from_entries(entries, node)?;
        membership::save(conn, &chain)?;
        Ok(Self { chain })
    }

    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    /// Appends entries from the Node (checked: signature, link, fork) and saves them.
    pub fn extend(&mut self, conn: &Connection, entries: Vec<Entry>) -> Result<()> {
        for e in entries {
            self.chain.append(e)?;
        }
        membership::save(conn, &self.chain)
    }

    /// This device's key for `epoch`: from the secret store, or unwrapped from the chain
    /// and kept there.
    pub fn key(&self, store: &dyn SecretStore, me: &DeviceKeys, epoch: u32) -> Result<GroupKey> {
        if let Some(k) = membership::load_key(store, &self.chain.group(), epoch)? {
            return Ok(k);
        }
        let k = self.chain.key_for(me, epoch)?;
        membership::store_key(store, &self.chain.group(), &k)?;
        Ok(k)
    }

    /// Whether this device is still a member (not removed).
    pub fn is_member(&self, me: DeviceId) -> bool {
        self.chain.member(me).is_some_and(|m| m.removed.is_none())
    }
}
