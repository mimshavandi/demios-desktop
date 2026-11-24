use dn_core::{CoreError, RecoveryAction};

pub type Result<T, E = InferenceError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum InferenceError {
    #[error("model file not found: {0}")]
    NotFound(String),
    #[error("failed to load model: {0}")]
    Load(String),
    #[error("unknown model handle {0}")]
    UnknownHandle(u64),
    #[error("{backend} does not support {what}")]
    Unsupported { backend: &'static str, what: String },
    #[error("prompt has {prompt} tokens but the context holds {n_ctx}")]
    ContextOverflow { prompt: usize, n_ctx: u32 },
    #[error("model is busy with another request")]
    Busy,
    #[error("inference failed: {0}")]
    Runtime(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("server returned {status}: {body}")]
    Server { status: u16, body: String },
}

impl From<InferenceError> for CoreError {
    fn from(e: InferenceError) -> Self {
        let developer_message = e.to_string();
        let (user_message, recovery) = match &e {
            InferenceError::NotFound(_) => (
                "The model file is missing. Download it again or pick another model.".to_string(),
                vec![],
            ),
            InferenceError::Http(_) | InferenceError::Server { .. } => (
                "The model server did not answer. Check that it is running.".to_string(),
                vec![RecoveryAction::Retry {
                    after_seconds: Some(5),
                }],
            ),
            InferenceError::ContextOverflow { .. } => (
                "This conversation is too long for the model's context window.".to_string(),
                vec![],
            ),
            InferenceError::Busy => (
                "This model is busy with another request. Try again when it finishes.".to_string(),
                vec![RecoveryAction::Retry {
                    after_seconds: Some(2),
                }],
            ),
            _ => ("The model could not answer.".to_string(), vec![]),
        };
        CoreError::ModelUnavailable {
            user_message,
            developer_message,
            recovery,
        }
    }
}
