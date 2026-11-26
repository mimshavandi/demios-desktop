//! dn-sync: device identity, pairing and (later) sync between a user's devices and their
//! Demios Node (ADR-0014, design note `docs/design/dn-sync.md`).
//!
//! This first part has no network code:
//! - [`identity`]: this device's long-term key pairs, kept in the secret store.
//! - [`pairing`]: the pairing link (QR code payload), the one-time secret, the proof that a
//!   joining device knows it, and the check numbers both sides show.
//! - [`devices`]: the device list of a group, stored in `sync_devices`.

pub mod b64;
pub mod devices;
pub mod identity;
pub mod pairing;

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
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, SyncError>;
