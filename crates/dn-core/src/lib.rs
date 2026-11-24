//! Shared domain types for demios-desktop. Every other crate depends on these;
//! changes here are contract changes and need an ADR reference in the PR.
//!
//! Modules map to ADRs: [`model`] → ADR-0003/0005, [`chat`] → ADR-0003,
//! [`artifact`] → ADR-0004, [`capability`] → ADR-0006, [`event`] → ADR-0008.

pub mod artifact;
pub mod capability;
pub mod chat;
pub mod error;
pub mod event;
pub mod id;
pub mod model;

pub use error::{CoreError, RecoveryAction};

/// Reverse-DNS application id (demios.io). Used for the Tauri bundle identifier, the
/// OS app-data directory (`dn-store`) and the keychain service name (`dn-secrets`).
/// Changing it orphans users' data and stored keys.
pub const APP_ID: &str = "io.demios.desktop";
pub use id::Id;
