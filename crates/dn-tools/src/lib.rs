//! dn-tools — MCP client, capability wrapper, approval gate and side-effect ledger
//! (ADR-0006, tasks T-CON-01..05).
//!
//! - [`client`]: MCP over stdio and Streamable HTTP (`rmcp`), restarts and timeouts.
//! - [`capability`]: MCP tool → [`dn_core::capability::CapabilitySpec`], risk seeding,
//!   user overrides, preflight.
//! - [`approval`]: approval requests as an async channel for the UI.
//! - [`ledger`]: SQLite side-effect ledger (`tool_ledger_*` tables, [`migrations`]).
//! - [`executor`]: ties it together; the agent loop's `ToolExecutor` adapter wraps this.
//! - [`oauth`]: OAuth 2.1 (code + PKCE, discovery) for remote servers.
//! - [`agent`]: `dn_agent::ToolExecutor` for [`Executor`].
//!
//! Errors leave the crate as [`dn_core::CoreError::Capability`]. Threat model:
//! `docs/security/mcp-tools.md`.

pub mod agent;
pub mod approval;
pub mod capability;
pub mod client;
pub mod config;
pub mod error;
pub mod executor;
pub mod ledger;
pub mod narrow;
pub mod oauth;
pub mod secrets;

pub use approval::{ApprovalDecision, ApprovalGate, ApprovalPrompt, ApprovalRequest};
pub use capability::{CapabilityOverride, McpCapability};
pub use client::{ConnectionStatus, McpCallOutput, McpClient, McpTool};
pub use config::{McpServerConfig, McpTransport, OAuthSettings, ValueRef};
pub use executor::{CallContext, Executor, RetryPolicy, ToolOutcome};
pub use ledger::migrations;
pub use narrow::{CapabilityResolver, Narrowed, ToolAllowlist};
pub use oauth::{LoopbackRedirect, OAuthManager, PendingAuthorization};
pub use secrets::{DnSecrets, MemorySecretStore, SecretStore};
