//! Provider keys (T-SHELL-02). Keys go to the OS keychain through `dn-secrets`, which
//! W-providers is building; until it lands the app uses [`MemoryKeyStore`], which keeps
//! keys for the session only. Keys are never written to SQLite, never logged and never
//! sent back to the UI (only "has a key" is).

use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum KeyStoreError {
    #[error("key store unavailable: {0}")]
    Unavailable(String),
}

/// The slice of the `dn-secrets` trait the shell needs. Swap the implementation, not the
/// callers, when `dn-secrets` merges.
pub trait KeyStore: Send + Sync {
    fn set(&self, account: &str, secret: &str) -> Result<(), KeyStoreError>;
    fn has(&self, account: &str) -> Result<bool, KeyStoreError>;
    fn delete(&self, account: &str) -> Result<(), KeyStoreError>;
    /// Short human description shown in settings, e.g. "session only (dev)".
    fn describe(&self) -> &'static str;
}

/// Account name under which a provider's API key is stored.
pub fn provider_account(provider_id: &str) -> String {
    format!("provider:{provider_id}:api_key")
}

/// In-memory stand-in. Not persisted: keys are gone after a restart.
#[derive(Default)]
pub struct MemoryKeyStore {
    inner: Mutex<HashMap<String, String>>,
}

impl KeyStore for MemoryKeyStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), KeyStoreError> {
        self.lock()?.insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn has(&self, account: &str) -> Result<bool, KeyStoreError> {
        Ok(self.lock()?.contains_key(account))
    }

    fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
        self.lock()?.remove(account);
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "session only: keys are kept in memory until the OS keychain store (dn-secrets) lands"
    }
}

impl MemoryKeyStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, String>>, KeyStoreError> {
        self.inner
            .lock()
            .map_err(|_| KeyStoreError::Unavailable("lock poisoned".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_has_delete() {
        let s = MemoryKeyStore::default();
        let a = provider_account("openai");
        assert!(!s.has(&a).unwrap());
        s.set(&a, "test-value").unwrap();
        assert!(s.has(&a).unwrap());
        s.delete(&a).unwrap();
        assert!(!s.has(&a).unwrap());
    }
}
