//! Minimal secret-store seam. The app passes [`DnSecrets`] (the OS keychain via `dn-secrets`);
//! tests use [`MemorySecretStore`].
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

/// [`SecretStore`] over `dn-secrets` (OS keychain). dn-tools ids (`mcp.github.token`,
/// `mcp.github.oauth`) map to `SecretId::mcp(id)`, i.e. keychain account `mcp/<id>`.
///
/// Keychain calls may block briefly; they happen at connect / sign-in time, not per tool call.
#[derive(Clone)]
pub struct DnSecrets(pub Arc<dyn dn_secrets::SecretStore>);

impl std::fmt::Debug for DnSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DnSecrets")
    }
}

impl SecretStore for DnSecrets {
    fn get(&self, secret_id: &str) -> Result<Option<String>, SecretError> {
        self.0
            .get(&dn_secrets::SecretId::mcp(secret_id))
            .map(|v| v.map(|s| s.expose().to_owned()))
            .map_err(|e| SecretError(e.to_string()))
    }
    fn set(&self, secret_id: &str, value: &str) -> Result<(), SecretError> {
        self.0
            .set(
                &dn_secrets::SecretId::mcp(secret_id),
                &dn_secrets::Secret::new(value),
            )
            .map_err(|e| SecretError(e.to_string()))
    }
    fn delete(&self, secret_id: &str) -> Result<(), SecretError> {
        self.0
            .delete(&dn_secrets::SecretId::mcp(secret_id))
            .map(|_| ())
            .map_err(|e| SecretError(e.to_string()))
    }
}

/// In-memory store for tests. Not persistent.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dn_secrets_adapter_round_trip() {
        let inner = Arc::new(dn_secrets::MemoryStore::new());
        let s = DnSecrets(inner.clone());
        assert_eq!(s.get("mcp.gh.token").unwrap(), None);
        s.set("mcp.gh.token", "v1").unwrap();
        assert_eq!(s.get("mcp.gh.token").unwrap().as_deref(), Some("v1"));
        let raw = dn_secrets::SecretStore::get(
            inner.as_ref(),
            &dn_secrets::SecretId::mcp("mcp.gh.token"),
        );
        assert!(raw.unwrap().is_some());
        s.delete("mcp.gh.token").unwrap();
        assert_eq!(s.get("mcp.gh.token").unwrap(), None);
        assert!(s.get("bad id with spaces").is_err());
    }
}
