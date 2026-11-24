//! Typed errors shared across crates. User-facing text and developer detail are kept apart
//! (Demios `CapabilityError` / `ModelUnavailableError` envelopes).

use crate::model::Capability;
use serde::{Deserialize, Serialize};

/// Something the UI can offer the user to fix an error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RecoveryAction {
    DownloadModel { capability: Capability },
    AddProviderKey,
    ChooseModelForSlot { agent_id: String, slot_id: String },
    ReconnectServer { server_id: String },
    Retry { after_seconds: Option<u32> },
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum CoreError {
    /// No model is configured for this capability. Never fall back silently to another source.
    #[error("no model configured for {capability:?} (agent `{agent_id}`, slot `{slot_id}`)")]
    ModelSourceRequired {
        capability: Capability,
        agent_id: String,
        slot_id: String,
        recovery: Vec<RecoveryAction>,
    },
    #[error("model unavailable: {developer_message}")]
    ModelUnavailable {
        user_message: String,
        developer_message: String,
        recovery: Vec<RecoveryAction>,
    },
    #[error("capability `{capability_id}` failed: {developer_message}")]
    Capability {
        capability_id: String,
        user_message: String,
        developer_message: String,
        retryable: bool,
        reconnect_required: bool,
    },
    #[error("cancelled")]
    Cancelled,
}
