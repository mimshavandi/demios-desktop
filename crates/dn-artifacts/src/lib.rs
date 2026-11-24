//! dn-artifacts — Claude-style artifacts (ADR-0004): an append-only SQLite store with
//! exact-match patching and rollback.
//!
//! The store works on a `&rusqlite::Connection` the app passes in; [`migrations`] creates this
//! crate's `artifact_*` tables and is run by `dn-store`. Contract types live in
//! [`dn_core::artifact`].

pub mod error;
pub mod store;

pub use error::{ArtifactError, Result};
pub use store::{Applied, ArtifactStore, ArtifactSummary, OpContext, migrations};
