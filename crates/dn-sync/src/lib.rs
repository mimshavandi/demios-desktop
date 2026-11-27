//! dn-sync: device identity, pairing and (later) sync between a user's devices and their
//! Demios Node (ADR-0014, design note `docs/design/dn-sync.md`).
//!
//! This first part has no network code:
//! - [`identity`]: this device's long-term key pairs, kept in the secret store.
//! - [`pairing`]: the pairing link (QR code payload), the one-time secret, the proof that a
//!   joining device knows it, and the check numbers both sides show.
//! - [`devices`]: the device list of a group, stored in `sync_devices`.
//! - [`hlc`], [`change`], [`envelope`], [`inbox`]: sync step 1 (design note §14): the clock,
//!   change numbers and vectors, signed and sealed envelopes, and the inbox that applies
//!   changes in causal order.
//! - [`transport`]: the paired channel (TLS 1.3, each side pinning the other's key), its
//!   version handshake, and pairing over it.
//! - [`membership`], [`group`]: the Node-signed membership chain and the group key (step 2).
//! - [`records`]: synced records, conflicts and tombstones (step 3).
//! - [`session`]: a sync round between a device and its Node on the direct route (step 4).

pub mod b64;
pub mod change;
pub mod devices;
pub mod envelope;
pub mod group;
pub mod hlc;
pub mod identity;
pub mod inbox;
pub mod link;
pub mod membership;
pub mod pairing;
pub mod records;
pub mod session;
pub mod transport;

pub use devices::{Device, DeviceKind, DeviceList};
pub use identity::{DeviceId, DeviceKeys, PublicIdentity};
pub use pairing::{CheckNumbers, PairingLink, PairingOffer};

/// Schema migrations for this crate (tables prefixed `sync_`). `dn-store` runs them.
pub fn migrations() -> &'static [&'static str] {
    &[
        devices::MIGRATION_DEVICES,
        inbox::MIGRATION,
        devices::MIGRATION_NODE_LINK,
        membership::MIGRATION,
        records::MIGRATION,
        session::MIGRATION,
    ]
}

/// What can go wrong in this crate. No variant carries key material.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("the secret store failed: {0}")]
    Secrets(#[from] dn_secrets::SecretError),
    #[error("this device's stored keys are damaged ({0}); they were not replaced")]
    DamagedKeys(&'static str),
    #[error("the crypto library failed ({0})")]
    Crypto(&'static str),
    #[error("this isn't a valid pairing link: {0}")]
    BadLink(&'static str),
    #[error("this pairing link is for a newer version of Demios; update this app")]
    NewerLinkVersion,
    #[error("this pairing code has expired or was already used")]
    OfferClosed,
    #[error("the device at that address isn't the Node from the pairing link")]
    NodeKeyMismatch,
    #[error("this device couldn't prove it has the pairing code")]
    WrongProof,
    #[error("signature check failed")]
    BadSignature,
    #[error("device name must be 1 to 64 characters with no control characters")]
    BadName,
    #[error("that device is already in the list")]
    AlreadyPaired,
    #[error("that device isn't in the list")]
    UnknownDevice,
    #[error("that device was removed and can't be paired again with the same keys")]
    Removed,
    #[error("the connection failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("the secure connection failed: {0}")]
    Tls(String),
    #[error("the other device broke the protocol: {0}")]
    Protocol(&'static str),
    #[error("{0}")]
    VersionMismatch(VersionMismatch),
    #[error("the Node didn't accept this device")]
    Declined,
    #[error(
        "the membership chain has two different entries at one place; stop and check the Node's key"
    )]
    ForkedChain,
    #[error("device {0} sent two different changes under one number")]
    Equivocation(DeviceId),
    #[error("the Node refused the sync: {0}")]
    Refused(String),
    #[error("the local store refused a synced record: {0}")]
    Store(String),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
}

/// Protocol majors that differ. The message names the side to update (ADR-0014 §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionMismatch {
    pub ours: u16,
    pub theirs: u16,
}

impl std::fmt::Display for VersionMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let side = if self.theirs > self.ours {
            "Update this app to sync"
        } else {
            "Update the other device to sync"
        };
        write!(
            f,
            "the other device speaks sync version {} and this one {}. {side}",
            self.theirs, self.ours
        )
    }
}

pub type Result<T> = std::result::Result<T, SyncError>;
