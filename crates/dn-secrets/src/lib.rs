//! dn-secrets — BYOK provider keys and MCP tokens in the OS keychain (ADR-0007).
//!
//! Values never go to SQLite or logs: [`Secret`] redacts itself in `Debug`/`Display` and
//! wipes its memory on drop (`zeroize`), and errors carry ids, never values. Other crates
//! depend on the [`SecretStore`] trait and use [`MemoryStore`] in tests; the app wires in
//! [`KeyringStore`].
//!
//! **Async callers must not call a store directly from a runtime thread.** The keychain
//! calls block, and on Linux the Secret Service backend runs its own `block_on`, which
//! panics inside a Tokio runtime. Use [`AsyncSecrets`], which runs each call in
//! `spawn_blocking`.

pub mod server;

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// Keychain service name all entries live under.
pub const DEFAULT_SERVICE: &str = dn_core::APP_ID;

/// Largest secret every backend can store, in UTF-16 code units: Windows Credential
/// Manager caps the blob at 2560 bytes and `keyring` stores UTF-16. Enforced on every
/// platform so behaviour is the same everywhere (about 1280 ASCII characters).
pub const MAX_SECRET_UTF16_UNITS: usize = 1280;

/// What a secret is for. Part of the keychain account name, so ids never collide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// API key for a BYOK provider or OpenAI-compatible endpoint (`byok:<provider>`).
    ProviderKey,
    /// OAuth / bearer token for an MCP server.
    McpToken,
    /// This device's own private keys for pairing and sync (ADR-0014 §2), e.g.
    /// `device/identity`. Never leaves the device.
    DeviceKey,
    /// A secret an add-on keeps (ADR-0013 §4), `addon/<add-on id>/<name>`. The host owns
    /// these ids and deletes them with the add-on's data.
    AddonSecret,
}

impl SecretKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::ProviderKey => "provider",
            Self::McpToken => "mcp",
            Self::DeviceKey => "device",
            Self::AddonSecret => "addon",
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

    pub fn device(id: impl Into<String>) -> Self {
        Self {
            kind: SecretKind::DeviceKey,
            id: id.into(),
        }
    }

    /// One of an add-on's secrets: `addon/<plugin_id>/<name>`.
    pub fn addon(plugin_id: &str, name: &str) -> Self {
        Self {
            kind: SecretKind::AddonSecret,
            id: format!("{plugin_id}/{name}"),
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
/// The buffer is zeroed on drop, including for clones (each clone owns a zeroizing copy).
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub(crate) Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
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
        Self::new(s)
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// Errors never contain secret values.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum SecretError {
    #[error("invalid secret id `{0}`")]
    InvalidId(String),
    #[error("empty secret value for `{0}`")]
    EmptyValue(String),
    /// Value longer than [`MAX_SECRET_UTF16_UNITS`].
    #[error("secret `{id}` is too large (max {max} UTF-16 code units)")]
    TooLarge { id: String, max: usize },
    /// The user (or OS policy) refused access to the keychain, or it is locked.
    #[error("access to the OS keychain was denied: {0}")]
    Denied(String),
    /// No usable OS keychain (e.g. no Secret Service on a headless Linux box).
    #[error("OS keychain unavailable: {0}")]
    Unavailable(String),
    /// A Node's passphrase-protected store hasn't been unlocked yet (`dn node unlock`).
    #[error("the secret store is locked; unlock it with `dn node unlock`")]
    Locked,
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

pub(crate) fn check(id: &SecretId, value: Option<&Secret>) -> Result<(), SecretError> {
    id.validate()?;
    if let Some(v) = value {
        if v.0.is_empty() {
            return Err(SecretError::EmptyValue(id.account()));
        }
        if v.0.encode_utf16().count() > MAX_SECRET_UTF16_UNITS {
            return Err(SecretError::TooLarge {
                id: id.account(),
                max: MAX_SECRET_UTF16_UNITS,
            });
        }
    }
    Ok(())
}

/// Set in CI's Rust test job (and by anyone who wants the same check locally): the OS
/// keychain may then only be opened under a test service name, one ending in `.test`, so
/// no test can read, overwrite or delete the app's real keys. `cargo test` of this crate
/// checks the same without it. Tests use [`MemoryStore`].
pub const TEST_KEYCHAIN_ONLY_ENV: &str = "DEMIOS_TEST_KEYCHAIN_ONLY";

/// Panics when a test opens the real keychain (see [`TEST_KEYCHAIN_ONLY_ENV`]). A panic,
/// not an error: callers may treat a keychain error as "no key", which would hide it.
fn refuse_real_keychain_in_tests(service: &str) {
    let guarded = cfg!(test) || std::env::var_os(TEST_KEYCHAIN_ONLY_ENV).is_some();
    assert!(
        !guarded || service.ends_with(".test"),
        "a test opened the real OS keychain (service `{service}`): use MemoryStore, or a \
         keychain test under a service name ending in `.test`"
    );
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
        let service = service.into();
        refuse_real_keychain_in_tests(&service);
        Self { service }
    }

    fn entry(&self, id: &SecretId) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, &id.account()).map_err(|e| map_err(id, e))
    }
}

fn map_err(id: &SecretId, e: keyring::Error) -> SecretError {
    match e {
        keyring::Error::PlatformFailure(_) => SecretError::Unavailable(e.to_string()),
        keyring::Error::NoStorageAccess(_) => SecretError::Denied(e.to_string()),
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
            Ok(v) => Ok(Some(Secret::new(v))),
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

/// Async front for any [`SecretStore`]: every call runs on Tokio's blocking pool, so
/// it is safe inside a runtime (the Linux Secret Service backend panics otherwise).
#[derive(Clone)]
pub struct AsyncSecrets {
    store: Arc<dyn SecretStore>,
}

impl AsyncSecrets {
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }

    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn SecretStore) -> Result<T, SecretError> + Send + 'static,
    ) -> Result<T, SecretError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(&*store))
            .await
            .map_err(|e| SecretError::Unavailable(format!("keychain task failed: {e}")))?
    }

    pub async fn get(&self, id: SecretId) -> Result<Option<Secret>, SecretError> {
        self.run(move |s| s.get(&id)).await
    }

    pub async fn set(&self, id: SecretId, value: Secret) -> Result<(), SecretError> {
        self.run(move |s| s.set(&id, &value)).await
    }

    pub async fn delete(&self, id: SecretId) -> Result<bool, SecretError> {
        self.run(move |s| s.delete(&id)).await
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
        let value = "sk-live-should-not-print-0123456789";
        let s = Secret::new(value);
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Config {
            key: Secret,
        }
        let shown = format!("{s:?} {s} {:?}", Config { key: s.clone() });
        assert!(!shown.contains(value), "{shown}");
        assert_eq!(s.expose(), value);
        // Errors name the id, never the value.
        let store = MemoryStore::new();
        let big = Secret::new(format!("{value}{}", "x".repeat(MAX_SECRET_UTF16_UNITS)));
        let err = store.set(&SecretId::mcp("github"), &big).unwrap_err();
        assert!(!err.to_string().contains(value));
        assert!(err.to_string().contains("mcp/github"));
    }

    #[test]
    fn too_large_is_rejected_on_every_store() {
        let id = SecretId::mcp("github");
        let at_limit = Secret::new("a".repeat(MAX_SECRET_UTF16_UNITS));
        // Non-BMP chars take two UTF-16 units each.
        let over = Secret::new("😀".repeat(MAX_SECRET_UTF16_UNITS / 2 + 1));
        let store = MemoryStore::new();
        store.set(&id, &at_limit).unwrap();
        assert_eq!(
            store.set(&id, &over),
            Err(SecretError::TooLarge {
                id: "mcp/github".into(),
                max: MAX_SECRET_UTF16_UNITS
            })
        );
        // The keychain store validates before touching the OS.
        assert!(matches!(
            KeyringStore::new("io.demios.app.test").set(&id, &over),
            Err(SecretError::TooLarge { .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn async_wrapper_works_inside_a_runtime() {
        let s = AsyncSecrets::new(Arc::new(MemoryStore::new()));
        let id = SecretId::provider("anthropic");
        s.set(id.clone(), "k-test".into()).await.unwrap();
        assert_eq!(s.get(id.clone()).await.unwrap().unwrap().expose(), "k-test");
        assert!(s.delete(id).await.unwrap());
    }

    /// Headless Linux (CI): no Secret Service → typed `Unavailable`, not a panic or hang.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_without_session_bus_is_unavailable() {
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
            eprintln!("skipped: a session bus is available");
            return;
        }
        let r = KeyringStore::new("io.demios.app.test").get(&SecretId::provider("openai"));
        assert!(matches!(r, Err(SecretError::Unavailable(_))), "{r:?}");
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

    #[test]
    #[should_panic(expected = "a test opened the real OS keychain")]
    fn a_test_cant_open_the_apps_real_keychain() {
        let _ = KeyringStore::default();
    }

    /// Touches the real OS keychain; run with `cargo test -p dn-secrets -- --ignored`.
    #[test]
    #[ignore = "uses the real OS keychain"]
    fn keyring_store_roundtrip() {
        roundtrip(&KeyringStore::new("io.demios.app.test"));
    }
}
