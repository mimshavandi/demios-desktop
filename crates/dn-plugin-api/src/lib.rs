//! `dn-plugin-api`: the stable interface Demios add-ons build against
//! ([ADR-0013](../../../docs/adr/0013-plugin-api.md)).
//!
//! An add-on is a Rust crate that depends on this crate and nothing else from the app. It
//! is linked into a separate add-on build; the regular build links none and doesn't show
//! Settings › Add-ons. An add-on implements [`Plugin`]:
//!
//! - its [`PluginManifest`] names it and the [`Permission`]s it asks for, and the user
//!   consents to each one;
//! - in [`Plugin::register`] it declares its tools, skills, background jobs and settings
//!   panel through the [`Registrar`];
//! - its handlers reach the app only through a [`Host`], which checks the add-on's
//!   permissions and its handler's tier on every call and marks content from outside as
//!   [`Untrusted`].
//!
//! **What the host enforces, and what it can't.** An add-on runs in the app's process. The
//! `Host` enforces permissions, call scopes, tier limits and ownership of data; it can't
//! stop an add-on from using the standard library's own network, process or file APIs.
//! Not doing so is a review rule for first-party add-ons, backed by checks in the add-on
//! build's CI. Third parties extend Demios through MCP servers and skills.
//!
//! **Stability.** [`API_MAJOR`] changes only with an ADR. Minor versions only add: public
//! structs and enums are `#[non_exhaustive]`, and new trait methods come with defaults. An
//! add-on built for another major is refused with a clear message
//! ([`manifest::check_compatible`]).
//!
//! The `testing` feature adds the `testing` module, a conformance kit with an in-memory host.

#![warn(missing_docs)]

pub mod data;
pub mod error;
pub mod host;
pub mod id;
pub mod licence;
pub mod manifest;
pub mod pattern;
pub mod plugin;
pub mod registrar;
pub mod schedule;
pub mod settings;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod untrusted;

pub use data::{
    ChatMessage, ConversationSummary, HttpMethod, HttpRequest, HttpResponse, KnowledgeHit,
    MemoryItem, ModelPrompt, ModelSlot, NewMemory, Role, Row, SqlValue, ToolResult,
};
/// Types from `dn-core` that cross the boundary, re-exported so add-ons don't depend on
/// other app crates.
pub use dn_core::{Id, capability::RiskTier};
pub use error::{ErrorKind, PluginError};
pub use host::{
    Access, Caller, ChatScope, Consent, Grants, GrantsHandle, Host, HostBackend, Origin,
};
pub use id::PluginId;
pub use licence::LicenceStatus;
pub use manifest::{Permission, PermissionRequest, PluginManifest, risk_floor};
pub use plugin::Plugin;
pub use registrar::{
    CancelToken, JobContext, JobDecl, JobHandler, RegisteredJob, RegisteredTool, Registrar,
    Registrations, SkillDecl, ToolDecl, ToolHandler, ToolInput,
};
pub use schedule::{MissedRunRule, MissedRuns, Schedule, missed_runs};
pub use settings::{ChoiceOption, Field, FieldKind, SettingsPanel};
pub use untrusted::{Quoted, Untrusted};

/// The major version of this interface. An add-on's manifest names the major it was built
/// for, and the host refuses any other. Changing it needs an ADR.
pub const API_MAJOR: u32 = 1;

/// The minor version: additions since this major began.
pub const API_MINOR: u32 = 0;

/// A boxed future, the return type of every asynchronous call across the boundary.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
