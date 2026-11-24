//! Error plumbing: everything leaving dn-tools is a [`CoreError::Capability`] with separate
//! user-facing and developer messages (ADR-0006).

use dn_core::CoreError;

/// Internal error kinds. Converted to [`CoreError`] at the crate boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    pub capability_id: String,
    pub kind: ToolErrorKind,
    pub developer_message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolErrorKind {
    /// Bad server config (missing secret, inline secret, bad URL…).
    Config,
    /// Could not start or reach the server.
    Connection,
    /// The server needs (re)authorisation.
    AuthRequired,
    /// The request or server did not answer in time.
    Timeout,
    /// The server returned a protocol error or the tool reported `isError`.
    ToolFailed,
    /// Unknown capability id.
    NotFound,
    /// Arguments missing or invalid.
    InvalidInput,
    /// The user denied the approval request.
    Denied,
    /// Policy/override blocks this capability.
    Blocked,
    /// The same idempotency key is already running.
    InFlight,
    /// Ledger (SQLite) failure.
    Ledger,
}

impl ToolError {
    pub fn new(
        capability_id: impl Into<String>,
        kind: ToolErrorKind,
        developer_message: impl Into<String>,
    ) -> Self {
        Self {
            capability_id: capability_id.into(),
            kind,
            developer_message: developer_message.into(),
        }
    }

    /// Whether retrying the same call may succeed (callers still check `retry_safe`).
    pub fn retryable(&self) -> bool {
        matches!(
            self.kind,
            ToolErrorKind::Connection | ToolErrorKind::Timeout | ToolErrorKind::InFlight
        )
    }

    pub fn reconnect_required(&self) -> bool {
        matches!(self.kind, ToolErrorKind::AuthRequired)
    }

    fn user_message(&self) -> String {
        let tool = display_name(&self.capability_id);
        match self.kind {
            ToolErrorKind::Config => format!("The tool \"{tool}\" is not set up correctly."),
            ToolErrorKind::Connection => {
                format!("Couldn't reach the tool server for \"{tool}\". Try again.")
            }
            ToolErrorKind::AuthRequired => {
                format!("\"{tool}\" needs you to reconnect its account.")
            }
            ToolErrorKind::Timeout => format!("\"{tool}\" took too long to respond."),
            ToolErrorKind::ToolFailed => format!("\"{tool}\" reported an error."),
            ToolErrorKind::NotFound => format!("The tool \"{tool}\" isn't available."),
            ToolErrorKind::InvalidInput => format!("\"{tool}\" was called with missing input."),
            ToolErrorKind::Denied => format!("You declined \"{tool}\"."),
            ToolErrorKind::Blocked => format!("\"{tool}\" is turned off."),
            ToolErrorKind::InFlight => format!("\"{tool}\" is already running this action."),
            ToolErrorKind::Ledger => format!("Couldn't record the result of \"{tool}\"."),
        }
    }
}

/// `mcp.files.write_file` → `write_file`.
fn display_name(capability_id: &str) -> &str {
    capability_id.rsplit('.').next().unwrap_or(capability_id)
}

impl From<ToolError> for CoreError {
    fn from(e: ToolError) -> Self {
        CoreError::Capability {
            user_message: e.user_message(),
            retryable: e.retryable(),
            reconnect_required: e.reconnect_required(),
            developer_message: e.developer_message,
            capability_id: e.capability_id,
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({:?}): {}",
            self.capability_id, self.kind, self.developer_message
        )
    }
}

impl std::error::Error for ToolError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_to_capability_envelope() {
        let e = ToolError::new("mcp.gh.create_issue", ToolErrorKind::AuthRequired, "401");
        match CoreError::from(e) {
            CoreError::Capability {
                capability_id,
                user_message,
                developer_message,
                retryable,
                reconnect_required,
            } => {
                assert_eq!(capability_id, "mcp.gh.create_issue");
                assert!(user_message.contains("create_issue"));
                assert_eq!(developer_message, "401");
                assert!(!retryable);
                assert!(reconnect_required);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
