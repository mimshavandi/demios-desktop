//! dn-sync: device identity, pairing and (later) sync between a user's devices and their
//! Demios Node (ADR-0014, design note `docs/design/dn-sync.md`).
//!
//! This first part has no network code:
//! - [`identity`]: this device's long-term key pairs, kept in the secret store.
//! - [`pairing`]: the pairing link (QR code payload), the one-time secret, the proof that a
//!   joining device knows it, and the check numbers both sides show.
//! - [`devices`]: the device list of a group, stored in `sync_devices`.
//! - [`transport`]: the paired channel (TLS 1.3, each side pinning the other's key), its
//!   version handshake, and pairing over it.

pub mod b64;
pub mod devices;
pub mod identity;
pub mod pairing;
pub mod transport;

pub use devices::{Device, DeviceKind, DeviceList};
pub use identity::{DeviceId, DeviceKeys, PublicIdentity};
pub use pairing::{CheckNumbers, PairingLink, PairingOffer};

/// Schema migrations for this crate (tables prefixed `sync_`). `dn-store` runs them.
pub fn migrations() -> &'static [&'static str] {
    devices::MIGRATIONS
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
