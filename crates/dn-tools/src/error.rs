//! Error plumbing: everything leaving dn-tools is a [`CoreError::Capability`] with separate
//! user-facing and developer messages (ADR-0006).

use dn_core::CoreError;

/// Internal error kinds. Converted to [`CoreError`] at the crate boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    pub capability_id: String,
    pub kind: ToolErrorKind,
    pub developer_message: String,
    /// The request reached (or may have reached) the server before failing, e.g. a timeout or
    /// a closed transport mid-call. For writes the outcome is then unknown.
    pub maybe_executed: bool,
    /// Extra text the model may see: our own hints (e.g. which fields are missing) or the
    /// tool's own `isError` content (which MCP intends for the model). Never stderr, paths or
    /// transport errors — those stay in `developer_message`.
    pub model_message: Option<String>,
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
    /// The server said its tools changed (`tools/list_changed`, #178) and they are being
    /// listed and reviewed again; its tools are refused until then.
    Changing,
    /// The executor's state can't be trusted after an internal error (a lock was poisoned):
    /// every tool is refused until restart, so per-tool choices can never be skipped.
    Unavailable,
    /// The same idempotency key is already running.
    InFlight,
    /// Ledger (SQLite) failure.
    Ledger,
    /// The run was cancelled (becomes [`CoreError::Cancelled`]).
    Cancelled,
    /// A previous attempt of this write ended without a known outcome (interrupted, cancelled
    /// or timed out mid-call). It won't run again until the user resolves it.
    OutcomeUnknown,
    /// A background job's call needs a person: the host queues it for the user, and it is
    /// never approved on its own (ADR-0013 §2).
    Held,
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
            maybe_executed: false,
            model_message: None,
        }
    }

    /// Attach text the model may see (see [`Self::model_message`]).
    pub fn with_model_message(mut self, msg: impl Into<String>) -> Self {
        self.model_message = Some(msg.into());
        self
    }

    /// Mark that the request may have been executed by the server.
    pub fn maybe_executed(mut self) -> Self {
        self.maybe_executed = true;
        self
    }

    /// Re-attribute to a (more specific) capability id, keeping kind and flags.
    pub fn for_capability(mut self, capability_id: &str) -> Self {
        self.capability_id = capability_id.to_owned();
        self
    }

    /// Whether retrying the same call may succeed (callers still check `retry_safe`).
    pub fn retryable(&self) -> bool {
        matches!(
            self.kind,
            ToolErrorKind::Connection
                | ToolErrorKind::Timeout
                | ToolErrorKind::InFlight
                | ToolErrorKind::Changing
        )
    }

    pub fn reconnect_required(&self) -> bool {
        matches!(self.kind, ToolErrorKind::AuthRequired)
    }

    /// Plain-language message for the user (and the model).
    pub fn user_message(&self) -> String {
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
            ToolErrorKind::Changing => format!(
                "The tool server for \"{tool}\" changed its tools. They are being checked \
                 again; try again in a moment."
            ),
            ToolErrorKind::Unavailable => "Tools are turned off after an internal error, so your \
                                           tool settings can't be skipped. Restart Demios to use \
                                           tools again."
                .to_owned(),
            ToolErrorKind::InFlight => format!("\"{tool}\" is already running this action."),
            ToolErrorKind::Ledger => format!("Couldn't record the result of \"{tool}\"."),
            ToolErrorKind::Cancelled => format!("\"{tool}\" was stopped."),
            ToolErrorKind::Held => {
                format!("\"{tool}\" waits for your answer in Demios before it runs.")
            }
            ToolErrorKind::OutcomeUnknown => format!(
                "An earlier \"{tool}\" action may or may not have gone through. Check it and \
                 confirm before running it again."
            ),
        }
    }
}

/// `mcp.files.write_file` → `write_file`.
fn display_name(capability_id: &str) -> &str {
    capability_id.rsplit('.').next().unwrap_or(capability_id)
}

impl From<ToolError> for CoreError {
    fn from(e: ToolError) -> Self {
        if e.kind == ToolErrorKind::Cancelled {
            return CoreError::Cancelled;
        }
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
