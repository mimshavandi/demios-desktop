//! Provider keys (T-SHELL-02) live in the OS keychain through `dn-secrets`
//! (`KeyringStore`); tests use its `MemoryStore`. Keys are never written to SQLite, never
//! logged and never sent to the UI (only "has a key" is). Every call can block on the
//! keychain, so callers run on a blocking thread.

use dn_secrets::{SecretId, SecretStore};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub use dn_secrets::{Secret, SecretError};

/// The key store plus a short description the settings screen shows.
#[derive(Clone)]
pub struct Keys {
    pub store: Arc<dyn SecretStore>,
    pub description: &'static str,
}

impl Keys {
    /// The OS keychain (macOS Keychain, Windows Credential Manager, Secret Service).
    /// Never in tests: they use [`Self::memory`] (`AppState::open_for_tests`), so
    /// `cargo test` can't touch the developer's real keys (#298).
    pub fn os_keychain() -> Self {
        if cfg!(test) {
            panic!(
                "a test opened the real OS keychain: use Keys::memory() or AppState::open_for_tests"
            );
        }
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

/// Whether a key is saved, as far as the keychain could tell (#200).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyState {
    Saved,
    None,
    /// The keychain didn't answer or failed: [`KeychainStatus`] says why. Never shown as
    /// "no key".
    Unknown,
}

/// The keychain as a whole, shown once in Settings › Models with what to do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum KeychainStatus {
    Ok,
    /// Still waiting for the keychain (it may be showing an unlock prompt).
    Checking,
    Unavailable {
        message: String,
    },
    Denied {
        message: String,
    },
    Failed {
        message: String,
    },
}

#[cfg(target_os = "linux")]
const UNAVAILABLE: &str =
    "No system keychain found. Install GNOME Keyring or KWallet to save API keys.";
#[cfg(not(target_os = "linux"))]
const UNAVAILABLE: &str = "Your system keychain isn't available, so Almend can't tell which API keys are saved. Try again.";

impl KeychainStatus {
    pub(crate) fn from_error(e: &SecretError) -> Self {
        match e {
            SecretError::Unavailable(_) => Self::Unavailable {
                message: UNAVAILABLE.into(),
            },
            SecretError::Denied(_) => Self::Denied {
                message: "Almend wasn't allowed to read your system keychain, so it can't tell which API keys are saved. Allow access, then try again.".into(),
            },
            e => {
                // SecretError never contains secret values.
                tracing::warn!("keychain check failed: {e}");
                Self::Failed {
                    message: "Couldn't read your system keychain, so Almend can't tell which API keys are saved. Try again.".into(),
                }
            }
        }
    }

    /// The problem as an IPC error, for a command that needed a key it couldn't read.
    pub fn error(&self) -> Option<crate::error::IpcError> {
        let code = match self {
            Self::Ok => return None,
            Self::Checking => "key_store_checking",
            Self::Unavailable { .. } => "key_store_unavailable",
            Self::Denied { .. } => "key_store_denied",
            Self::Failed { .. } => "key_store",
        };
        Some(crate::error::IpcError::new(code, self.message()?))
    }

    pub fn is_problem(&self) -> bool {
        !matches!(self, Self::Ok)
    }

    /// The message to show, if the keychain isn't fine.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Ok => None,
            Self::Checking => {
                Some("Still checking your system keychain. If it asked for a password, answer it.")
            }
            Self::Unavailable { message } | Self::Denied { message } | Self::Failed { message } => {
                Some(message)
            }
        }
    }
}

/// Which of some keys are saved, read in one pass. The catalog and model resolution use this
/// instead of asking the keychain themselves, so a keychain failure shows as "unknown" with
/// its reason, never as "no key".
#[derive(Clone, Debug)]
pub struct KeyCheck {
    states: HashMap<String, KeyState>,
    pub status: KeychainStatus,
}

impl KeyCheck {
    /// Asks the keychain about each id. Blocking. Stops at the first failure: a keychain that
    /// is missing, locked or refusing won't answer for the rest either.
    pub fn run<I, S>(keys: &Keys, ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut states = HashMap::new();
        let mut status = KeychainStatus::Ok;
        for id in ids.into_iter().map(Into::into) {
            let state = if status.is_problem() {
                KeyState::Unknown
            } else {
                match keys.has(&id) {
                    Ok(true) => KeyState::Saved,
                    Ok(false) => KeyState::None,
                    Err(e) => {
                        status = KeychainStatus::from_error(&e);
                        KeyState::Unknown
                    }
                }
            };
            states.insert(id, state);
        }
        Self { states, status }
    }

    /// Every key unknown while the keychain hasn't answered yet.
    pub fn checking() -> Self {
        Self {
            states: HashMap::new(),
            status: KeychainStatus::Checking,
        }
    }

    /// The state of one key; `Unknown` for an id this check didn't cover.
    pub fn state(&self, id: &str) -> KeyState {
        self.states.get(id).copied().unwrap_or(KeyState::Unknown)
    }

    pub fn saved(&self, id: &str) -> bool {
        self.state(id) == KeyState::Saved
    }
}

type Pending = watch::Receiver<Option<Arc<KeyCheck>>>;

/// The keychain check that is running, if any. Only one runs at a time: a keychain waiting
/// on an unlock prompt is not asked again (which could stack prompts), and a later caller
/// gets the same answer once the user has answered.
#[derive(Default)]
pub struct KeyChecks {
    running: Mutex<Option<(Vec<String>, Pending)>>,
}

impl KeyChecks {
    /// Joins the running check for the same ids, or starts one on a blocking thread. The
    /// keychain call has no time limit: a user may be answering its prompt.
    pub fn start(&self, keys: &Keys, ids: Vec<String>) -> Pending {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((for_ids, rx)) = running.as_ref()
            && *for_ids == ids
            && rx.borrow().is_none()
            && rx.has_changed().is_ok()
        {
            return rx.clone();
        }
        let (tx, rx) = watch::channel(None);
        *running = Some((ids.clone(), rx.clone()));
        let keys = keys.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _ = tx.send(Some(Arc::new(KeyCheck::run(&keys, ids))));
        });
        rx
    }
}

/// The check's answer, however long the keychain takes.
pub async fn answer(mut rx: Pending) -> Arc<KeyCheck> {
    loop {
        let got = rx.borrow_and_update().clone();
        if let Some(c) = got {
            return c;
        }
        if rx.changed().await.is_err() {
            tracing::warn!("keychain check stopped without an answer");
            return Arc::new(KeyCheck {
                states: HashMap::new(),
                status: KeychainStatus::Failed {
                    message: "Couldn't read your system keychain, so Almend can't tell which API keys are saved. Try again.".into(),
                },
            });
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "a test opened the real OS keychain")]
    fn a_test_cant_open_the_os_keychain() {
        let _ = Keys::os_keychain();
    }

    #[test]
    #[should_panic(expected = "a test opened the real OS keychain")]
    fn a_test_cant_open_the_app_state_with_the_os_keychain() {
        let dir = std::env::temp_dir().join(format!("dn-keys-{}", dn_core::Id::new()));
        let _ = crate::state::AppState::open(&dir.join("db.sqlite3"), None);
    }

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

    /// A keychain that fails every call with the given error.
    pub(crate) struct Failing(pub fn() -> SecretError);

    impl SecretStore for Failing {
        fn get(&self, _: &SecretId) -> Result<Option<Secret>, SecretError> {
            Err((self.0)())
        }
        fn set(&self, _: &SecretId, _: &Secret) -> Result<(), SecretError> {
            Err((self.0)())
        }
        fn delete(&self, _: &SecretId) -> Result<bool, SecretError> {
            Err((self.0)())
        }
    }

    pub(crate) fn failing(e: fn() -> SecretError) -> Keys {
        Keys {
            store: Arc::new(Failing(e)),
            description: "failing (tests)",
        }
    }

    #[test]
    fn key_check_reports_saved_and_missing_keys() {
        let k = Keys::memory();
        k.set("openai", &Secret::new("test-value")).unwrap();
        let c = KeyCheck::run(&k, ["openai", "anthropic"]);
        assert_eq!(c.status, KeychainStatus::Ok);
        assert_eq!(c.state("openai"), KeyState::Saved);
        assert_eq!(c.state("anthropic"), KeyState::None);
        assert_eq!(c.state("never-asked"), KeyState::Unknown);
    }

    #[test]
    fn an_unavailable_keychain_is_unknown_not_no_key() {
        let k = failing(|| SecretError::Unavailable("no secret service".into()));
        let c = KeyCheck::run(&k, ["openai", "anthropic"]);
        assert!(matches!(c.status, KeychainStatus::Unavailable { .. }));
        assert_eq!(c.state("openai"), KeyState::Unknown);
        assert_eq!(c.state("anthropic"), KeyState::Unknown);
        #[cfg(target_os = "linux")]
        assert_eq!(
            c.status.message(),
            Some("No system keychain found. Install GNOME Keyring or KWallet to save API keys.")
        );
    }

    /// A keychain that doesn't answer until the test lets it (a locked keyring whose unlock
    /// prompt the user hasn't answered yet). Counts the calls.
    struct Hanging {
        gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl SecretStore for Hanging {
        fn get(&self, _: &SecretId) -> Result<Option<Secret>, SecretError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = self.gate.lock().unwrap().recv();
            Ok(Some(Secret::new("test-value")))
        }
        fn set(&self, _: &SecretId, _: &Secret) -> Result<(), SecretError> {
            Ok(())
        }
        fn delete(&self, _: &SecretId) -> Result<bool, SecretError> {
            Ok(true)
        }
    }

    #[test]
    fn a_keychain_that_does_not_answer_is_asked_once_and_its_answer_is_used() {
        let (release, gate) = std::sync::mpsc::channel();
        let store = Arc::new(Hanging {
            gate: std::sync::Mutex::new(gate),
            calls: Default::default(),
        });
        let keys = Keys {
            store: store.clone(),
            description: "hanging (tests)",
        };
        let checks = KeyChecks::default();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let first = checks.start(&keys, vec!["openai".into()]);
            let waited =
                tokio::time::timeout(std::time::Duration::from_millis(200), answer(first)).await;
            assert!(waited.is_err(), "no answer yet");
            // A second caller (Retry, another screen) joins the same check: one prompt.
            let second = checks.start(&keys, vec!["openai".into()]);
            release.send(()).unwrap();
            let c = answer(second).await;
            assert_eq!(c.status, KeychainStatus::Ok);
            assert_eq!(c.state("openai"), KeyState::Saved);
        });
        assert_eq!(store.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_denied_keychain_is_unknown_and_says_so() {
        let k = failing(|| SecretError::Denied("locked".into()));
        let c = KeyCheck::run(&k, ["openai"]);
        assert!(matches!(c.status, KeychainStatus::Denied { .. }));
        assert_eq!(c.state("openai"), KeyState::Unknown);
        assert!(c.status.message().unwrap().contains("wasn't allowed"));
    }
}
