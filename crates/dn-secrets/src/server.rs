//! Secret stores for a Demios Node: a machine that may have no unlocked desktop keychain
//! (ADR-0014 §7).
//!
//! | Choice | Where secrets live | Restarts unattended | Stolen disk |
//! |---|---|---|---|
//! | [`ServerStoreChoice::Keychain`] | macOS system keychain, or Windows Credential Manager (DPAPI) under the Node's service account | yes | protected by the OS |
//! | [`ServerStoreChoice::EncryptedFile`] with a key from the service manager | `secrets.enc` in the data folder; the key comes from a systemd credential (sealed to the TPM by `LoadCredentialEncrypted=`), a Docker or Podman secret, or a key file on another mount | yes | protected if the key isn't on the same disk |
//! | [`ServerStoreChoice::EncryptedFile`] with [`KeySource::Passphrase`] | `secrets.enc`; the key comes from a passphrase typed at start (`dn node unlock`) | no: the Node waits locked | protected |
//! | [`ServerStoreChoice::InsecurePlaintext`] | `secrets.plain`, owner-only, **not encrypted** | yes | not protected |
//!
//! - **No fallback.** The Node records its choice at install, and [`open`] opens exactly
//!   that store or fails with the reason. It never moves secrets to another store by itself.
//! - **Plaintext only behind an explicit flag**, and its [`StoreReport`] says it's
//!   unprotected, so every client can show a warning.
//! - **The encrypted file:**
//!   - The file is AES-256-GCM, with a fresh random nonce on every write.
//!   - Its key is HKDF-SHA256 over the master key and a per-file salt. For a passphrase, the
//!     master key is PBKDF2-HMAC-SHA256 with [`PBKDF2_ITERATIONS`] rounds.
//!   - The header (format, KDF, rounds, salt) is authenticated as associated data.
//!   - It's written to a temporary file and renamed into place, owner-only on Unix.
//! - A key file must be owner-only and **on a different file system from the data
//!   folder**. Otherwise a stolen disk would carry both, and the store refuses to open.

use crate::{Secret, SecretError, SecretId, SecretStore, check};
use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use aws_lc_rs::{hkdf, pbkdf2, rand};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use zeroize::Zeroizing;

/// PBKDF2-HMAC-SHA256 rounds for a passphrase (OWASP's 2023 figure for this KDF).
pub const PBKDF2_ITERATIONS: u32 = 600_000;
/// Rounds a file may ask for; outside this range the file is refused.
const MIN_ITERATIONS: u32 = 100_000;
const MAX_ITERATIONS: u32 = 10_000_000;
/// Name of the encrypted file in the data folder.
pub const ENCRYPTED_FILE: &str = "secrets.enc";
/// Name of the plaintext file, used only with the insecure flag.
pub const PLAINTEXT_FILE: &str = "secrets.plain";
/// Where Docker and Podman mount secrets.
pub const CONTAINER_SECRETS_DIR: &str = "/run/secrets";

const MAGIC_ENCRYPTED: &[u8; 8] = b"DNSECv1\0";
const MAGIC_PLAINTEXT: &[u8; 8] = b"DNSECpt1";
const KDF_EXTERNAL: u8 = 0;
const KDF_PBKDF2: u8 = 1;
const SALT_LEN: usize = 16;
/// magic, kdf, rounds, salt
const HEADER_LEN: usize = 8 + 1 + 4 + SALT_LEN;
const HKDF_INFO: &[u8] = b"demios secrets file v1";
const MAX_ENTRIES: usize = 4096;

/// Where an encrypted file's key comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeySource {
    /// `LoadCredentialEncrypted=<name>` in the systemd unit: systemd decrypts it (with the
    /// TPM when it was sealed with `--with-key=tpm2`) into `$CREDENTIALS_DIRECTORY/<name>`.
    SystemdCredential { name: String },
    /// A Docker or Podman secret, mounted at `/run/secrets/<name>`.
    ContainerSecret { name: String },
    /// A key file on a mount other than the data folder's.
    KeyFile { path: PathBuf },
    /// A passphrase typed at start.
    Passphrase,
}

/// The store a Node was installed with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerStoreChoice {
    Keychain,
    EncryptedFile(KeySource),
    /// For test setups only (`--insecure-plaintext-secrets`).
    InsecurePlaintext,
}

/// What a store is, for every client's status screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreReport {
    /// Stable id: `keychain`, `systemd_credential`, `container_secret`, `key_file`,
    /// `passphrase` or `insecure_plaintext`.
    pub kind: &'static str,
    /// What the status screen says, e.g. "Encrypted file, key from a systemd credential".
    pub label: String,
    /// `false` for the plaintext store: clients show a warning.
    pub protected_at_rest: bool,
    /// `true` while a passphrase store waits for `dn node unlock`.
    pub locked: bool,
}

/// Reads environment variables; a parameter so tests don't touch the real environment.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// Opens the store the Node was installed with. A store that can't be opened is an error
/// that says why; nothing else is tried.
pub fn open(
    choice: &ServerStoreChoice,
    data_dir: &Path,
    env: Env<'_>,
) -> Result<Box<dyn ServerStore>, SecretError> {
    match choice {
        ServerStoreChoice::Keychain => Ok(Box::new(KeychainServerStore(
            crate::KeyringStore::default(),
        ))),
        ServerStoreChoice::EncryptedFile(KeySource::Passphrase) => Ok(Box::new(
            EncryptedFileStore::locked(data_dir.join(ENCRYPTED_FILE)),
        )),
        ServerStoreChoice::EncryptedFile(source) => {
            let key = read_key(source, data_dir, env, Path::new(CONTAINER_SECRETS_DIR))?;
            Ok(Box::new(EncryptedFileStore::open_with_key(
                data_dir.join(ENCRYPTED_FILE),
                source.clone(),
                &key,
            )?))
        }
        ServerStoreChoice::InsecurePlaintext => Ok(Box::new(PlaintextFileStore::open(
            data_dir.join(PLAINTEXT_FILE),
        )?)),
    }
}

/// A [`SecretStore`] that can say what it is.
pub trait ServerStore: SecretStore {
    fn report(&self) -> StoreReport;
    /// Unlocks a passphrase store. Other stores aren't locked and refuse this.
    fn unlock(&self, _passphrase: &Secret) -> Result<(), SecretError> {
        Err(no_passphrase())
    }
}

/// The platform keychain under the Node's service account.
struct KeychainServerStore(crate::KeyringStore);

impl SecretStore for KeychainServerStore {
    fn get(&self, id: &SecretId) -> Result<Option<Secret>, SecretError> {
        self.0.get(id)
    }
    fn set(&self, id: &SecretId, value: &Secret) -> Result<(), SecretError> {
        self.0.set(id, value)
    }
    fn delete(&self, id: &SecretId) -> Result<bool, SecretError> {
        self.0.delete(id)
    }
}

impl ServerStore for KeychainServerStore {
    fn report(&self) -> StoreReport {
        let label = if cfg!(target_os = "macos") {
            "System keychain"
        } else if cfg!(windows) {
            "Windows Credential Manager (DPAPI) under the service account"
        } else {
            "System keychain (Secret Service)"
        };
        StoreReport {
            kind: "keychain",
            label: label.into(),
            protected_at_rest: true,
            locked: false,
        }
    }
}

fn no_passphrase() -> SecretError {
    SecretError::Backend {
        id: "store".into(),
        message: "this secret store has no passphrase".into(),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Reads a 32-byte master key: the raw bytes, or 64 hex digits (a trailing newline is
/// allowed), so both `head -c 32 /dev/urandom` and `openssl rand -hex 32` work.
fn parse_key(bytes: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
    let mut key = Zeroizing::new([0u8; 32]);
    if bytes.len() == 32 {
        key.copy_from_slice(bytes);
        return Some(key);
    }
    let text = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let text = text.strip_suffix(b"\r").unwrap_or(text);
    if text.len() != 64 {
        return None;
    }
    for (i, pair) in text.chunks(2).enumerate() {
        let hex = std::str::from_utf8(pair).ok()?;
        key[i] = u8::from_str_radix(hex, 16).ok()?;
    }
    Some(key)
}

fn unavailable(message: impl Into<String>) -> SecretError {
    SecretError::Unavailable(message.into())
}

fn read_key_file(path: &Path, what: &str) -> Result<Zeroizing<[u8; 32]>, SecretError> {
    let bytes = Zeroizing::new(fs::read(path).map_err(|e| {
        unavailable(format!(
            "couldn't read the {what} at {}: {e}",
            path.display()
        ))
    })?);
    parse_key(&bytes).ok_or_else(|| {
        unavailable(format!(
            "the {what} at {} isn't a 32-byte key (raw, or 64 hex digits)",
            path.display()
        ))
    })
}

/// Refuses a key file inside the data folder, on every platform.
fn check_not_inside(key: &Path, data_dir: &Path) -> Result<(), SecretError> {
    let key_real = fs::canonicalize(key).map_err(|e| {
        unavailable(format!(
            "couldn't read the key file at {}: {e}",
            key.display()
        ))
    })?;
    let data = fs::canonicalize(data_dir).map_err(|e| {
        unavailable(format!(
            "couldn't read the data folder {}: {e}",
            data_dir.display()
        ))
    })?;
    if key_real.starts_with(&data) {
        return Err(unavailable(format!(
            "the key file at {} is inside the data folder; put it on a separate mount",
            key.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn check_key_file_placement(key: &Path, data_dir: &Path) -> Result<(), SecretError> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(key).map_err(|e| {
        unavailable(format!(
            "couldn't read the key file at {}: {e}",
            key.display()
        ))
    })?;
    if meta.mode() & 0o077 != 0 {
        return Err(SecretError::Denied(format!(
            "the key file at {} can be read by other users; make it owner-only (chmod 600)",
            key.display()
        )));
    }
    check_not_inside(key, data_dir)?;
    let data = fs::metadata(data_dir).map_err(|e| {
        unavailable(format!(
            "couldn't read the data folder {}: {e}",
            data_dir.display()
        ))
    })?;
    if meta.dev() == data.dev() {
        return Err(unavailable(format!(
            "the key file at {} is on the same disk as the secrets; put it on a separate mount",
            key.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_key_file_placement(key: &Path, data_dir: &Path) -> Result<(), SecretError> {
    // No device numbers here: refuse at least a key file inside the data folder.
    check_not_inside(key, data_dir)
}

fn read_key(
    source: &KeySource,
    data_dir: &Path,
    env: Env<'_>,
    container_dir: &Path,
) -> Result<Zeroizing<[u8; 32]>, SecretError> {
    match source {
        KeySource::SystemdCredential { name } => {
            if !valid_name(name) {
                return Err(SecretError::InvalidId(name.clone()));
            }
            let dir = env("CREDENTIALS_DIRECTORY").ok_or_else(|| {
                unavailable(
                    "no systemd credentials: start the Node from its systemd unit, which loads the key with LoadCredentialEncrypted=",
                )
            })?;
            read_key_file(&Path::new(&dir).join(name), "systemd credential")
        }
        KeySource::ContainerSecret { name } => {
            if !valid_name(name) {
                return Err(SecretError::InvalidId(name.clone()));
            }
            read_key_file(&container_dir.join(name), "container secret")
        }
        KeySource::KeyFile { path } => {
            check_key_file_placement(path, data_dir)?;
            read_key_file(path, "key file")
        }
        KeySource::Passphrase => Err(SecretError::Locked),
    }
}

fn random<const N: usize>() -> Result<[u8; N], SecretError> {
    let mut out = [0u8; N];
    rand::fill(&mut out).map_err(|_| unavailable("the system random number generator failed"))?;
    Ok(out)
}

fn file_key(master: &[u8; 32], salt: &[u8; SALT_LEN]) -> Result<Zeroizing<[u8; 32]>, SecretError> {
    let crypto = || unavailable("key derivation failed");
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(master);
    let okm = prk
        .expand(&[HKDF_INFO], hkdf::HKDF_SHA256)
        .map_err(|_| crypto())?;
    let mut key = Zeroizing::new([0u8; 32]);
    okm.fill(key.as_mut()).map_err(|_| crypto())?;
    Ok(key)
}

fn passphrase_master(
    passphrase: &Secret,
    salt: &[u8; SALT_LEN],
    iterations: u32,
) -> Result<Zeroizing<[u8; 32]>, SecretError> {
    let rounds = NonZeroU32::new(iterations).ok_or_else(|| unavailable("bad round count"))?;
    let mut master = Zeroizing::new([0u8; 32]);
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        rounds,
        salt,
        passphrase.expose().as_bytes(),
        master.as_mut(),
    );
    Ok(master)
}

type Entries = HashMap<String, Zeroizing<String>>;

/// `count, then (account length, account, value length, value)` per entry, big-endian.
fn encode_entries(entries: &Entries) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::new());
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    let mut accounts: Vec<_> = entries.keys().collect();
    accounts.sort();
    for account in accounts {
        let value = &entries[account];
        out.extend_from_slice(&(account.len() as u32).to_be_bytes());
        out.extend_from_slice(account.as_bytes());
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    }
    out
}

fn decode_entries(bytes: &[u8]) -> Option<Entries> {
    fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        if bytes.len() < n {
            return None;
        }
        let (head, rest) = bytes.split_at(n);
        *bytes = rest;
        Some(head)
    }
    fn len(bytes: &mut &[u8]) -> Option<usize> {
        Some(u32::from_be_bytes(take(bytes, 4)?.try_into().ok()?) as usize)
    }
    let mut rest = bytes;
    let count = len(&mut rest)?;
    if count > MAX_ENTRIES {
        return None;
    }
    let mut entries = Entries::with_capacity(count);
    for _ in 0..count {
        let n = len(&mut rest)?;
        let account = std::str::from_utf8(take(&mut rest, n)?).ok()?.to_owned();
        let n = len(&mut rest)?;
        let value = Zeroizing::new(std::str::from_utf8(take(&mut rest, n)?).ok()?.to_owned());
        if entries.insert(account, value).is_some() {
            return None;
        }
    }
    rest.is_empty().then_some(entries)
}

/// Writes `bytes` to `path` through a temporary file and a rename, owner-only on Unix.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), SecretError> {
    let fail = |e: std::io::Error| SecretError::Backend {
        id: "store".into(),
        message: format!("couldn't write {}: {e}", path.display()),
    };
    let dir = path
        .parent()
        .ok_or_else(|| unavailable("the secrets file has no folder"))?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("secrets")
    ));
    // A leftover temp file (or a link planted there) is removed, and the new one must not
    // exist yet, so its owner-only mode always applies.
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(fail(e)),
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(fail)?;
    file.write_all(bytes).map_err(fail)?;
    file.sync_all().map_err(fail)?;
    drop(file);
    fs::rename(&tmp, path).map_err(fail)?;
    Ok(())
}

#[cfg(unix)]
fn check_owner_only(path: &Path) -> Result<(), SecretError> {
    use std::os::unix::fs::MetadataExt;
    match fs::metadata(path) {
        Ok(meta) if meta.mode() & 0o077 != 0 => Err(SecretError::Denied(format!(
            "{} can be read by other users; make it owner-only (chmod 600)",
            path.display()
        ))),
        _ => Ok(()),
    }
}

#[cfg(not(unix))]
fn check_owner_only(_path: &Path) -> Result<(), SecretError> {
    Ok(())
}

struct Unlocked {
    key: LessSafeKey,
    header: [u8; HEADER_LEN],
    entries: Entries,
}

/// The encrypted secrets file.
pub struct EncryptedFileStore {
    path: PathBuf,
    source: KeySource,
    state: Mutex<Option<Unlocked>>,
}

impl std::fmt::Debug for EncryptedFileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedFileStore")
            .field("path", &self.path)
            .field("source", &self.source)
            .field("locked", &self.is_locked())
            .finish()
    }
}

fn header(kdf: u8, iterations: u32, salt: &[u8; SALT_LEN]) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..8].copy_from_slice(MAGIC_ENCRYPTED);
    h[8] = kdf;
    h[9..13].copy_from_slice(&iterations.to_be_bytes());
    h[13..].copy_from_slice(salt);
    h
}

fn aead_key(key: &[u8; 32]) -> Result<LessSafeKey, SecretError> {
    Ok(LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, key).map_err(|_| unavailable("bad key"))?,
    ))
}

fn damaged(path: &Path) -> SecretError {
    SecretError::Backend {
        id: "store".into(),
        message: format!(
            "{} is damaged or isn't a Demios secrets file",
            path.display()
        ),
    }
}

impl EncryptedFileStore {
    /// Opens (or creates) the file with a key from the service manager.
    pub fn open_with_key(
        path: PathBuf,
        source: KeySource,
        master: &[u8; 32],
    ) -> Result<Self, SecretError> {
        if source == KeySource::Passphrase {
            return Err(unavailable(
                "a passphrase store is opened locked, then unlocked",
            ));
        }
        let store = Self {
            path,
            source,
            state: Mutex::new(None),
        };
        let unlocked = match store.read_file()? {
            Some(bytes) => store.decrypt(&bytes, KDF_EXTERNAL, |_, salt| file_key(master, salt))?,
            None => {
                let salt = random::<SALT_LEN>()?;
                let unlocked = Unlocked {
                    key: aead_key(&*file_key(master, &salt)?)?,
                    header: header(KDF_EXTERNAL, 0, &salt),
                    entries: Entries::new(),
                };
                store.write(&unlocked)?;
                unlocked
            }
        };
        *store.lock() = Some(unlocked);
        Ok(store)
    }

    /// A passphrase store, locked until [`unlock`](ServerStore::unlock) or
    /// [`initialize`](Self::initialize).
    pub fn locked(path: PathBuf) -> Self {
        Self {
            path,
            source: KeySource::Passphrase,
            state: Mutex::new(None),
        }
    }

    /// Creates a new passphrase-protected file. Refuses if one exists, so a typo can't
    /// replace the real secrets.
    pub fn initialize(&self, passphrase: &Secret) -> Result<(), SecretError> {
        self.initialize_with(passphrase, PBKDF2_ITERATIONS)
    }

    fn initialize_with(&self, passphrase: &Secret, iterations: u32) -> Result<(), SecretError> {
        if self.source != KeySource::Passphrase {
            return Err(unavailable("this store doesn't use a passphrase"));
        }
        if passphrase.expose().chars().count() < 12 {
            return Err(SecretError::Denied(
                "the passphrase must be at least 12 characters".into(),
            ));
        }
        if self.path.exists() {
            return Err(SecretError::Backend {
                id: "store".into(),
                message: format!("{} already exists; unlock it instead", self.path.display()),
            });
        }
        let salt = random::<SALT_LEN>()?;
        let master = passphrase_master(passphrase, &salt, iterations)?;
        let unlocked = Unlocked {
            key: aead_key(&*file_key(&master, &salt)?)?,
            header: header(KDF_PBKDF2, iterations, &salt),
            entries: Entries::new(),
        };
        self.write(&unlocked)?;
        *self.lock() = Some(unlocked);
        Ok(())
    }

    pub fn is_locked(&self) -> bool {
        self.lock().is_none()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Unlocked>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn read_file(&self) -> Result<Option<Vec<u8>>, SecretError> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                check_owner_only(&self.path)?;
                Ok(Some(bytes))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(unavailable(format!(
                "couldn't read {}: {e}",
                self.path.display()
            ))),
        }
    }

    fn decrypt(
        &self,
        bytes: &[u8],
        expected_kdf: u8,
        derive: impl FnOnce(u32, &[u8; SALT_LEN]) -> Result<Zeroizing<[u8; 32]>, SecretError>,
    ) -> Result<Unlocked, SecretError> {
        if bytes.len() < HEADER_LEN + NONCE_LEN + AES_256_GCM.tag_len()
            || &bytes[..8] != MAGIC_ENCRYPTED
        {
            return Err(damaged(&self.path));
        }
        let mut head = [0u8; HEADER_LEN];
        head.copy_from_slice(&bytes[..HEADER_LEN]);
        let kdf = head[8];
        if kdf != expected_kdf {
            return Err(unavailable(if kdf == KDF_PBKDF2 {
                "this secrets file is protected by a passphrase, not by a key from the service manager"
            } else {
                "this secrets file is protected by a key from the service manager, not by a passphrase"
            }));
        }
        let iterations = u32::from_be_bytes([head[9], head[10], head[11], head[12]]);
        let rounds_ok = match kdf {
            KDF_PBKDF2 => (MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations),
            _ => iterations == 0,
        };
        if !rounds_ok {
            return Err(damaged(&self.path));
        }
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&head[13..]);
        let key = aead_key(&*derive(iterations, &salt)?)?;
        let nonce = Nonce::try_assume_unique_for_key(&bytes[HEADER_LEN..HEADER_LEN + NONCE_LEN])
            .map_err(|_| damaged(&self.path))?;
        let mut sealed = Zeroizing::new(bytes[HEADER_LEN + NONCE_LEN..].to_vec());
        let plain = key
            .open_in_place(nonce, Aad::from(&head[..]), &mut sealed)
            .map_err(|_| {
                SecretError::Denied(if kdf == KDF_PBKDF2 {
                    "wrong passphrase, or the secrets file was changed".into()
                } else {
                    "the key doesn't open the secrets file, or the file was changed".into()
                })
            })?;
        let entries = decode_entries(plain).ok_or_else(|| damaged(&self.path))?;
        Ok(Unlocked {
            key,
            header: head,
            entries,
        })
    }

    fn write(&self, unlocked: &Unlocked) -> Result<(), SecretError> {
        let nonce_bytes = random::<NONCE_LEN>()?;
        let mut sealed = encode_entries(&unlocked.entries);
        unlocked
            .key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(&unlocked.header[..]),
                &mut *sealed,
            )
            .map_err(|_| unavailable("encryption failed"))?;
        let mut file = Vec::with_capacity(HEADER_LEN + NONCE_LEN + sealed.len());
        file.extend_from_slice(&unlocked.header);
        file.extend_from_slice(&nonce_bytes);
        file.extend_from_slice(&sealed);
        write_atomic(&self.path, &file)
    }

    fn with_unlocked<T>(
        &self,
        f: impl FnOnce(&mut Unlocked) -> Result<T, SecretError>,
    ) -> Result<T, SecretError> {
        let mut state = self.lock();
        let unlocked = state.as_mut().ok_or(SecretError::Locked)?;
        f(unlocked)
    }
}

impl SecretStore for EncryptedFileStore {
    fn get(&self, id: &SecretId) -> Result<Option<Secret>, SecretError> {
        check(id, None)?;
        self.with_unlocked(|u| {
            Ok(u.entries
                .get(&id.account())
                .map(|v| Secret::new(v.as_str())))
        })
    }

    fn set(&self, id: &SecretId, value: &Secret) -> Result<(), SecretError> {
        check(id, Some(value))?;
        self.with_unlocked(|u| {
            let previous = u
                .entries
                .insert(id.account(), Zeroizing::new(value.expose().to_owned()));
            if let Err(e) = self.write(u) {
                // Keep memory and disk the same: undo the change that wasn't saved.
                match previous {
                    Some(old) => u.entries.insert(id.account(), old),
                    None => u.entries.remove(&id.account()),
                };
                return Err(e);
            }
            Ok(())
        })
    }

    fn delete(&self, id: &SecretId) -> Result<bool, SecretError> {
        check(id, None)?;
        self.with_unlocked(|u| {
            let Some(old) = u.entries.remove(&id.account()) else {
                return Ok(false);
            };
            if let Err(e) = self.write(u) {
                u.entries.insert(id.account(), old);
                return Err(e);
            }
            Ok(true)
        })
    }
}

impl ServerStore for EncryptedFileStore {
    fn report(&self) -> StoreReport {
        let (kind, from) = match &self.source {
            KeySource::SystemdCredential { .. } => ("systemd_credential", "a systemd credential"),
            KeySource::ContainerSecret { .. } => ("container_secret", "a container secret"),
            KeySource::KeyFile { .. } => ("key_file", "a key file on another disk"),
            KeySource::Passphrase => ("passphrase", "a passphrase"),
        };
        StoreReport {
            kind,
            label: format!("Encrypted file, key from {from}"),
            protected_at_rest: true,
            locked: self.is_locked(),
        }
    }

    fn unlock(&self, passphrase: &Secret) -> Result<(), SecretError> {
        if self.source != KeySource::Passphrase {
            return Err(no_passphrase());
        }
        let bytes = self.read_file()?.ok_or_else(|| {
            unavailable("there's no secrets file yet; create one with `dn node unlock --new`")
        })?;
        let unlocked = self.decrypt(&bytes, KDF_PBKDF2, |iterations, salt| {
            file_key(&*passphrase_master(passphrase, salt, iterations)?, salt)
        })?;
        *self.lock() = Some(unlocked);
        Ok(())
    }
}

/// Secrets in a plaintext, owner-only file. Only for test setups; reported as unprotected.
pub struct PlaintextFileStore {
    path: PathBuf,
    entries: Mutex<Entries>,
}

impl std::fmt::Debug for PlaintextFileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaintextFileStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl PlaintextFileStore {
    pub fn open(path: PathBuf) -> Result<Self, SecretError> {
        let entries = match fs::read(&path) {
            Ok(bytes) => {
                check_owner_only(&path)?;
                let body = bytes
                    .strip_prefix(MAGIC_PLAINTEXT.as_slice())
                    .ok_or_else(|| damaged(&path))?;
                decode_entries(body).ok_or_else(|| damaged(&path))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Entries::new(),
            Err(e) => {
                return Err(unavailable(format!(
                    "couldn't read {}: {e}",
                    path.display()
                )));
            }
        };
        Ok(Self {
            path,
            entries: Mutex::new(entries),
        })
    }

    fn save(&self, entries: &Entries) -> Result<(), SecretError> {
        let mut bytes = Zeroizing::new(MAGIC_PLAINTEXT.to_vec());
        bytes.extend_from_slice(&encode_entries(entries));
        write_atomic(&self.path, &bytes)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl SecretStore for PlaintextFileStore {
    fn get(&self, id: &SecretId) -> Result<Option<Secret>, SecretError> {
        check(id, None)?;
        Ok(self
            .lock()
            .get(&id.account())
            .map(|v| Secret::new(v.as_str())))
    }

    fn set(&self, id: &SecretId, value: &Secret) -> Result<(), SecretError> {
        check(id, Some(value))?;
        let mut entries = self.lock();
        let previous = entries.insert(id.account(), Zeroizing::new(value.expose().to_owned()));
        if let Err(e) = self.save(&entries) {
            match previous {
                Some(old) => entries.insert(id.account(), old),
                None => entries.remove(&id.account()),
            };
            return Err(e);
        }
        Ok(())
    }

    fn delete(&self, id: &SecretId) -> Result<bool, SecretError> {
        check(id, None)?;
        let mut entries = self.lock();
        let Some(old) = entries.remove(&id.account()) else {
            return Ok(false);
        };
        if let Err(e) = self.save(&entries) {
            entries.insert(id.account(), old);
            return Err(e);
        }
        Ok(true)
    }
}

impl ServerStore for PlaintextFileStore {
    fn report(&self) -> StoreReport {
        StoreReport {
            kind: "insecure_plaintext",
            label: "Plaintext file (unprotected, for testing only)".into(),
            protected_at_rest: false,
            locked: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder under the system temp dir, removed on drop.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let n: [u8; 8] = random().unwrap();
            let dir = std::env::temp_dir().join(format!(
                "dn-secrets-test-{}",
                n.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const TEST_ROUNDS: u32 = MIN_ITERATIONS;
    const KEY: [u8; 32] = [42; 32];

    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    fn ext() -> KeySource {
        KeySource::ContainerSecret {
            name: "demios".into(),
        }
    }

    fn id() -> SecretId {
        SecretId::provider("openai")
    }

    #[test]
    fn the_encrypted_file_keeps_secrets_across_restarts_and_holds_no_plaintext() {
        let dir = TempDir::new();
        let path = dir.0.join(ENCRYPTED_FILE);
        let store = EncryptedFileStore::open_with_key(path.clone(), ext(), &KEY).unwrap();
        store.set(&id(), &"sk-test-not-real-1234".into()).unwrap();
        store
            .set(&SecretId::mcp("github"), &"ghp-test".into())
            .unwrap();
        assert!(store.delete(&SecretId::mcp("github")).unwrap());
        drop(store);

        let raw = fs::read(&path).unwrap();
        assert!(!raw.windows(8).any(|w| w == b"sk-test-"));
        assert!(!raw.windows(6).any(|w| w == b"openai"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        }

        let again = EncryptedFileStore::open_with_key(path, ext(), &KEY).unwrap();
        assert_eq!(
            again.get(&id()).unwrap().unwrap().expose(),
            "sk-test-not-real-1234"
        );
        assert_eq!(again.get(&SecretId::mcp("github")).unwrap(), None);
    }

    #[test]
    fn a_wrong_key_or_a_changed_file_is_refused_not_replaced() {
        let dir = TempDir::new();
        let path = dir.0.join(ENCRYPTED_FILE);
        let s = EncryptedFileStore::open_with_key(path.clone(), ext(), &KEY).unwrap();
        s.set(&id(), &"sk-test".into()).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(matches!(
            EncryptedFileStore::open_with_key(path.clone(), ext(), &[1; 32]),
            Err(SecretError::Denied(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), before);

        // Any changed byte, in the header or the body, is caught.
        for i in [9, HEADER_LEN + 2, before.len() - 1] {
            let mut changed = before.clone();
            changed[i] ^= 1;
            write_atomic(&path, &changed).unwrap();
            assert!(
                EncryptedFileStore::open_with_key(path.clone(), ext(), &KEY).is_err(),
                "byte {i}"
            );
        }
        write_atomic(&path, b"not a secrets file").unwrap();
        assert!(matches!(
            EncryptedFileStore::open_with_key(path, ext(), &KEY),
            Err(SecretError::Backend { .. })
        ));
    }

    #[test]
    fn a_passphrase_store_waits_locked_and_unlocks_with_the_right_passphrase() {
        let dir = TempDir::new();
        let path = dir.0.join(ENCRYPTED_FILE);
        let store = EncryptedFileStore::locked(path.clone());
        assert!(store.is_locked());
        assert!(matches!(store.get(&id()), Err(SecretError::Locked)));
        assert!(matches!(
            store.unlock(&"correct horse battery".into()),
            Err(SecretError::Unavailable(_))
        ));
        assert!(matches!(
            store.initialize_with(&"short".into(), TEST_ROUNDS),
            Err(SecretError::Denied(_))
        ));
        store
            .initialize_with(&"correct horse battery".into(), TEST_ROUNDS)
            .unwrap();
        store.set(&id(), &"sk-test".into()).unwrap();
        assert!(
            store
                .initialize_with(&"another passphrase".into(), TEST_ROUNDS)
                .is_err()
        );

        let restarted = EncryptedFileStore::locked(path.clone());
        assert!(restarted.report().locked);
        assert!(matches!(
            restarted.unlock(&"wrong horse battery".into()),
            Err(SecretError::Denied(_))
        ));
        assert!(restarted.is_locked());
        restarted.unlock(&"correct horse battery".into()).unwrap();
        assert_eq!(restarted.get(&id()).unwrap().unwrap().expose(), "sk-test");
        assert!(!restarted.report().locked);

        // A passphrase file can't be opened as if it had an external key, and back.
        assert!(matches!(
            EncryptedFileStore::open_with_key(path, ext(), &KEY),
            Err(SecretError::Unavailable(_))
        ));
    }

    #[test]
    fn a_systemd_credential_supplies_the_key() {
        let dir = TempDir::new();
        let creds = TempDir::new();
        fs::write(
            creds.0.join("demios-secrets"),
            format!("{}\n", "2a".repeat(32)),
        )
        .unwrap();
        let creds_path = creds.0.clone().into_os_string();
        let env = move |name: &str| (name == "CREDENTIALS_DIRECTORY").then(|| creds_path.clone());
        let choice = ServerStoreChoice::EncryptedFile(KeySource::SystemdCredential {
            name: "demios-secrets".into(),
        });
        let store = open(&choice, &dir.0, &env).unwrap();
        store.set(&id(), &"sk-test".into()).unwrap();
        assert_eq!(store.report().kind, "systemd_credential");
        assert_eq!(
            store.report().label,
            "Encrypted file, key from a systemd credential"
        );
        // The hex credential is the same key as the raw bytes.
        let direct =
            EncryptedFileStore::open_with_key(dir.0.join(ENCRYPTED_FILE), ext(), &KEY).unwrap();
        assert_eq!(direct.get(&id()).unwrap().unwrap().expose(), "sk-test");

        // Without systemd's directory, it fails and says how to start the Node.
        let Err(SecretError::Unavailable(why)) = open(&choice, &dir.0, &no_env) else {
            panic!("expected unavailable");
        };
        assert!(why.contains("systemd unit"));
        let traversal = ServerStoreChoice::EncryptedFile(KeySource::SystemdCredential {
            name: "../etc/passwd".into(),
        });
        assert!(matches!(
            open(&traversal, &dir.0, &env),
            Err(SecretError::InvalidId(_))
        ));
    }

    #[test]
    fn a_container_secret_supplies_the_key() {
        let dir = TempDir::new();
        let secrets = TempDir::new();
        fs::write(secrets.0.join("demios"), KEY).unwrap();
        let source = KeySource::ContainerSecret {
            name: "demios".into(),
        };
        let key = read_key(&source, &dir.0, &no_env, &secrets.0).unwrap();
        assert_eq!(*key, KEY);
        fs::write(secrets.0.join("short"), b"abc").unwrap();
        assert!(
            read_key(
                &KeySource::ContainerSecret {
                    name: "short".into()
                },
                &dir.0,
                &no_env,
                &secrets.0
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_key_file_on_the_same_disk_or_readable_by_others_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new();
        let key_path = dir.0.join("node.key");
        fs::write(&key_path, KEY).unwrap();
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
        let source = KeySource::KeyFile {
            path: key_path.clone(),
        };
        assert!(matches!(
            read_key(&source, &dir.0, &no_env, Path::new(CONTAINER_SECRETS_DIR)),
            Err(SecretError::Denied(_))
        ));
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();
        let Err(SecretError::Unavailable(why)) =
            read_key(&source, &dir.0, &no_env, Path::new(CONTAINER_SECRETS_DIR))
        else {
            panic!("expected the same-disk refusal");
        };
        assert!(why.contains("separate mount"));
    }

    #[cfg(unix)]
    #[test]
    fn a_secrets_file_readable_by_others_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new();
        let path = dir.0.join(ENCRYPTED_FILE);
        EncryptedFileStore::open_with_key(path.clone(), ext(), &KEY).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            EncryptedFileStore::open_with_key(path, ext(), &KEY),
            Err(SecretError::Denied(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_leftover_temp_file_does_not_keep_its_permissions() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = TempDir::new();
        let tmp = dir.0.join(".secrets.enc.tmp");
        fs::write(&tmp, b"old").unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644)).unwrap();
        let path = dir.0.join(ENCRYPTED_FILE);
        EncryptedFileStore::open_with_key(path.clone(), ext(), &KEY).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn the_plaintext_store_works_only_when_chosen_and_says_it_is_unprotected() {
        let dir = TempDir::new();
        let store = open(&ServerStoreChoice::InsecurePlaintext, &dir.0, &no_env).unwrap();
        store.set(&id(), &"sk-test".into()).unwrap();
        let report = store.report();
        assert!(!report.protected_at_rest);
        assert_eq!(report.kind, "insecure_plaintext");
        drop(store);
        let again = open(&ServerStoreChoice::InsecurePlaintext, &dir.0, &no_env).unwrap();
        assert_eq!(again.get(&id()).unwrap().unwrap().expose(), "sk-test");
        // The encrypted choice never reads the plaintext file.
        assert!(!dir.0.join(ENCRYPTED_FILE).exists());
    }

    #[test]
    fn opening_one_store_never_falls_back_to_another() {
        let dir = TempDir::new();
        let choice = ServerStoreChoice::EncryptedFile(KeySource::ContainerSecret {
            name: "missing-secret".into(),
        });
        assert!(open(&choice, &dir.0, &no_env).is_err());
        assert!(
            fs::read_dir(&dir.0).unwrap().next().is_none(),
            "nothing written"
        );
    }

    #[test]
    fn entries_are_decoded_strictly() {
        let mut entries = Entries::new();
        entries.insert("provider/a".into(), Zeroizing::new("1".into()));
        let good = encode_entries(&entries);
        assert_eq!(decode_entries(&good).unwrap().len(), 1);
        assert!(decode_entries(&good[..good.len() - 1]).is_none());
        let mut extra = good.to_vec();
        extra.push(0);
        assert!(decode_entries(&extra).is_none());
        assert!(decode_entries(&u32::MAX.to_be_bytes()).is_none());
    }

    #[test]
    fn keys_parse_as_raw_bytes_or_hex() {
        assert_eq!(*parse_key(&KEY).unwrap(), KEY);
        assert_eq!(*parse_key("2a".repeat(32).as_bytes()).unwrap(), KEY);
        assert_eq!(
            *parse_key(format!("{}\r\n", "2a".repeat(32)).as_bytes()).unwrap(),
            KEY
        );
        assert!(parse_key(b"zz").is_none());
        assert!(parse_key("zz".repeat(32).as_bytes()).is_none());
    }

    #[test]
    fn reports_name_each_store() {
        let dir = TempDir::new();
        let path = dir.0.join(ENCRYPTED_FILE);
        let file = |source| {
            let _ = fs::remove_file(&path);
            EncryptedFileStore::open_with_key(path.clone(), source, &KEY)
                .unwrap()
                .report()
        };
        assert_eq!(
            file(KeySource::ContainerSecret { name: "x".into() }).label,
            "Encrypted file, key from a container secret"
        );
        assert_eq!(
            file(KeySource::KeyFile {
                path: "/mnt/key".into()
            })
            .kind,
            "key_file"
        );
        assert_eq!(EncryptedFileStore::locked(path).report().kind, "passphrase");
    }

    #[test]
    fn debug_output_shows_no_secrets() {
        let dir = TempDir::new();
        let store =
            EncryptedFileStore::open_with_key(dir.0.join(ENCRYPTED_FILE), ext(), &KEY).unwrap();
        store.set(&id(), &"sk-test-visible?".into()).unwrap();
        assert!(!format!("{store:?}").contains("sk-test"));
    }
}
