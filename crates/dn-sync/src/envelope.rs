//! Envelopes: a change, signed by its author and sealed with the group key
//! (docs/design/dn-sync.md §7.2).
//!
//! 1. The author signs the change bytes with its Ed25519 device key.
//! 2. `signature ‖ change` is sealed with the key of the change's epoch:
//!    - a random 32-byte salt;
//!    - `HKDF-SHA256(group key, salt, "dn-sync v1 envelope")` gives a 32-byte key and a
//!      12-byte nonce, used once;
//!    - AES-256-GCM, with `"dn-sync v1" ‖ group ‖ epoch` as associated data.
//! 3. The envelope is `version ‖ group ‖ epoch ‖ salt ‖ ciphertext`. Only the group and the
//!    epoch show outside the seal.
//!
//! [`open`] checks the seal, then the signature against the author's key as the caller
//! knows it, then that the change names the same group and epoch.

use crate::change::{CHANGE_VERSION, Change, GroupId, MAX_CHANGE_BYTES};
use crate::identity::{DeviceId, DeviceKeys, PublicIdentity};
use crate::{Result, SyncError};
use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::hkdf;
use zeroize::Zeroizing;

const HKDF_INFO: &[u8] = b"dn-sync v1 envelope";
const AAD_LABEL: &[u8] = b"dn-sync v1";
const SALT_LEN: usize = 32;
const SIG_LEN: usize = 64;
const HEADER_LEN: usize = 1 + 16 + 4 + SALT_LEN;
/// Largest envelope accepted, in bytes.
pub const MAX_ENVELOPE_BYTES: usize = HEADER_LEN + SIG_LEN + MAX_CHANGE_BYTES + 16;

/// A group key of one epoch. `Debug` shows only the epoch.
pub struct GroupKey {
    pub epoch: u32,
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for GroupKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GroupKey(epoch {})", self.epoch)
    }
}

impl GroupKey {
    /// A new random key (the Node makes one at `Create` and on every rotation, §8.3).
    pub fn generate(epoch: u32) -> Result<Self> {
        let mut key = Zeroizing::new([0u8; 32]);
        aws_lc_rs::rand::fill(key.as_mut()).map_err(|_| SyncError::Crypto("random"))?;
        Ok(Self { epoch, key })
    }

    /// The key's bytes, to wrap it for a device or keep it in the secret store.
    pub(crate) fn bytes(&self) -> &[u8; 32] {
        &self.key
    }

    pub fn from_bytes(epoch: u32, bytes: &[u8; 32]) -> Self {
        Self {
            epoch,
            key: Zeroizing::new(*bytes),
        }
    }

    fn message_key(&self, salt: &[u8; SALT_LEN]) -> Result<(LessSafeKey, [u8; NONCE_LEN])> {
        self.derive(salt, HKDF_INFO)
    }

    fn derive(&self, salt: &[u8; SALT_LEN], info: &[u8]) -> Result<(LessSafeKey, [u8; NONCE_LEN])> {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(self.key.as_ref());
        let info = [info];
        let okm = prk
            .expand(&info, Len44)
            .map_err(|_| SyncError::Crypto("hkdf"))?;
        let mut out = Zeroizing::new([0u8; 44]);
        okm.fill(out.as_mut())
            .map_err(|_| SyncError::Crypto("hkdf"))?;
        let key =
            UnboundKey::new(&AES_256_GCM, &out[..32]).map_err(|_| SyncError::Crypto("aead key"))?;
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&out[32..44]);
        Ok((LessSafeKey::new(key), nonce))
    }
}

/// HKDF output length for a 32-byte key and a 12-byte nonce.
struct Len44;
impl hkdf::KeyType for Len44 {
    fn len(&self) -> usize {
        44
    }
}

fn aad(group: &GroupId, epoch: u32) -> Vec<u8> {
    let mut a = AAD_LABEL.to_vec();
    a.extend_from_slice(&group.0);
    a.extend_from_slice(&epoch.to_be_bytes());
    a
}

/// What only shows outside the seal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub group: GroupId,
    pub epoch: u32,
}

/// Reads the outside of an envelope, to pick the key.
pub fn header(envelope: &[u8]) -> Result<Header> {
    if envelope.len() < HEADER_LEN || envelope[0] != CHANGE_VERSION as u8 {
        return Err(SyncError::Protocol("not a sync envelope"));
    }
    let mut group = [0u8; 16];
    group.copy_from_slice(&envelope[1..17]);
    let epoch = u32::from_be_bytes([envelope[17], envelope[18], envelope[19], envelope[20]]);
    Ok(Header {
        group: GroupId(group),
        epoch,
    })
}

/// Signs `change` with the author's keys and seals it with `key`. The change must be the
/// author's, of the key's epoch.
pub fn seal(change: &Change, author: &DeviceKeys, key: &GroupKey) -> Result<Vec<u8>> {
    if change.author != author.id() || change.epoch != key.epoch {
        return Err(SyncError::Protocol(
            "a change is sealed by its author, with its epoch's key",
        ));
    }
    let bytes = change.to_bytes()?;
    let mut plain = Zeroizing::new(Vec::with_capacity(SIG_LEN + bytes.len()));
    plain.extend_from_slice(&author.sign(&bytes));
    plain.extend_from_slice(&bytes);
    let mut salt = [0u8; SALT_LEN];
    aws_lc_rs::rand::fill(&mut salt).map_err(|_| SyncError::Crypto("random"))?;
    let (aead, nonce) = key.message_key(&salt)?;
    let mut sealed = plain.to_vec();
    aead.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(aad(&change.group, change.epoch)),
        &mut sealed,
    )
    .map_err(|_| SyncError::Crypto("seal"))?;
    let mut out = Vec::with_capacity(HEADER_LEN + sealed.len());
    out.push(CHANGE_VERSION as u8);
    out.extend_from_slice(&change.group.0);
    out.extend_from_slice(&change.epoch.to_be_bytes());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Signs `body` with `signer` and seals `signature ‖ body` with `key`, like an envelope but
/// with its own HKDF label and associated data (`label`), so a sealed blob of one kind can
/// never be opened as another (used for snapshots).
pub(crate) fn seal_blob(
    label: &[u8],
    group: &GroupId,
    body: &[u8],
    signer: &DeviceKeys,
    key: &GroupKey,
) -> Result<Vec<u8>> {
    let mut plain = Zeroizing::new(Vec::with_capacity(SIG_LEN + body.len()));
    plain.extend_from_slice(&signer.sign(body));
    plain.extend_from_slice(body);
    let mut salt = [0u8; SALT_LEN];
    aws_lc_rs::rand::fill(&mut salt).map_err(|_| SyncError::Crypto("random"))?;
    let (aead, nonce) = key.derive(&salt, label)?;
    let mut sealed = plain.to_vec();
    aead.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(blob_aad(label, group, key.epoch)),
        &mut sealed,
    )
    .map_err(|_| SyncError::Crypto("seal"))?;
    let mut out = Vec::with_capacity(HEADER_LEN + sealed.len());
    out.push(CHANGE_VERSION as u8);
    out.extend_from_slice(&group.0);
    out.extend_from_slice(&key.epoch.to_be_bytes());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn blob_aad(label: &[u8], group: &GroupId, epoch: u32) -> Vec<u8> {
    let mut a = label.to_vec();
    a.extend_from_slice(&group.0);
    a.extend_from_slice(&epoch.to_be_bytes());
    a
}

/// Opens a blob sealed with [`seal_blob`] and the same `label`, and checks its signature
/// against `signer`. Returns the body.
pub(crate) fn open_blob(
    label: &[u8],
    sealed: &[u8],
    key: &GroupKey,
    signer: &PublicIdentity,
) -> Result<Vec<u8>> {
    let head = header(sealed)?;
    if head.epoch != key.epoch {
        return Err(SyncError::Protocol("sealed with another epoch's key"));
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&sealed[21..HEADER_LEN]);
    let (aead, nonce) = key.derive(&salt, label)?;
    let mut buf = Zeroizing::new(sealed[HEADER_LEN..].to_vec());
    let plain = aead
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(blob_aad(label, &head.group, head.epoch)),
            &mut buf,
        )
        .map_err(|_| SyncError::Protocol("it doesn't open with the group key"))?;
    if plain.len() < SIG_LEN {
        return Err(SyncError::Protocol("too short"));
    }
    let (signature, body) = plain.split_at(SIG_LEN);
    signer.verify(body, signature)?;
    Ok(body.to_vec())
}

/// An opened envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct Opened {
    pub change: Change,
    /// SHA-256 of the signed change bytes: the identity of the change. It ignores the seal
    /// (salt, ciphertext), so the same change sealed twice, by its author or re-sealed by
    /// another member, is one change, and only a different signed change under one
    /// (author, seq) is equivocation.
    pub hash: [u8; 32],
    /// The envelope as received, kept in the inbox.
    pub envelope: Vec<u8>,
}

/// SHA-256 of a change's signed bytes ([`Opened::hash`]).
pub fn change_hash(bytes: &[u8]) -> [u8; 32] {
    let mut h = [0u8; 32];
    h.copy_from_slice(digest(&SHA256, bytes).as_ref());
    h
}

/// SHA-256 of an envelope.
pub fn envelope_hash(envelope: &[u8]) -> [u8; 32] {
    let mut h = [0u8; 32];
    h.copy_from_slice(digest(&SHA256, envelope).as_ref());
    h
}

/// Opens an envelope with `key`, and checks the signature against the author's public keys,
/// which `author_keys` looks up by author and epoch (a member as membership stood at that
/// epoch, §8). Any failure refuses the envelope.
pub fn open(
    envelope: &[u8],
    key: &GroupKey,
    author_keys: impl FnOnce(DeviceId, u32) -> Result<PublicIdentity>,
) -> Result<Opened> {
    if envelope.len() > MAX_ENVELOPE_BYTES {
        return Err(SyncError::Protocol("envelope too large"));
    }
    let head = header(envelope)?;
    if head.epoch != key.epoch {
        return Err(SyncError::Protocol(
            "envelope sealed with another epoch's key",
        ));
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&envelope[21..HEADER_LEN]);
    let (aead, nonce) = key.message_key(&salt)?;
    let mut sealed = Zeroizing::new(envelope[HEADER_LEN..].to_vec());
    let plain = aead
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad(&head.group, head.epoch)),
            &mut sealed,
        )
        .map_err(|_| SyncError::Protocol("the envelope doesn't open with the group key"))?;
    if plain.len() < SIG_LEN {
        return Err(SyncError::Protocol("envelope too short"));
    }
    let (signature, bytes) = plain.split_at(SIG_LEN);
    let change = Change::from_bytes(bytes)?;
    if change.group != head.group || change.epoch != head.epoch {
        return Err(SyncError::Protocol(
            "the change names another group or epoch",
        ));
    }
    author_keys(change.author, change.epoch)?.verify(bytes, signature)?;
    Ok(Opened {
        change,
        hash: change_hash(bytes),
        envelope: envelope.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::{Op, Vector};
    use crate::hlc::Hlc;
    use std::collections::BTreeMap;

    fn change(author: &DeviceKeys, epoch: u32) -> Change {
        let mut deps = Vector::default();
        deps.set(author.id(), 0);
        Change {
            group: GroupId([3; 16]),
            epoch,
            author: author.id(),
            seq: 1,
            deps,
            hlc: Hlc::from_parts(1_000, 0),
            ops: vec![Op::Put {
                collection: "chat".into(),
                id: "c1".into(),
                fields: BTreeMap::from([("title".into(), serde_json::json!("A private title"))]),
            }],
        }
    }

    #[test]
    fn a_sealed_change_opens_with_the_key_and_shows_nothing_outside() {
        let me = DeviceKeys::generate().unwrap();
        let key = GroupKey::generate(0).unwrap();
        let c = change(&me, 0);
        let env = seal(&c, &me, &key).unwrap();
        assert!(!env.windows(7).any(|w| w == b"private"));
        assert!(!env.windows(16).any(|w| w == me.id().0));
        assert_eq!(
            header(&env).unwrap(),
            Header {
                group: GroupId([3; 16]),
                epoch: 0
            }
        );
        let opened = open(&env, &key, |_, _| Ok(me.public())).unwrap();
        assert_eq!(opened.change, c);
        // Two seals of one change differ (fresh salt).
        assert_ne!(seal(&c, &me, &key).unwrap(), env);
    }

    #[test]
    fn a_tampered_envelope_or_another_key_is_refused() {
        let me = DeviceKeys::generate().unwrap();
        let key = GroupKey::generate(0).unwrap();
        let env = seal(&change(&me, 0), &me, &key).unwrap();
        for i in [0, 5, 18, 30, HEADER_LEN + 3, env.len() - 1] {
            let mut bad = env.clone();
            bad[i] ^= 1;
            assert!(
                open(&bad, &key, |_, _| Ok(me.public())).is_err(),
                "byte {i}"
            );
        }
        let other = GroupKey::generate(0).unwrap();
        assert!(open(&env, &other, |_, _| Ok(me.public())).is_err());
        let next_epoch = GroupKey::from_bytes(1, &[0; 32]);
        assert!(open(&env, &next_epoch, |_, _| Ok(me.public())).is_err());
    }

    #[test]
    fn a_member_with_the_group_key_still_cant_sign_for_another() {
        let me = DeviceKeys::generate().unwrap();
        let other = DeviceKeys::generate().unwrap();
        let key = GroupKey::generate(0).unwrap();
        // Sealed by `me` but checked against `other`'s key (as if `me` claimed to be `other`).
        let env = seal(&change(&me, 0), &me, &key).unwrap();
        assert!(matches!(
            open(&env, &key, |_, _| Ok(other.public())),
            Err(SyncError::BadSignature)
        ));
        // And a non-member is refused by the lookup.
        assert!(open(&env, &key, |_, _| Err(SyncError::UnknownDevice)).is_err());
    }

    #[test]
    fn only_the_author_seals_and_only_with_its_epoch() {
        let me = DeviceKeys::generate().unwrap();
        let other = DeviceKeys::generate().unwrap();
        let key = GroupKey::generate(1).unwrap();
        assert!(seal(&change(&me, 1), &other, &key).is_err());
        assert!(seal(&change(&me, 0), &me, &key).is_err());
    }

    #[test]
    fn the_same_change_sealed_twice_is_one_change() {
        let me = DeviceKeys::generate().unwrap();
        let key = GroupKey::generate(0).unwrap();
        let c = change(&me, 0);
        let a = open(&seal(&c, &me, &key).unwrap(), &key, |_, _| Ok(me.public())).unwrap();
        let b = open(&seal(&c, &me, &key).unwrap(), &key, |_, _| Ok(me.public())).unwrap();
        assert_ne!(a.envelope, b.envelope);
        assert_eq!(a.hash, b.hash);
    }

    #[test]
    fn debug_hides_the_key() {
        assert_eq!(
            format!("{:?}", GroupKey::from_bytes(4, &[9; 32])),
            "GroupKey(epoch 4)"
        );
    }
}
