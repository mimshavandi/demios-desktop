//! Licence status (ADR-0013 §6). Checking a licence is the add-on build's job; this crate
//! only defines what the host shows in Settings.

use serde::{Deserialize, Serialize};

/// An add-on's licence, as shown in Settings › Add-ons.
///
/// When it isn't [`LicenceStatus::Valid`] or [`LicenceStatus::NotRequired`], the add-on's
/// features stop with a message naming the reason. Its data is never locked: memory stays
/// viewable and deletable, purges still run, and "Delete all local data" always works.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LicenceStatus {
    /// The add-on needs no licence.
    NotRequired,
    /// A valid licence.
    Valid {
        /// Who it is licensed to, shown as plain text.
        licensee: String,
        /// When it ends, Unix seconds; `None` when it doesn't.
        expires_at: Option<i64>,
    },
    /// The licence ended.
    Expired {
        /// When it ended, Unix seconds.
        ended_at: i64,
    },
    /// No licence file was found.
    Missing,
    /// The licence file can't be used, for example its signature doesn't match.
    Invalid {
        /// Why, in words for the user.
        reason: String,
    },
}

impl LicenceStatus {
    /// True when the add-on's features may run.
    pub fn allows_use(&self) -> bool {
        matches!(self, Self::NotRequired | Self::Valid { .. })
    }
}
