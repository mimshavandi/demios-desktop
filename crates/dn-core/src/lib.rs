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
pub use id::Id;
