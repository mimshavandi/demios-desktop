//! Claude-style artifacts (ADR-0004): append-only versions, model edits by patch or rewrite.

use crate::Id;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactType {
    Html,
    React,
    Svg,
    Mermaid,
    Markdown,
    Code { language: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    Model,
    User,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVersion {
    /// 1-based, strictly increasing per artifact.
    pub n: u32,
    pub content: String,
    pub author: Author,
    /// Version this one was derived from (`None` for v1; a rollback points at the restored version).
    pub parent_n: Option<u32>,
    pub message_id: Option<Id>,
    /// RFC 3339 timestamp.
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: Id,
    pub conversation_id: Id,
    pub artifact_type: ArtifactType,
    pub title: String,
    pub versions: Vec<ArtifactVersion>,
}

impl Artifact {
    pub fn latest(&self) -> Option<&ArtifactVersion> {
        self.versions.last()
    }
}

/// The three operations models use on artifacts (tool names `artifact_create`, `artifact_update`, `artifact_rewrite`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ArtifactOp {
    Create {
        artifact_type: ArtifactType,
        title: String,
        content: String,
    },
    /// Exact-match replacement; `old_str` must occur exactly once in the latest version.
    Update {
        id: Id,
        old_str: String,
        new_str: String,
    },
    Rewrite {
        id: Id,
        content: String,
    },
}
