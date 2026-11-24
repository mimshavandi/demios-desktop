//! Minimal secret-store seam. W-providers owns the real keychain-backed store in `dn-secrets`;
//! until it lands, dn-tools depends only on this trait so the adapter is a thin shim.
//!
//! Values are looked up by *secret id* (e.g. `mcp.github.token`). Server configs only ever carry
//! ids, never the values themselves.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("secret store: {0}")]
pub struct SecretError(pub String);

/// Read/write access to secrets by id. Implementations must not log values.
pub trait SecretStore: Send + Sync {
    fn get(&self, secret_id: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, secret_id: &str, value: &str) -> Result<(), SecretError>;
    fn delete(&self, secret_id: &str) -> Result<(), SecretError>;
}

impl<T: SecretStore + ?Sized> SecretStore for Arc<T> {
    fn get(&self, secret_id: &str) -> Result<Option<String>, SecretError> {
        (**self).get(secret_id)
    }
    fn set(&self, secret_id: &str, value: &str) -> Result<(), SecretError> {
        (**self).set(secret_id, value)
    }
    fn delete(&self, secret_id: &str) -> Result<(), SecretError> {
        (**self).delete(secret_id)
    }
}

/// In-memory store for tests and until `dn-secrets` lands. Not persistent.
#[derive(Default, Clone)]
pub struct MemorySecretStore {
    values: Arc<Mutex<HashMap<String, String>>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for MemorySecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.values.lock().map(|v| v.len()).unwrap_or(0);
        f.debug_struct("MemorySecretStore")
            .field("entries", &n)
            .finish()
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, secret_id: &str) -> Result<Option<String>, SecretError> {
        let values = self
            .values
            .lock()
            .map_err(|_| SecretError("poisoned".into()))?;
        Ok(values.get(secret_id).cloned())
    }
    fn set(&self, secret_id: &str, value: &str) -> Result<(), SecretError> {
        let mut values = self
            .values
            .lock()
            .map_err(|_| SecretError("poisoned".into()))?;
        values.insert(secret_id.to_owned(), value.to_owned());
        Ok(())
    }
    fn delete(&self, secret_id: &str) -> Result<(), SecretError> {
        let mut values = self
            .values
            .lock()
            .map_err(|_| SecretError("poisoned".into()))?;
        values.remove(secret_id);
        Ok(())
    }
}
