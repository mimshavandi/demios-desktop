use dn_core::Id;

/// Errors from the artifact store and the model protocol.
///
/// The `Display` text of the patch errors is written for the model: it is sent back as the tool
/// result so the model can correct itself (re-read, add context, or fall back to a rewrite).
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("artifact {id} not found")]
    NotFound { id: Id },

    #[error("artifact {id} has no version {n}")]
    VersionNotFound { id: Id, n: u32 },

    #[error("old_str must not be empty; use artifact_rewrite to replace the whole content")]
    EmptyOldStr,

    #[error(
        "old_str was not found in artifact {id} (latest version {n}); copy the exact text \
         (including whitespace) from the latest version, or use artifact_rewrite"
    )]
    NoMatch { id: Id, n: u32 },

    #[error(
        "old_str matches {count} times in artifact {id} (latest version {n}); include more \
         surrounding text so it matches exactly once"
    )]
    AmbiguousMatch { id: Id, n: u32, count: usize },

    #[error("invalid input for tool `{tool}`: {reason}")]
    InvalidToolInput { tool: String, reason: String },

    #[error("unknown artifact tool `{0}`")]
    UnknownTool(String),

    /// A stored row could not be decoded (should not happen unless the database was edited by hand).
    #[error("corrupt artifact row: {0}")]
    Corrupt(String),

    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub type Result<T, E = ArtifactError> = std::result::Result<T, E>;
