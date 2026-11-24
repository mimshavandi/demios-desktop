//! dn-secrets — BYOK provider keys and MCP tokens in the OS keychain (ADR-0007).
//!
//! Values never go to SQLite or logs: [`Secret`] redacts itself in `Debug`/`Display`,
//! and errors carry ids, never values. Other crates depend on the [`SecretStore`] trait
//! and use [`MemoryStore`] in tests; the app wires in [`KeyringStore`].

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

/// Keychain service name all entries live under.
pub const DEFAULT_SERVICE: &str = "dev.demios.desktop";

/// What a secret is for. Part of the keychain account name, so ids never collide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// API key for a BYOK provider or OpenAI-compatible endpoint (`byok:<provider>`).
    ProviderKey,
    /// OAuth / bearer token for an MCP server.
    McpToken,
}

impl SecretKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::ProviderKey => "provider",
            Self::McpToken => "mcp",
        }
    }
}

/// Identifies one secret, e.g. `provider/openai` or `mcp/github`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SecretId {
    pub kind: SecretKind,
    pub id: String,
}

impl SecretId {
    pub fn provider(id: impl Into<String>) -> Self {
        Self {
            kind: SecretKind::ProviderKey,
            id: id.into(),
        }
    }

    pub fn mcp(id: impl Into<String>) -> Self {
        Self {
            kind: SecretKind::McpToken,
            id: id.into(),
        }
    }

    /// Keychain account name: `<kind>/<id>`.
    pub fn account(&self) -> String {
        format!("{}/{}", self.kind.prefix(), self.id)
    }

    fn validate(&self) -> Result<(), SecretError> {
        let ok = !self.id.is_empty()
            && self.id.len() <= 128
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
        if ok {
            Ok(())
        } else {
            Err(SecretError::InvalidId(self.account()))
        }
    }
}

impl fmt::Display for SecretId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.account())
    }
}

/// A secret value. `Debug` and `Display` print `***`; call [`expose`](Self::expose) to use it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Only pass it straight to where it is needed (e.g. an HTTP header).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Errors never contain secret values.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum SecretError {
    #[error("invalid secret id `{0}`")]
    InvalidId(String),
    #[error("empty secret value for `{0}`")]
    EmptyValue(String),
    /// No usable OS keychain (e.g. no Secret Service on a headless Linux box).
    #[error("OS keychain unavailable: {0}")]
    Unavailable(String),
    #[error("OS keychain error for `{id}`: {message}")]
    Backend { id: String, message: String },
}

/// Where secrets are kept. Implementations must be thread-safe and must not log values.
///
/// Calls may block on the OS keychain; from async code, run them in `spawn_blocking`.
pub trait SecretStore: Send + Sync {
    fn get(&self, id: &SecretId) -> Result<Option<Secret>, SecretError>;
    fn set(&self, id: &SecretId, value: &Secret) -> Result<(), SecretError>;
    /// Returns whether something was deleted.
    fn delete(&self, id: &SecretId) -> Result<bool, SecretError>;
}

fn check(id: &SecretId, value: Option<&Secret>) -> Result<(), SecretError> {
    id.validate()?;
    if value.is_some_and(|v| v.0.is_empty()) {
        return Err(SecretError::EmptyValue(id.account()));
    }
    Ok(())
}

/// OS keychain: macOS Keychain, Windows Credential Manager, Linux Secret Service.
#[derive(Clone, Debug)]
pub struct KeyringStore {
    service: String,
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new(DEFAULT_SERVICE)
    }
}

impl KeyringStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, id: &SecretId) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, &id.account()).map_err(|e| map_err(id, e))
    }
}

fn map_err(id: &SecretId, e: keyring::Error) -> SecretError {
    match e {
        keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_) => {
            SecretError::Unavailable(e.to_string())
        }
        // `keyring` error texts describe the failure, not the stored value.
        other => SecretError::Backend {
            id: id.account(),
            message: other.to_string(),
        },
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, id: &SecretId) -> Result<Option<Secret>, SecretError> {
        check(id, None)?;
        match self.entry(id)?.get_password() {
            Ok(v) => Ok(Some(Secret(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_err(id, e)),
        }
    }

    fn set(&self, id: &SecretId, value: &Secret) -> Result<(), SecretError> {
        check(id, Some(value))?;
        self.entry(id)?
            .set_password(value.expose())
            .map_err(|e| map_err(id, e))
    }

    fn delete(&self, id: &SecretId) -> Result<bool, SecretError> {
        check(id, None)?;
        match self.entry(id)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(map_err(id, e)),
        }
    }
}

/// In-memory store for tests and for other crates' fakes. Same validation as the keychain.
#[derive(Debug, Default)]
pub struct MemoryStore {
    map: Mutex<HashMap<SecretId, Secret>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-filled store, e.g. `MemoryStore::with([(SecretId::provider("openai"), "sk-test".into())])`.
    pub fn with(items: impl IntoIterator<Item = (SecretId, Secret)>) -> Self {
        Self {
            map: Mutex::new(items.into_iter().collect()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SecretId, Secret>> {
        self.map.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, id: &SecretId) -> Result<Option<Secret>, SecretError> {
        check(id, None)?;
        Ok(self.lock().get(id).cloned())
    }

    fn set(&self, id: &SecretId, value: &Secret) -> Result<(), SecretError> {
        check(id, Some(value))?;
        self.lock().insert(id.clone(), value.clone());
        Ok(())
    }

    fn delete(&self, id: &SecretId) -> Result<bool, SecretError> {
        check(id, None)?;
        Ok(self.lock().remove(id).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(store: &dyn SecretStore) {
        let id = SecretId::provider("openai");
        assert_eq!(store.get(&id).unwrap(), None);
        store.set(&id, &"sk-test-not-real".into()).unwrap();
        assert_eq!(
            store.get(&id).unwrap().unwrap().expose(),
            "sk-test-not-real"
        );
        // Provider and MCP namespaces don't collide.
        assert_eq!(store.get(&SecretId::mcp("openai")).unwrap(), None);
        assert!(store.delete(&id).unwrap());
        assert!(!store.delete(&id).unwrap());
        assert_eq!(store.get(&id).unwrap(), None);
    }

    #[test]
    fn memory_store_roundtrip() {
        roundtrip(&MemoryStore::new());
    }

    #[test]
    fn values_are_redacted() {
        let s = Secret::new("sk-live-should-not-print");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(format!("{s}"), "***");
        let e = SecretError::EmptyValue(SecretId::provider("x").account());
        assert!(!e.to_string().contains("sk-"));
    }

    #[test]
    fn rejects_bad_ids_and_empty_values() {
        let store = MemoryStore::new();
        assert!(matches!(
            store.get(&SecretId::provider("")),
            Err(SecretError::InvalidId(_))
        ));
        assert!(matches!(
            store.get(&SecretId::mcp("a/b")),
            Err(SecretError::InvalidId(_))
        ));
        assert!(matches!(
            store.set(&SecretId::provider("openai"), &"".into()),
            Err(SecretError::EmptyValue(_))
        ));
        assert_eq!(SecretId::mcp("github").to_string(), "mcp/github");
    }

    /// Touches the real OS keychain; run with `cargo test -p dn-secrets -- --ignored`.
    #[test]
    #[ignore = "uses the real OS keychain"]
    fn keyring_store_roundtrip() {
        roundtrip(&KeyringStore::new("dev.demios.desktop.test"));
    }
}
