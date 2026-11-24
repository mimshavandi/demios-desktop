//! Provider keys (T-SHELL-02) live in the OS keychain through `dn-secrets`
//! (`KeyringStore`); tests use its `MemoryStore`. Keys are never written to SQLite, never
//! logged and never sent to the UI (only "has a key" is). Every call can block on the
//! keychain, so callers run on a blocking thread.

use dn_secrets::{SecretId, SecretStore};
use std::sync::Arc;

pub use dn_secrets::{Secret, SecretError};

/// The key store plus a short description the settings screen shows.
#[derive(Clone)]
pub struct Keys {
    pub store: Arc<dyn SecretStore>,
    pub description: &'static str,
}

impl Keys {
    /// The OS keychain (macOS Keychain, Windows Credential Manager, Secret Service).
    pub fn os_keychain() -> Self {
        Self {
            store: Arc::new(dn_secrets::KeyringStore::default()),
            description: "your system keychain",
        }
    }

    /// In-memory store for tests.
    pub fn memory() -> Self {
        Self {
            store: Arc::new(dn_secrets::MemoryStore::new()),
            description: "memory (tests)",
        }
    }

    pub fn has(&self, provider_id: &str) -> Result<bool, SecretError> {
        Ok(self.store.get(&SecretId::provider(provider_id))?.is_some())
    }

    pub fn get(&self, provider_id: &str) -> Result<Option<Secret>, SecretError> {
        self.store.get(&SecretId::provider(provider_id))
    }

    pub fn set(&self, provider_id: &str, value: &Secret) -> Result<(), SecretError> {
        self.store.set(&SecretId::provider(provider_id), value)
    }

    pub fn delete(&self, provider_id: &str) -> Result<bool, SecretError> {
        self.store.delete(&SecretId::provider(provider_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_has_delete() {
        let k = Keys::memory();
        assert!(!k.has("openai").unwrap());
        k.set("openai", &Secret::new("test-value")).unwrap();
        assert!(k.has("openai").unwrap());
        assert_eq!(k.get("openai").unwrap().unwrap().expose(), "test-value");
        assert!(k.delete("openai").unwrap());
        assert!(!k.has("openai").unwrap());
    }
}
