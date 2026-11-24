//! Event envelope for local events and, later, team sync (ADR-0008).
//! Ported from the Demios EventBus envelope v1.

use crate::Id;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Envelope {
    /// Dedup key; replays with the same id are ignored.
    pub event_id: Id,
    pub schema_version: u16,
    /// Dotted type, e.g. `artifact.version.created`.
    #[serde(rename = "type")]
    pub event_type: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<Id>,
    /// Sync space this event belongs to (None = local only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<Id>,
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub data: serde_json::Value,
}
