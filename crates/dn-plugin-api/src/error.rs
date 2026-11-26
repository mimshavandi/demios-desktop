//! Errors that cross the add-on boundary.

use serde::{Deserialize, Serialize};
use std::fmt;

/// What kind of failure a [`PluginError`] is. The host decides from this what it shows and
/// whether the add-on can go on.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The add-on doesn't hold the permission, or the user turned it off (ADR-0013 §3).
    PermissionDenied,
    /// The `Host` handle was revoked, for example while "Delete all local data" runs.
    Revoked,
    /// The call needs more than the handler's tier allows (ADR-0013 §2).
    TierLimit,
    /// The data, tool or name belongs to someone else, or isn't one the add-on may use.
    NotAllowed,
    /// The add-on gave a value the host can't accept (an id, a schedule, a path).
    InvalidInput,
    /// The add-on was built for another `dn-plugin-api` major version (ADR-0013 §7).
    Incompatible,
    /// The call was cancelled, or a job ran out of its time budget.
    Cancelled,
    /// The service isn't available right now (for example no model is chosen).
    Unavailable,
    /// A background job's call needs a person, so it waits for the user's answer instead of
    /// running (ADR-0013 §2). It is never approved on its own.
    Held,
    /// The add-on's own code failed.
    Failed,
}

/// An error from the host or from an add-on, with a message a person can read.
///
/// The message is shown as plain text, never as markup (ADR-0013 §2).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginError {
    /// What kind of failure this is.
    pub kind: ErrorKind,
    /// What happened, in words for the user.
    pub message: String,
}

impl PluginError {
    /// An error of `kind` with `message`.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// [`ErrorKind::InvalidInput`].
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidInput, message)
    }

    /// [`ErrorKind::Failed`], for an add-on's own failures.
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Failed, message)
    }

    /// [`ErrorKind::NotAllowed`].
    pub fn not_allowed(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotAllowed, message)
    }

    /// [`ErrorKind::Unavailable`].
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unavailable, message)
    }

    /// [`ErrorKind::Cancelled`].
    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Cancelled, message)
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PluginError {}
