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

    #[error(
        "{what} is {actual} bytes; artifacts are limited to {max} bytes. Make it smaller or split \
         it into several artifacts"
    )]
    TooLarge {
        what: &'static str,
        max: usize,
        actual: usize,
    },

    #[error(
        "the limit of {max} {what} is reached, so this change was not saved; change an existing \
         artifact instead of creating more, or start a new chat"
    )]
    TooMany { what: &'static str, max: usize },

    #[error("title is {actual} characters; use a short title (at most {max})")]
    TitleTooLong { max: usize, actual: usize },

    #[error(
        "artifact {id} is a {existing} artifact, not {requested}; keep the type, or use a new id \
         to create a different artifact"
    )]
    TypeMismatch {
        id: Id,
        existing: String,
        requested: String,
    },

    #[error(
        "the <artifact> block was not closed with </artifact>, so it was not saved; write it again in full"
    )]
    Unterminated,

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
