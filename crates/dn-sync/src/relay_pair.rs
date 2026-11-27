//! Pairing through the Relay, for a Node that shares no network with the device (a VPS;
//! ADR-0014 §4a, docs/design/dn-sync.md §10.4).
//!
//! 1. The Node makes a [`RelayOffer`]: a pairing offer whose link carries the Relay's URL and
//!    an **open, one-message pairing mailbox** there, owned by a fresh key for this offer.
//! 2. The new device makes its own one-message **reply mailbox**, also with a fresh key, and
//!    sends a [`join_through_relay`] request to the pairing mailbox:
//!    - its public keys, name, kind, the reply mailbox and the proof that it has the code;
//!    - **signed** with its Ed25519 key, which stands in for the TLS handshake's proof that
//!      it holds the key;
//!    - **sealed** with HPKE to the Node's X25519 key from the link, with the pairing
//!      mailbox in the HPKE `info`, so the Relay can't read or change it.
//! 3. The proof is the same HMAC as on the direct route, with the TLS channel binding
//!    replaced by [`relay_binding`]: the Node's keys and the pairing mailbox under a
//!    Relay-only label. A proof made for one route or mailbox never checks on another, the
//!    mailbox takes one message, and the offer closes after it, so it can't be replayed.
//! 4. The Node fetches it ([`RelayOffer::read_join`]), checks the signature, the device
//!    list and the proof, and shows the [`CheckNumbers`]. One message closes the mailbox and
//!    the offer, whatever it held.
//! 5. After the person answers, [`RelayOffer::finish_join`] adds the device on yes and, either
//!    way, delivers an answer **signed** by the Node and **sealed** to the device, naming the
//!    request's hash. The device trusts it only under the Node's key from the link
//!    ([`RelayJoinPending::answer`]).
//!
//! The Relay is behind [`PairingRelay`]. Its HTTP client comes with the Relay client
//! (ADR-0016 action item 2); until then nothing outside the tests runs this.

use crate::devices::{Device, DeviceKind, DeviceList};
use crate::identity::{DeviceId, DeviceKeys, PublicIdentity};
use crate::pairing::{
    CheckNumbers, MailboxId, OFFER_TTL_SECS, PairingLink, PairingOffer, RelayRoute, join_proof,
};
use crate::transport::{JoinRequest, kind_name};
use crate::{Result, SyncError, b64};
use aws_lc_rs::digest::{SHA256, digest};
use hpke::aead::AesGcm256;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable, OpModeR, OpModeS, Serializable};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::Mutex;
use zeroize::Zeroizing;

type Kem = X25519HkdfSha256;

const BINDING_LABEL: &[u8] = b"dn-pair v2 relay binding";
const REQUEST_INFO: &[u8] = b"dn-pair v2 relay request";
const REQUEST_SIG: &[u8] = b"dn-pair v2 relay request sig";
const ANSWER_INFO: &[u8] = b"dn-pair v2 relay answer";
const ANSWER_SIG: &[u8] = b"dn-pair v2 relay answer sig";
/// Largest sealed pairing message opened, in bytes. A request is well under 2 KiB.
pub const MAX_MESSAGE: usize = 16 * 1024;
/// Every pairing message is padded to this size before sealing, so its size on the Relay
/// says nothing about the device's name (ADR-0016 §3).
const PADDED: usize = 4096;

/// `plain` behind a 2-byte length, padded with zeros to [`PADDED`].
fn pad(plain: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if plain.len() > PADDED - 2 {
        return Err(SyncError::Protocol("the pairing message is too large"));
    }
    let mut out = Zeroizing::new(Vec::with_capacity(PADDED));
    out.extend_from_slice(&(plain.len() as u16).to_be_bytes());
    out.extend_from_slice(plain);
    out.resize(PADDED, 0);
    Ok(out)
}

fn unpad(padded: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let bad = SyncError::Protocol("the pairing message is damaged");
    if padded.len() != PADDED {
        return Err(bad);
    }
    let len = u16::from_be_bytes([padded[0], padded[1]]) as usize;
    if len > PADDED - 2 || padded[2 + len..].iter().any(|&b| b != 0) {
        return Err(bad);
    }
    Ok(Zeroizing::new(padded[2..2 + len].to_vec()))
}

/// The Relay as pairing uses it: open mailboxes that take **one** message and then close
/// (ADR-0016 §1, §4.4). `owner` is the mailbox's key pair; the client signs owner requests
/// with it. A real client long-polls in [`fetch`](Self::fetch); `None` means nothing came
/// within its wait.
pub trait PairingRelay {
    /// Creates the open, one-message mailbox of `owner`, deleted at `expires_at` (Unix s).
    fn create(
        &self,
        owner: &DeviceKeys,
        expires_at: u64,
    ) -> impl Future<Output = Result<()>> + Send;
    /// Delivers `sealed` to an open mailbox. Refused once it holds its message or is closed.
    fn deliver(&self, mailbox: MailboxId, sealed: &[u8])
    -> impl Future<Output = Result<()>> + Send;
    /// The mailbox's message, if one came. Doesn't delete it; [`close`](Self::close) does.
    fn fetch(&self, owner: &DeviceKeys) -> impl Future<Output = Result<Option<Vec<u8>>>> + Send;
    /// Closes the mailbox and deletes what it holds.
    fn close(&self, owner: &DeviceKeys) -> impl Future<Output = Result<()>> + Send;
}

/// The value the proof is bound to on the Relay route, in place of the TLS exporter value:
/// the Node's keys and the pairing mailbox, under a label the TLS route never uses.
pub fn relay_binding(node: &PublicIdentity, mailbox: &MailboxId) -> [u8; 32] {
    let mut input = Vec::with_capacity(BINDING_LABEL.len() + 96);
    input.extend_from_slice(BINDING_LABEL);
    input.extend_from_slice(&node.to_bytes());
    input.extend_from_slice(&mailbox.0);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest(&SHA256, &input).as_ref());
    out
}

fn with_mailbox(label: &[u8], mailbox: &MailboxId) -> Vec<u8> {
    let mut v = Vec::with_capacity(label.len() + 32);
    v.extend_from_slice(label);
    v.extend_from_slice(&mailbox.0);
    v
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(digest(&SHA256, bytes).as_ref());
    out
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sealed {
    enc: String,
    ciphertext: String,
}

fn seal(to: &[u8; 32], mailbox: &MailboxId, info: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
    let pk = <Kem as hpke::Kem>::PublicKey::from_bytes(to)
        .map_err(|_| SyncError::Crypto("hpke public key"))?;
    let (enc, ct) = hpke::single_shot_seal::<AesGcm256, HkdfSha256, Kem>(
        &OpModeS::Base,
        &pk,
        &with_mailbox(info, mailbox),
        &pad(plain)?,
        &mailbox.0,
    )
    .map_err(|_| SyncError::Crypto("hpke seal"))?;
    Ok(serde_json::to_vec(&Sealed {
        enc: b64::encode(&enc.to_bytes()),
        ciphertext: b64::encode(&ct),
    })
    .expect("plain struct serializes"))
}

fn open(
    me: &DeviceKeys,
    mailbox: &MailboxId,
    info: &[u8],
    sealed: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    if sealed.len() > MAX_MESSAGE {
        return Err(SyncError::Protocol("the pairing message is too large"));
    }
    let bad = SyncError::Protocol("the pairing message doesn't open");
    let s: Sealed = serde_json::from_slice(sealed).map_err(|_| bad)?;
    let secret = me.agreement_secret()?;
    let sk = <Kem as hpke::Kem>::PrivateKey::from_bytes(secret.as_ref())
        .map_err(|_| SyncError::Crypto("hpke private key"))?;
    let enc = <Kem as hpke::Kem>::EncappedKey::from_bytes(
        &b64::decode(&s.enc).ok_or(SyncError::Protocol("the pairing message doesn't open"))?,
    )
    .map_err(|_| SyncError::Protocol("the pairing message doesn't open"))?;
    let ct = b64::decode(&s.ciphertext)
        .ok_or(SyncError::Protocol("the pairing message doesn't open"))?;
    hpke::single_shot_open::<AesGcm256, HkdfSha256, Kem>(
        &OpModeR::Base,
        &sk,
        &enc,
        &with_mailbox(info, mailbox),
        &ct,
        &mailbox.0,
    )
    .map(Zeroizing::new)
    .map_err(|_| SyncError::Protocol("the pairing message doesn't open"))
    .and_then(|padded| unpad(&padded))
}

/// What the joining device signs. The keys are base64 of 32 bytes each.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestBody {
    /// The pairing mailbox this request is for.
    mailbox: String,
    ed25519: String,
    x25519: String,
    name: String,
    kind: String,
    reply: String,
    proof: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Signed {
    /// base64 of the JSON body's exact bytes.
    body: String,
    signature: String,
}

/// What the Node signs back.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerBody {
    /// The reply mailbox this answer is for.
    mailbox: String,
    /// SHA-256 of the signed request it answers.
    request: String,
    accepted: bool,
}

fn signed_bytes(label: &[u8], mailbox: &MailboxId, body: &[u8]) -> Vec<u8> {
    let mut v = with_mailbox(label, mailbox);
    v.extend_from_slice(body);
    v
}

fn sign(keys: &DeviceKeys, label: &[u8], mailbox: &MailboxId, body: &[u8]) -> Vec<u8> {
    let sig = keys.sign(&signed_bytes(label, mailbox, body));
    serde_json::to_vec(&Signed {
        body: b64::encode(body),
        signature: b64::encode(&sig),
    })
    .expect("plain struct serializes")
}

/// Splits a [`Signed`] message into its body bytes and signature.
fn unpack(bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let bad = SyncError::Protocol("the pairing message is damaged");
    let s: Signed = serde_json::from_slice(bytes).map_err(|_| bad)?;
    let body = b64::decode(&s.body).ok_or(SyncError::Protocol("the pairing message is damaged"))?;
    let sig =
        b64::decode(&s.signature).ok_or(SyncError::Protocol("the pairing message is damaged"))?;
    Ok((body, sig))
}

fn key32(text: &str) -> Result<[u8; 32]> {
    b64::decode_array::<32>(text).ok_or(SyncError::Protocol("bad key"))
}

/// What the person answered to the check numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersonAnswer {
    /// They match. `by` is the paired device the person confirmed on; `None` on the Node.
    Confirmed {
        by: Option<DeviceId>,
    },
    Declined,
}

/// The offer as the Node keeps it while it waits for a request through the Relay.
pub struct RelayOffer {
    offer: PairingOffer,
    mailbox: DeviceKeys,
}

impl std::fmt::Debug for RelayOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayOffer")
            .field("offer", &self.offer)
            .finish_non_exhaustive()
    }
}

/// A request read from the pairing mailbox: what the person confirms, and where the
/// answer goes.
#[derive(Debug)]
pub struct RelayJoinRequest {
    pub request: JoinRequest,
    reply: MailboxId,
    request_hash: [u8; 32],
}

impl RelayOffer {
    /// Makes an offer whose link carries `relay_url` and a fresh one-message pairing
    /// mailbox, and creates that mailbox on the Relay, open for the offer's 10 minutes.
    /// `address` is the Node's direct address, if devices can also reach it directly.
    pub async fn open(
        relay: &impl PairingRelay,
        relay_url: &str,
        address: Option<&str>,
        node: PublicIdentity,
        now: u64,
    ) -> Result<Self> {
        let mailbox = DeviceKeys::generate()?;
        let route = RelayRoute {
            url: relay_url.to_owned(),
            mailbox: MailboxId::of(&mailbox.public().x25519),
        };
        let offer = PairingOffer::with_route(address, Some(route), node, now)?;
        relay.create(&mailbox, now + OFFER_TTL_SECS).await?;
        Ok(Self { offer, mailbox })
    }

    pub fn link(&self) -> &PairingLink {
        self.offer.link()
    }

    pub fn offer(&self) -> &PairingOffer {
        &self.offer
    }

    fn mailbox_id(&self) -> MailboxId {
        MailboxId::of(&self.mailbox.public().x25519)
    }

    /// Fetches the pairing mailbox. `Ok(None)` while nothing has come (call again until
    /// the code expires). A message closes the mailbox and the offer, whether or not it
    /// checks: the mailbox takes one message (ADR-0014 §4a). The database is locked only
    /// for the device-list lookup, never across a network wait.
    pub async fn read_join(
        &mut self,
        relay: &impl PairingRelay,
        db: &Mutex<Connection>,
        node: &DeviceKeys,
        now: impl Fn() -> u64,
    ) -> Result<Option<RelayJoinRequest>> {
        if self.offer.seconds_left(now()) == 0 {
            self.offer.close();
            relay.close(&self.mailbox).await?;
            return Err(SyncError::OfferClosed);
        }
        let Some(sealed) = relay.fetch(&self.mailbox).await? else {
            return Ok(None);
        };
        // The time the request arrived, not when the Node started waiting for it.
        let at = now();
        let closed = relay.close(&self.mailbox).await;
        let result = self.check(db, node, &sealed, at);
        if result.is_err() {
            self.offer.close();
        }
        closed?;
        result.map(Some)
    }

    fn check(
        &mut self,
        db: &Mutex<Connection>,
        node: &DeviceKeys,
        sealed: &[u8],
        now: u64,
    ) -> Result<RelayJoinRequest> {
        let mailbox = self.mailbox_id();
        if node.public() != self.offer.link().node {
            return Err(SyncError::NodeKeyMismatch);
        }
        let signed = open(node, &mailbox, REQUEST_INFO, sealed)?;
        let (body_bytes, sig) = unpack(&signed)?;
        let body: RequestBody = serde_json::from_slice(&body_bytes)
            .map_err(|_| SyncError::Protocol("the pairing request is damaged"))?;
        if MailboxId::from_b64(&body.mailbox) != Some(mailbox) {
            return Err(SyncError::Protocol("the request is for another pairing"));
        }
        let identity = PublicIdentity {
            ed25519: key32(&body.ed25519)?,
            x25519: key32(&body.x25519)?,
        };
        // Proof of the signing key: what TLS checks on the direct route.
        identity
            .verify(&signed_bytes(REQUEST_SIG, &mailbox, &body_bytes), &sig)
            .map_err(|_| SyncError::BadSignature)?;
        let existing = DeviceList::by_signing_key(&lock_db(db), &identity.ed25519)?;
        if let Some(existing) = existing {
            return Err(if existing.is_member() {
                SyncError::AlreadyPaired
            } else {
                SyncError::Removed
            });
        }
        let kind = match body.kind.as_str() {
            "desktop" => DeviceKind::Desktop,
            "phone" => DeviceKind::Phone,
            _ => return Err(SyncError::Protocol("bad device kind")),
        };
        let reply = MailboxId::from_b64(&body.reply).ok_or(SyncError::Protocol("bad mailbox"))?;
        let proof = b64::decode_array::<32>(&body.proof).ok_or(SyncError::Protocol("bad proof"))?;
        // The name is shown to the person before the device is added: same rules as the list.
        let name = crate::devices::clean_name(&body.name)?;
        let numbers = self.offer.accept(
            &identity,
            &proof,
            &relay_binding(&self.offer.link().node, &mailbox),
            now,
        )?;
        Ok(RelayJoinRequest {
            request: JoinRequest {
                identity,
                name,
                kind,
                numbers,
            },
            reply,
            request_hash: hash(&signed),
        })
    }

    /// After the person answered: adds the device if they confirmed the numbers, and either
    /// way delivers the Node's signed answer to the device's reply mailbox.
    pub async fn finish_join(
        &self,
        relay: &impl PairingRelay,
        db: &Mutex<Connection>,
        node: &DeviceKeys,
        join: &RelayJoinRequest,
        answer: PersonAnswer,
        now_ms: i64,
    ) -> Result<Option<Device>> {
        let added = match answer {
            PersonAnswer::Confirmed { by } => {
                let r = &join.request;
                Some(DeviceList::add(
                    &lock_db(db),
                    &r.identity,
                    &r.name,
                    r.kind,
                    by,
                    now_ms,
                ))
            }
            PersonAnswer::Declined => None,
        };
        let accepted = matches!(added, Some(Ok(_)));
        let body = serde_json::to_vec(&AnswerBody {
            mailbox: join.reply.to_b64(),
            request: b64::encode(&join.request_hash),
            accepted,
        })
        .expect("plain struct serializes");
        let signed = sign(node, ANSWER_SIG, &join.reply, &body);
        let sealed = seal(
            &join.request.identity.x25519,
            &join.reply,
            ANSWER_INFO,
            &signed,
        )?;
        relay.deliver(join.reply, &sealed).await?;
        added.transpose()
    }
}

fn lock_db(db: &Mutex<Connection>) -> std::sync::MutexGuard<'_, Connection> {
    db.lock().unwrap_or_else(|p| p.into_inner())
}

/// The joining device's side after it sent its request: show the numbers, then wait for
/// the Node's answer in the reply mailbox.
pub struct RelayJoinPending {
    pub numbers: CheckNumbers,
    node: PublicIdentity,
    reply: DeviceKeys,
    request_hash: [u8; 32],
}

impl std::fmt::Debug for RelayJoinPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayJoinPending")
            .field("numbers", &self.numbers)
            .finish_non_exhaustive()
    }
}

/// Pairs this device with the Node in `link` through its Relay: makes a reply mailbox,
/// and delivers the signed, sealed request to the link's pairing mailbox.
pub async fn join_through_relay(
    relay: &impl PairingRelay,
    keys: &DeviceKeys,
    link: &PairingLink,
    name: &str,
    kind: DeviceKind,
    now: u64,
) -> Result<RelayJoinPending> {
    if kind == DeviceKind::Node {
        return Err(SyncError::Protocol("a Node doesn't join another Node"));
    }
    let route = link
        .relay
        .as_ref()
        .ok_or(SyncError::BadLink("it has no Relay"))?;
    if now >= link.expires_at {
        return Err(SyncError::OfferClosed);
    }
    let reply = DeviceKeys::generate()?;
    let reply_id = MailboxId::of(&reply.public().x25519);
    relay.create(&reply, link.expires_at).await?;
    let me = keys.public();
    let proof = join_proof(link, &me, &relay_binding(&link.node, &route.mailbox));
    let body = serde_json::to_vec(&RequestBody {
        mailbox: route.mailbox.to_b64(),
        ed25519: b64::encode(&me.ed25519),
        x25519: b64::encode(&me.x25519),
        name: name.to_owned(),
        kind: kind_name(kind).to_owned(),
        reply: reply_id.to_b64(),
        proof: b64::encode(&proof),
    })
    .expect("plain struct serializes");
    let signed = sign(keys, REQUEST_SIG, &route.mailbox, &body);
    let sealed = seal(&link.node.x25519, &route.mailbox, REQUEST_INFO, &signed)?;
    if let Err(e) = relay.deliver(route.mailbox, &sealed).await {
        let _ = relay.close(&reply).await;
        return Err(e);
    }
    Ok(RelayJoinPending {
        numbers: CheckNumbers::new(&link.node, &me),
        node: link.node,
        reply,
        request_hash: hash(&signed),
    })
}

impl RelayJoinPending {
    /// Fetches the reply mailbox once. `Ok(None)` while the Node hasn't answered (the
    /// person may be reading the numbers; the code's expiry still applies on the Node).
    /// `Ok(Some(()))`: the Node added this device. A refusal is [`SyncError::Declined`].
    /// The answer counts only if the Node's key from the link signed it, for this request.
    pub async fn answer(&self, relay: &impl PairingRelay, keys: &DeviceKeys) -> Result<Option<()>> {
        let Some(sealed) = relay.fetch(&self.reply).await? else {
            return Ok(None);
        };
        let closed = relay.close(&self.reply).await;
        let result = self.check(keys, &sealed);
        closed?;
        result.map(Some)
    }

    fn check(&self, keys: &DeviceKeys, sealed: &[u8]) -> Result<()> {
        let mailbox = MailboxId::of(&self.reply.public().x25519);
        let signed = open(keys, &mailbox, ANSWER_INFO, sealed)?;
        let (body_bytes, sig) = unpack(&signed)?;
        self.node
            .verify(&signed_bytes(ANSWER_SIG, &mailbox, &body_bytes), &sig)
            .map_err(|_| SyncError::NodeKeyMismatch)?;
        let body: AnswerBody = serde_json::from_slice(&body_bytes)
            .map_err(|_| SyncError::Protocol("the Node's answer is damaged"))?;
        if MailboxId::from_b64(&body.mailbox) != Some(mailbox)
            || b64::decode_array::<32>(&body.request) != Some(self.request_hash)
        {
            return Err(SyncError::Protocol("the answer is for another pairing"));
        }
        if body.accepted {
            Ok(())
        } else {
            Err(SyncError::Declined)
        }
    }

    /// Stops waiting (the numbers didn't match): closes the reply mailbox.
    pub async fn cancel(self, relay: &impl PairingRelay) -> Result<()> {
        relay.close(&self.reply).await
    }
}

/// An in-memory Relay with the rules pairing relies on (ADR-0016 §1, §4.4, §6): one
/// message per mailbox, refused after that or once closed or expired; only the owner
/// fetches and closes. For tests.
#[cfg(test)]
pub(crate) mod memory {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Mailbox {
        expires_at: u64,
        message: Option<Vec<u8>>,
        closed: bool,
    }

    #[derive(Default)]
    pub struct MemoryRelay {
        boxes: Mutex<HashMap<MailboxId, Mailbox>>,
        pub now: std::sync::atomic::AtomicU64,
        /// Every message delivered, as the operator sees it.
        pub seen: Mutex<Vec<Vec<u8>>>,
    }

    impl MemoryRelay {
        pub fn at(now: u64) -> Self {
            let r = Self::default();
            r.now.store(now, std::sync::atomic::Ordering::SeqCst);
            r
        }

        fn now(&self) -> u64 {
            self.now.load(std::sync::atomic::Ordering::SeqCst)
        }

        pub fn is_closed(&self, id: MailboxId) -> bool {
            self.boxes.lock().unwrap().get(&id).is_none_or(|b| b.closed)
        }

        /// The operator replaces the message in a mailbox.
        pub fn tamper(&self, id: MailboxId, bytes: Vec<u8>) {
            if let Some(b) = self.boxes.lock().unwrap().get_mut(&id) {
                b.message = Some(bytes);
            }
        }
    }

    fn refused(why: &str) -> SyncError {
        SyncError::Refused(why.to_owned())
    }

    impl PairingRelay for MemoryRelay {
        async fn create(&self, owner: &DeviceKeys, expires_at: u64) -> Result<()> {
            let id = MailboxId::of(&owner.public().x25519);
            let mut boxes = self.boxes.lock().unwrap();
            if boxes.contains_key(&id) {
                return Err(refused("that mailbox exists"));
            }
            boxes.insert(
                id,
                Mailbox {
                    expires_at,
                    ..Default::default()
                },
            );
            Ok(())
        }

        async fn deliver(&self, mailbox: MailboxId, sealed: &[u8]) -> Result<()> {
            let now = self.now();
            let mut boxes = self.boxes.lock().unwrap();
            let b = boxes
                .get_mut(&mailbox)
                .ok_or_else(|| refused("no such mailbox"))?;
            if b.closed || now >= b.expires_at {
                return Err(refused("This mailbox is closed"));
            }
            if b.message.is_some() {
                return Err(refused("This mailbox takes one message"));
            }
            b.message = Some(sealed.to_vec());
            self.seen.lock().unwrap().push(sealed.to_vec());
            Ok(())
        }

        async fn fetch(&self, owner: &DeviceKeys) -> Result<Option<Vec<u8>>> {
            let id = MailboxId::of(&owner.public().x25519);
            let boxes = self.boxes.lock().unwrap();
            let b = boxes.get(&id).ok_or_else(|| refused("no such mailbox"))?;
            Ok(if b.closed { None } else { b.message.clone() })
        }

        async fn close(&self, owner: &DeviceKeys) -> Result<()> {
            let id = MailboxId::of(&owner.public().x25519);
            if let Some(b) = self.boxes.lock().unwrap().get_mut(&id) {
                b.closed = true;
                b.message = None;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::memory::MemoryRelay;
    use super::*;
    use crate::pairing::PairingLink;
    use dn_store::MigrationSet;

    const NOW: u64 = 1_800_000_000;
    const RELAY: &str = "https://relay.example.net";

    fn db() -> Mutex<Connection> {
        Mutex::new(
            dn_store::open_in_memory(&[MigrationSet::new("sync", crate::migrations())]).unwrap(),
        )
    }

    struct Setup {
        relay: MemoryRelay,
        db: Mutex<Connection>,
        node: DeviceKeys,
        phone: DeviceKeys,
        offer: RelayOffer,
        link: PairingLink,
    }

    async fn setup() -> Setup {
        let relay = MemoryRelay::at(NOW);
        let node = DeviceKeys::generate().unwrap();
        let offer = RelayOffer::open(&relay, RELAY, None, node.public(), NOW)
            .await
            .unwrap();
        let link = PairingLink::parse(&offer.link().to_url(), NOW + 1).unwrap();
        Setup {
            relay,
            db: db(),
            node,
            phone: DeviceKeys::generate().unwrap(),
            offer,
            link,
        }
    }

    fn mailbox(s: &Setup) -> MailboxId {
        s.link.relay.as_ref().unwrap().mailbox
    }

    /// The done-when test (#304): the Node has no listener and no address; everything goes
    /// through the Relay.
    #[tokio::test]
    async fn a_device_pairs_with_a_node_reachable_only_through_the_relay() {
        let mut s = setup().await;
        assert_eq!(s.link.address, None);
        assert_eq!(s.link.relay.as_ref().unwrap().url, RELAY);

        let pending = join_through_relay(
            &s.relay,
            &s.phone,
            &s.link,
            "iPhone",
            DeviceKind::Phone,
            NOW + 2,
        )
        .await
        .unwrap();
        assert_eq!(pending.answer(&s.relay, &s.phone).await.unwrap(), None);

        let join = s
            .offer
            .read_join(&s.relay, &s.db, &s.node, || NOW + 3)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(join.request.numbers, pending.numbers);
        assert_eq!(join.request.name, "iPhone");
        assert_eq!(join.request.kind, DeviceKind::Phone);
        assert_eq!(join.request.identity, s.phone.public());
        // One message: the pairing mailbox and the code are used up.
        assert!(s.relay.is_closed(mailbox(&s)));
        assert_eq!(s.offer.offer().seconds_left(NOW + 3), 0);

        let device = s
            .offer
            .finish_join(
                &s.relay,
                &s.db,
                &s.node,
                &join,
                PersonAnswer::Confirmed { by: None },
                5,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(device.keys, s.phone.public());
        assert_eq!(pending.answer(&s.relay, &s.phone).await.unwrap(), Some(()));
        let members = DeviceList::members(&s.db.lock().unwrap()).unwrap();
        assert_eq!(members.len(), 1);
    }

    #[tokio::test]
    async fn every_request_has_the_same_size_on_the_relay() {
        let mut sizes = Vec::new();
        for name in [
            "A",
            "A much longer name for this phone, nearly sixty-four chars",
        ] {
            let s = setup().await;
            join_through_relay(&s.relay, &s.phone, &s.link, name, DeviceKind::Phone, NOW)
                .await
                .unwrap();
            sizes.push(s.relay.seen.lock().unwrap()[0].len());
        }
        assert_eq!(sizes[0], sizes[1]);
    }

    #[test]
    fn padding_round_trips_and_refuses_junk() {
        assert_eq!(&*unpad(&pad(b"hello").unwrap()).unwrap(), b"hello");
        let mut p = pad(b"hello").unwrap();
        p[100] = 1;
        assert!(unpad(&p).is_err());
        assert!(unpad(&p[..100]).is_err());
        assert!(pad(&[0; PADDED]).is_err());
    }

    #[tokio::test]
    async fn the_relay_sees_no_names_or_keys() {
        let s = setup().await;
        join_through_relay(
            &s.relay,
            &s.phone,
            &s.link,
            "Kitchen iPhone",
            DeviceKind::Phone,
            NOW,
        )
        .await
        .unwrap();
        let seen = s.relay.seen.lock().unwrap();
        let text = String::from_utf8_lossy(&seen[0]);
        assert!(!text.contains("iPhone"));
        assert!(!text.contains(&b64::encode(&s.phone.public().ed25519)));
        assert!(!text.contains(&b64::encode(&s.phone.public().x25519)));
    }

    #[tokio::test]
    async fn declining_adds_nothing_and_tells_the_device() {
        let mut s = setup().await;
        let pending = join_through_relay(
            &s.relay,
            &s.phone,
            &s.link,
            "iPhone",
            DeviceKind::Phone,
            NOW,
        )
        .await
        .unwrap();
        let join = s
            .offer
            .read_join(&s.relay, &s.db, &s.node, || NOW)
            .await
            .unwrap()
            .unwrap();
        let added = s
            .offer
            .finish_join(&s.relay, &s.db, &s.node, &join, PersonAnswer::Declined, 5)
            .await
            .unwrap();
        assert!(added.is_none());
        assert!(matches!(
            pending.answer(&s.relay, &s.phone).await,
            Err(SyncError::Declined)
        ));
        assert!(
            DeviceList::members(&s.db.lock().unwrap())
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_request_without_the_code_is_refused_and_ends_the_pairing() {
        let mut s = setup().await;
        // Someone who knows the Node's key and the mailbox, but not the secret.
        let other = RelayOffer::open(&s.relay, RELAY, None, s.node.public(), NOW)
            .await
            .unwrap();
        let mut forged = other.link().clone();
        forged.relay = s.link.relay.clone();
        join_through_relay(&s.relay, &s.phone, &forged, "x", DeviceKind::Phone, NOW)
            .await
            .unwrap();
        assert!(matches!(
            s.offer.read_join(&s.relay, &s.db, &s.node, || NOW).await,
            Err(SyncError::WrongProof)
        ));
        // The mailbox took its one message; the real device can't deliver any more.
        assert!(
            join_through_relay(&s.relay, &s.phone, &s.link, "x", DeviceKind::Phone, NOW)
                .await
                .is_err()
        );
        assert_eq!(s.offer.offer().seconds_left(NOW), 0);
    }

    #[tokio::test]
    async fn a_proof_bound_to_another_mailbox_or_the_tls_route_is_refused() {
        let s = setup().await;
        let mailbox = mailbox(&s);
        let me = s.phone.public();
        let here = relay_binding(&s.link.node, &mailbox);
        let elsewhere = relay_binding(&s.link.node, &MailboxId([9; 32]));
        assert_ne!(here, elsewhere);
        // A TLS route proof (bound to an exporter value) never equals a Relay one.
        let tls = join_proof(&s.link, &me, &[7; 32]);
        let relay = join_proof(&s.link, &me, &here);
        assert_ne!(tls, relay);

        let mut offer = s.offer;
        assert!(matches!(
            offer
                .offer
                .accept(&me, &join_proof(&s.link, &me, &elsewhere), &here, NOW),
            Err(SyncError::WrongProof)
        ));
        assert!(matches!(
            offer.offer.accept(&me, &tls, &here, NOW),
            Err(SyncError::WrongProof)
        ));
        assert!(offer.offer.accept(&me, &relay, &here, NOW).is_ok());
    }

    #[tokio::test]
    async fn a_request_signed_by_another_key_or_tampered_with_is_refused() {
        // Tampered: the Relay swaps the bytes.
        let mut s = setup().await;
        join_through_relay(&s.relay, &s.phone, &s.link, "x", DeviceKind::Phone, NOW)
            .await
            .unwrap();
        let mut bytes = s.relay.seen.lock().unwrap()[0].clone();
        let at = bytes.len() / 2;
        bytes[at] ^= 1;
        s.relay.tamper(mailbox(&s), bytes);
        assert!(
            s.offer
                .read_join(&s.relay, &s.db, &s.node, || NOW)
                .await
                .is_err()
        );

        // Signed by a key other than the one it names.
        let s = setup().await;
        let route = s.link.relay.clone().unwrap();
        let liar = DeviceKeys::generate().unwrap();
        let me = s.phone.public();
        let body = serde_json::to_vec(&RequestBody {
            mailbox: route.mailbox.to_b64(),
            ed25519: b64::encode(&me.ed25519),
            x25519: b64::encode(&me.x25519),
            name: "x".into(),
            kind: "phone".into(),
            reply: MailboxId([1; 32]).to_b64(),
            proof: b64::encode(&join_proof(
                &s.link,
                &me,
                &relay_binding(&s.link.node, &route.mailbox),
            )),
        })
        .unwrap();
        let signed = sign(&liar, REQUEST_SIG, &route.mailbox, &body);
        let sealed = seal(&s.link.node.x25519, &route.mailbox, REQUEST_INFO, &signed).unwrap();
        s.relay.deliver(route.mailbox, &sealed).await.unwrap();
        let mut offer = s.offer;
        assert!(matches!(
            offer.read_join(&s.relay, &s.db, &s.node, || NOW).await,
            Err(SyncError::BadSignature)
        ));
    }

    #[tokio::test]
    async fn the_device_trusts_only_the_nodes_own_answer_to_its_own_request() {
        let mut s = setup().await;
        let pending = join_through_relay(
            &s.relay,
            &s.phone,
            &s.link,
            "iPhone",
            DeviceKind::Phone,
            NOW,
        )
        .await
        .unwrap();
        let join = s
            .offer
            .read_join(&s.relay, &s.db, &s.node, || NOW)
            .await
            .unwrap()
            .unwrap();
        let reply = MailboxId::of(&pending.reply.public().x25519);
        let answer = |signer: &DeviceKeys, request: [u8; 32]| {
            let body = serde_json::to_vec(&AnswerBody {
                mailbox: reply.to_b64(),
                request: b64::encode(&request),
                accepted: true,
            })
            .unwrap();
            let signed = sign(signer, ANSWER_SIG, &reply, &body);
            seal(&s.phone.public().x25519, &reply, ANSWER_INFO, &signed).unwrap()
        };
        // An impostor that knows the reply mailbox signs "accepted".
        let impostor = DeviceKeys::generate().unwrap();
        assert!(matches!(
            pending.check(&s.phone, &answer(&impostor, join.request_hash)),
            Err(SyncError::NodeKeyMismatch)
        ));
        // The Node's real signature, but for another request.
        assert!(matches!(
            pending.check(&s.phone, &answer(&s.node, [0; 32])),
            Err(SyncError::Protocol(_))
        ));
        assert!(
            pending
                .check(&s.phone, &answer(&s.node, join.request_hash))
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_removed_or_already_paired_device_is_refused() {
        for removed in [false, true] {
            let mut s = setup().await;
            {
                let conn = s.db.lock().unwrap();
                let d =
                    DeviceList::add(&conn, &s.phone.public(), "old", DeviceKind::Phone, None, 1)
                        .unwrap();
                if removed {
                    DeviceList::remove(&conn, d.id, 2).unwrap();
                }
            }
            join_through_relay(&s.relay, &s.phone, &s.link, "x", DeviceKind::Phone, NOW)
                .await
                .unwrap();
            let r = s.offer.read_join(&s.relay, &s.db, &s.node, || NOW).await;
            if removed {
                assert!(matches!(r, Err(SyncError::Removed)), "{r:?}");
            } else {
                assert!(matches!(r, Err(SyncError::AlreadyPaired)), "{r:?}");
            }
        }
    }

    #[tokio::test]
    async fn an_expired_code_closes_the_mailbox() {
        let mut s = setup().await;
        let expiry = NOW + OFFER_TTL_SECS;
        assert!(matches!(
            join_through_relay(&s.relay, &s.phone, &s.link, "x", DeviceKind::Phone, expiry).await,
            Err(SyncError::OfferClosed)
        ));
        assert!(matches!(
            s.offer.read_join(&s.relay, &s.db, &s.node, || expiry).await,
            Err(SyncError::OfferClosed)
        ));
        assert!(s.relay.is_closed(mailbox(&s)));
    }

    #[tokio::test]
    async fn each_offer_and_reply_gets_its_own_mailbox() {
        let relay = MemoryRelay::at(NOW);
        let node = DeviceKeys::generate().unwrap();
        let a = RelayOffer::open(&relay, RELAY, None, node.public(), NOW)
            .await
            .unwrap();
        let b = RelayOffer::open(&relay, RELAY, None, node.public(), NOW)
            .await
            .unwrap();
        assert_ne!(a.mailbox_id(), b.mailbox_id());
        assert_ne!(a.mailbox_id(), MailboxId::of(&node.public().x25519));
    }
}
