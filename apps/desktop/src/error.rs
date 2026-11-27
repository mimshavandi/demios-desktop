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
                tracing::warn!("store error: {other}");
                Self::new("storage", "Could not read or write local data.")
            }
        }
    }
}

impl From<crate::secrets::SecretError> for IpcError {
    fn from(e: crate::secrets::SecretError) -> Self {
        use crate::secrets::SecretError as E;
        // SecretError never contains secret values.
        match e {
            E::Denied(_) => Self::new(
                "key_store_denied",
                "Access to your system keychain was denied. Allow it, then try again.",
            ),
            E::Unavailable(_) => Self::new(
                "key_store_unavailable",
                "Your system keychain isn't available, so the key can't be stored.",
            ),
            // Only a Node's passphrase store locks; the app uses the system keychain.
            E::Locked => Self::new(
                "key_store_locked",
                "The secret store is locked. Unlock it, then try again.",
            ),
            E::TooLarge { .. } | E::EmptyValue(_) | E::InvalidId(_) => Self::invalid(e.to_string()),
            E::Backend { .. } => {
                tracing::warn!("keychain error: {e}");
                Self::new("key_store", "Couldn't use your system keychain. Try again.")
            }
        }
    }
}

pub type IpcResult<T> = Result<T, IpcError>;
