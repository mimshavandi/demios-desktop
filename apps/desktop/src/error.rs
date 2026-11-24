//! The one error type IPC commands return. The UI gets a code, a message it may show,
//! and recovery actions from `dn-core`; developer detail stays in the backend.

use dn_core::RecoveryAction;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct IpcError {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovery: Vec<RecoveryAction>,
}

impl IpcError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            recovery: vec![],
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_input", message)
    }
}

impl From<dn_store::StoreError> for IpcError {
    fn from(e: dn_store::StoreError) -> Self {
        match e {
            dn_store::StoreError::NotFound(what) => Self::new("not_found", what),
            dn_store::StoreError::Invalid(what) => Self::invalid(what),
            other => {
                eprintln!("store error: {other}");
                Self::new("storage", "Could not read or write local data.")
            }
        }
    }
}

impl From<crate::secrets::KeyStoreError> for IpcError {
    fn from(e: crate::secrets::KeyStoreError) -> Self {
        Self::new("key_store", e.to_string())
    }
}

pub type IpcResult<T> = Result<T, IpcError>;
