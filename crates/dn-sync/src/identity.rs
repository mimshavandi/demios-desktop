//! This device's long-term identity (ADR-0014 §2): an Ed25519 key pair for signatures and
//! an X25519 key pair for key agreement. Both are created on first start and kept in the
//! device's secret store under `device/identity`. The private keys never leave the device
//! and are never written to SQLite or logs.

use crate::{Result, SyncError, b64};
use aws_lc_rs::agreement::{self, X25519};
use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::encoding::{AsBigEndian, Curve25519SeedBin};
use aws_lc_rs::signature::{self, ED25519, Ed25519KeyPair, KeyPair};
use dn_secrets::{Secret, SecretId, SecretStore};
use std::fmt;
use zeroize::Zeroizing;

/// Secret store id of this device's keys.
pub fn secret_id() -> SecretId {
    SecretId::device("identity")
}

/// Format tag of the stored value, so a later format can be told apart.
const STORED_V1: &str = "v1";

/// A device's id: the first 16 bytes of SHA-256 over its two public keys. Shown in logs
/// and device lists; the keys themselves are what authenticate.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(pub [u8; 16]);

impl DeviceId {
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != 32 || !text.is_ascii() {
            return None;
        }
        let mut out = [0u8; 16];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Self(out))
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({})", self.to_hex())
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A device's public keys, as other devices know them.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicIdentity {
    pub ed25519: [u8; 32],
    pub x25519: [u8; 32],
}

impl PublicIdentity {
    pub fn id(&self) -> DeviceId {
        let mut both = [0u8; 64];
        both[..32].copy_from_slice(&self.ed25519);
        both[32..].copy_from_slice(&self.x25519);
        let hash = digest(&SHA256, &both);
        let mut id = [0u8; 16];
        id.copy_from_slice(&hash.as_ref()[..16]);
        DeviceId(id)
    }

    /// Checks an Ed25519 signature made by this device.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<()> {
        signature::UnparsedPublicKey::new(&ED25519, &self.ed25519)
            .verify(message, signature)
            .map_err(|_| SyncError::BadSignature)
    }

    /// The 64 public-key bytes, Ed25519 first.
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&self.ed25519);
        out[32..].copy_from_slice(&self.x25519);
        out
    }

    pub fn from_bytes(bytes: &[u8; 64]) -> Self {
        let mut ed25519 = [0u8; 32];
        let mut x25519 = [0u8; 32];
        ed25519.copy_from_slice(&bytes[..32]);
        x25519.copy_from_slice(&bytes[32..]);
        Self { ed25519, x25519 }
    }
}

impl fmt::Debug for PublicIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicIdentity({})", self.id())
    }
}

/// This device's key pairs. `Debug` prints the device id only.
pub struct DeviceKeys {
    signing: Ed25519KeyPair,
    agreement: agreement::PrivateKey,
    public: PublicIdentity,
}

impl fmt::Debug for DeviceKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceKeys({})", self.public.id())
    }
}

impl DeviceKeys {
    /// Makes new key pairs from the system random number generator.
    pub fn generate() -> Result<Self> {
        let signing =
            Ed25519KeyPair::generate().map_err(|_| SyncError::Crypto("ed25519 keygen"))?;
        let agreement = agreement::PrivateKey::generate(&X25519)
            .map_err(|_| SyncError::Crypto("x25519 keygen"))?;
        Self::assemble(signing, agreement)
    }

    fn assemble(signing: Ed25519KeyPair, agreement: agreement::PrivateKey) -> Result<Self> {
        let ed25519: [u8; 32] = signing
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| SyncError::Crypto("ed25519 public key"))?;
        let x_public = agreement
            .compute_public_key()
            .map_err(|_| SyncError::Crypto("x25519 public key"))?;
        let x25519: [u8; 32] = x_public
            .as_ref()
            .try_into()
            .map_err(|_| SyncError::Crypto("x25519 public key"))?;
        Ok(Self {
            signing,
            agreement,
            public: PublicIdentity { ed25519, x25519 },
        })
    }

    pub fn public(&self) -> PublicIdentity {
        self.public
    }

    pub fn id(&self) -> DeviceId {
        self.public.id()
    }

    /// Signs `message` with the device's Ed25519 key.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        let sig = self.signing.sign(message);
        let mut out = [0u8; 64];
        out.copy_from_slice(sig.as_ref());
        out
    }

    /// The Ed25519 private key as PKCS#8 v1, for rustls's signing key. Zeroized on drop.
    pub(crate) fn signing_pkcs8(&self) -> Result<Zeroizing<Vec<u8>>> {
        let doc = self
            .signing
            .to_pkcs8v1()
            .map_err(|_| SyncError::Crypto("ed25519 pkcs8"))?;
        Ok(Zeroizing::new(doc.as_ref().to_vec()))
    }

    /// The X25519 private key's bytes, for opening HPKE-wrapped group keys. Zeroized on drop.
    pub(crate) fn agreement_secret(&self) -> Result<Zeroizing<[u8; 32]>> {
        let bin: Curve25519SeedBin<'static> = self
            .agreement
            .as_be_bytes()
            .map_err(|_| SyncError::Crypto("x25519 private key"))?;
        let mut out = Zeroizing::new([0u8; 32]);
        out.copy_from_slice(bin.as_ref());
        Ok(out)
    }

    /// The X25519 private key, for key agreement (HPKE comes with sync).
    pub fn agreement_key(&self) -> &agreement::PrivateKey {
        &self.agreement
    }

    /// The stored form: `v1.<ed25519 seed>.<x25519 private key>` in base64url.
    fn to_stored(&self) -> Result<Zeroizing<String>> {
        let seed = self
            .signing
            .seed()
            .map_err(|_| SyncError::Crypto("ed25519 seed"))?;
        let seed: Curve25519SeedBin<'static> = seed
            .as_be_bytes()
            .map_err(|_| SyncError::Crypto("ed25519 seed"))?;
        let x: Curve25519SeedBin<'static> = self
            .agreement
            .as_be_bytes()
            .map_err(|_| SyncError::Crypto("x25519 private key"))?;
        Ok(Zeroizing::new(format!(
            "{STORED_V1}.{}.{}",
            b64::encode(seed.as_ref()),
            b64::encode(x.as_ref())
        )))
    }

    fn from_stored(text: &str) -> Result<Self> {
        let mut parts = text.split('.');
        let (Some(STORED_V1), Some(seed), Some(x), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(SyncError::DamagedKeys("unknown format"));
        };
        let seed = Zeroizing::new(
            b64::decode_array::<32>(seed).ok_or(SyncError::DamagedKeys("signing key"))?,
        );
        let x = Zeroizing::new(
            b64::decode_array::<32>(x).ok_or(SyncError::DamagedKeys("agreement key"))?,
        );
        let signing = Ed25519KeyPair::from_seed_unchecked(seed.as_ref())
            .map_err(|_| SyncError::DamagedKeys("signing key"))?;
        let agreement = agreement::PrivateKey::from_private_key(&X25519, x.as_ref())
            .map_err(|_| SyncError::DamagedKeys("agreement key"))?;
        Self::assemble(signing, agreement)
    }

    /// Loads this device's keys, or creates and saves them on first start.
    ///
    /// A secret store that fails is an error, never a reason to make new keys: new keys
    /// would be a different device, and every pairing would be lost. Stored keys that
    /// can't be read are reported as damaged and left in place.
    pub fn load_or_create(store: &dyn SecretStore) -> Result<Self> {
        let id = secret_id();
        if let Some(stored) = store.get(&id)? {
            return Self::from_stored(stored.expose());
        }
        let keys = Self::generate()?;
        let stored = keys.to_stored()?;
        store.set(&id, &Secret::new(stored.as_str()))?;
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_secrets::{MemoryStore, SecretError};

    #[test]
    fn keys_are_created_once_and_loaded_after() {
        let store = MemoryStore::new();
        let first = DeviceKeys::load_or_create(&store).unwrap();
        let again = DeviceKeys::load_or_create(&store).unwrap();
        assert_eq!(first.public(), again.public());
        let msg = b"hello";
        again.public().verify(msg, &first.sign(msg)).unwrap();
    }

    #[test]
    fn two_devices_have_different_ids_and_signatures_do_not_cross() {
        let a = DeviceKeys::generate().unwrap();
        let b = DeviceKeys::generate().unwrap();
        assert_ne!(a.id(), b.id());
        let sig = a.sign(b"m");
        assert!(matches!(
            b.public().verify(b"m", &sig),
            Err(SyncError::BadSignature)
        ));
        assert!(matches!(
            a.public().verify(b"other", &sig),
            Err(SyncError::BadSignature)
        ));
    }

    #[test]
    fn damaged_stored_keys_are_reported_and_not_replaced() {
        let store = MemoryStore::with([(secret_id(), Secret::new("v1.not-keys.x"))]);
        assert!(matches!(
            DeviceKeys::load_or_create(&store),
            Err(SyncError::DamagedKeys(_))
        ));
        assert_eq!(
            store.get(&secret_id()).unwrap().unwrap().expose(),
            "v1.not-keys.x"
        );
        let store = MemoryStore::with([(secret_id(), Secret::new("v9.a.b"))]);
        assert!(matches!(
            DeviceKeys::load_or_create(&store),
            Err(SyncError::DamagedKeys("unknown format"))
        ));
    }

    struct Unavailable;
    impl SecretStore for Unavailable {
        fn get(&self, _: &SecretId) -> std::result::Result<Option<Secret>, SecretError> {
            Err(SecretError::Unavailable("no keychain".into()))
        }
        fn set(&self, _: &SecretId, _: &Secret) -> std::result::Result<(), SecretError> {
            panic!("must not write when reading failed")
        }
        fn delete(&self, _: &SecretId) -> std::result::Result<bool, SecretError> {
            Ok(false)
        }
    }

    #[test]
    fn an_unreadable_secret_store_is_an_error_not_a_new_identity() {
        assert!(matches!(
            DeviceKeys::load_or_create(&Unavailable),
            Err(SyncError::Secrets(SecretError::Unavailable(_)))
        ));
    }

    #[test]
    fn device_id_hex_round_trips_and_is_strict() {
        let id = DeviceKeys::generate().unwrap().id();
        assert_eq!(DeviceId::from_hex(&id.to_hex()), Some(id));
        assert_eq!(DeviceId::from_hex("zz"), None);
        assert_eq!(DeviceId::from_hex(&"é".repeat(16)), None);
    }

    #[test]
    fn debug_output_never_shows_key_material() {
        let keys = DeviceKeys::generate().unwrap();
        let shown = format!("{keys:?}");
        assert_eq!(shown, format!("DeviceKeys({})", keys.id()));
        let stored = keys.to_stored().unwrap();
        let seed_part = stored.split('.').nth(1).unwrap();
        assert!(!shown.contains(seed_part));
    }
}
