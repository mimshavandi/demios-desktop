//! dn-artifacts — Claude-style artifacts (ADR-0004): an append-only SQLite store with
//! exact-match patching and rollback, and the model tool protocol (artifacts need tool calls; there
//! is no text-format fallback, ADR-0004).
//!
//! The store works on a `&rusqlite::Connection` the app passes in; [`migrations`] creates this
//! crate's `artifact_*` tables and is run by `dn-store`. Contract types live in
//! [`dn_core::artifact`].

pub mod error;
pub mod protocol;
pub mod store;

pub use error::{ArtifactError, Result};
pub use protocol::{execute_tool_call, op_from_tool_call, tool_result, tool_specs};
pub use store::{Applied, ArtifactStore, ArtifactSummary, OpContext, migrations};
