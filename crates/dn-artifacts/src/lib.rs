//! dn-artifacts — Claude-style artifacts (ADR-0004): an append-only SQLite store with
//! exact-match patching and rollback, the model tool protocol and the fenced fallback parser.
//!
//! The store works on a `&rusqlite::Connection` the app passes in; [`migrations`] creates this
//! crate's `artifact_*` tables and is run by `dn-store`. Contract types live in
//! [`dn_core::artifact`].

pub mod error;
pub mod fenced;
pub mod protocol;
pub mod store;

pub use error::{ArtifactError, Result};
pub use fenced::{FenceEvent, FenceHeader, FencedArtifact, FencedParser};
pub use protocol::{apply_fenced, execute_tool_call, op_from_tool_call, tool_result, tool_specs};
pub use store::{Applied, ArtifactStore, ArtifactSummary, OpContext, migrations};
