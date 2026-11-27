//! The paired channel between a device and its Node (ADR-0014 §2, §4, §8).
//!
//! - **TLS 1.3 only** (rustls with the aws-lc-rs provider), with **raw public keys**
//!   (RFC 7250) instead of certificates: each side presents its Ed25519 device key, and
//!   each side checks the other's key against the one it already knows. No certificate
//!   authority, no host names, no session resumption.
//!   - A device connecting to its Node **pins** the Node's key: from the pairing link
//!     while pairing, from the device list after that. Any other key fails the handshake
//!     with [`SyncError::NodeKeyMismatch`]. That's also why a proxy that terminates TLS
//!     breaks pairing on purpose (§4): it can't present the Node's key.
//!   - The Node accepts any Ed25519 key in the handshake, then decides who it is:
//!     [`authorize_member`] for a paired device, or [`read_join`] for a device that's
//!     pairing with a code. A removed device is refused either way.
//! - **Version handshake:** the first frames are a `hello` each way with the protocol's
//!   major and minor version. A different major refuses to sync, and the message names
//!   the side to update ([`SyncError::VersionMismatch`]). No partial sync.
//! - **Frames:** a 4-byte big-endian length, then a JSON message, at most
//!   [`MAX_FRAME`] bytes.
//! - **Channel binding:** both sides derive 32 bytes from the TLS exporter (RFC 8446
//!   §7.5). The pairing proof covers them, so a proof can't be used on another connection.
//!
//! Time limits on unauthenticated peers belong to the Node runtime, which knows when no
//! person is waiting on a prompt.

use crate::devices::{Device, DeviceKind, DeviceList};
use crate::identity::{DeviceKeys, PublicIdentity};
use crate::pairing::{CheckNumbers, PairingLink, PairingOffer, join_proof};
use crate::{Result, SyncError, VersionMismatch, b64};
use rusqlite::Connection;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, aws_lc_rs};
use rustls::pki_types::{
    CertificateDer, PrivatePkcs8KeyDer, ServerName, SubjectPublicKeyInfoDer, UnixTime,
};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::sign::CertifiedKey;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig,
    SignatureScheme,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};

/// Name of the protocol in the `hello` frame and in ALPN.
pub const PROTOCOL: &str = "demios-node";
/// Protocol major version: a different major refuses to sync.
pub const PROTOCOL_MAJOR: u16 = 1;
/// Protocol minor version: newer minors only add optional fields and messages a peer sends
/// only to a peer of that minor or later.
/// - 1: `SyncStart.applied`, `SyncState.floor` and the snapshot messages (#294).
pub const PROTOCOL_MINOR: u16 = 1;
/// Largest frame accepted, in bytes.
pub const MAX_FRAME: usize = 64 * 1024;
/// Largest frame once the other side is a checked member and the channel carries sync
/// (envelopes up to 1 MiB, base64, a few per frame).
pub const MAX_SYNC_FRAME: usize = 4 * 1024 * 1024;
const EXPORTER_LABEL: &[u8] = b"EXPORTER-demios-node-channel";
/// DER prefix of an Ed25519 SubjectPublicKeyInfo (RFC 8410), followed by the 32 key bytes.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The DER SubjectPublicKeyInfo of an Ed25519 key, as TLS raw public keys carry it.
pub fn ed25519_spki(key: &[u8; 32]) -> Vec<u8> {
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(key);
    der
}

fn ed25519_from_spki(der: &[u8]) -> Option<[u8; 32]> {
    der.strip_prefix(&ED25519_SPKI_PREFIX)?.try_into().ok()
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(aws_lc_rs::default_provider())
}

fn tls_error(e: rustls::Error) -> SyncError {
    SyncError::Tls(e.to_string())
}

fn certified_key(keys: &DeviceKeys, provider: &CryptoProvider) -> Result<Arc<CertifiedKey>> {
    let pkcs8 = keys.signing_pkcs8()?;
    let der = PrivatePkcs8KeyDer::from(pkcs8.to_vec());
    let signing = provider
        .key_provider
        .load_private_key(der.into())
        .map_err(tls_error)?;
    let spki = ed25519_spki(&keys.public().ed25519);
    Ok(Arc::new(CertifiedKey::new(
        vec![CertificateDer::from(spki)],
        signing,
    )))
}

fn verify_ed25519(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
    algs: &WebPkiSupportedAlgorithms,
) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
    if dss.scheme != SignatureScheme::ED25519 {
        return Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::NoSignatureSchemesInCommon,
        ));
    }
    rustls::crypto::verify_tls13_signature_with_raw_key(
        message,
        &SubjectPublicKeyInfoDer::from(cert.as_ref()),
        dss,
        algs,
    )
}

/// Client side: accepts only the pinned Ed25519 key.
#[derive(Debug)]
struct PinnedServer {
    expected: Vec<u8>,
    algs: WebPkiSupportedAlgorithms,
    mismatch: AtomicBool,
}

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if intermediates.is_empty() && end_entity.as_ref() == self.expected.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            self.mismatch.store(true, Ordering::SeqCst);
            Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls13RequiredForQuic,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_ed25519(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

/// Node side: requires a client key, accepts any well-formed Ed25519 key, and leaves the
/// decision (member, pairing, refused) to [`authorize_member`] and [`read_join`].
#[derive(Debug)]
struct AnyEd25519Client {
    algs: WebPkiSupportedAlgorithms,
}

impl ClientCertVerifier for AnyEd25519Client {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        if intermediates.is_empty() && ed25519_from_spki(end_entity.as_ref()).is_some() {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::BadEncoding,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls13RequiredForQuic,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_ed25519(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

fn client_config(
    keys: &DeviceKeys,
    pinned: &[u8; 32],
) -> Result<(ClientConfig, Arc<PinnedServer>)> {
    let provider = provider();
    let verifier = Arc::new(PinnedServer {
        expected: ed25519_spki(pinned),
        algs: provider.signature_verification_algorithms,
        mismatch: AtomicBool::new(false),
    });
    let key = certified_key(keys, &provider)?;
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_error)?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_client_cert_resolver(Arc::new(
            rustls::client::AlwaysResolvesClientRawPublicKeys::new(key),
        ));
    config.alpn_protocols = vec![PROTOCOL.as_bytes().to_vec()];
    config.enable_sni = false;
    config.resumption = rustls::client::Resumption::disabled();
    Ok((config, verifier))
}

fn server_config(keys: &DeviceKeys) -> Result<ServerConfig> {
    let provider = provider();
    let verifier = Arc::new(AnyEd25519Client {
        algs: provider.signature_verification_algorithms,
    });
    let key = certified_key(keys, &provider)?;
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_error)?
        .with_client_cert_verifier(verifier)
        .with_cert_resolver(Arc::new(
            rustls::server::AlwaysResolvesServerRawPublicKeys::new(key),
        ));
    config.alpn_protocols = vec![PROTOCOL.as_bytes().to_vec()];
    config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    Ok(config)
}

/// A message on the channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        protocol: String,
        major: u16,
        minor: u16,
    },
    /// A pairing device's keys and proof. Its Ed25519 key is the one it presented in TLS.
    Join {
        x25519: String,
        name: String,
        kind: String,
        proof: String,
    },
    JoinAnswer {
        accepted: bool,
    },
    /// A paired device asks for the Node's status (Settings › Node).
    StatusRequest,
    /// The Node's status, as every client shows it.
    Status(NodeStatus),
    /// A paired device asks for the device list (Settings › Node).
    DevicesRequest,
    /// The current members, the Node included.
    Devices {
        devices: Vec<DeviceInfo>,
    },
    /// A paired device removes a device (itself included, to unpair). The Node can't be
    /// removed this way.
    RemoveDevice {
        id: String,
    },
    /// A paired device asks for the held calls waiting for a person (ADR-0014 §6;
    /// read-only for now, ADR-0019).
    HeldRequest,
    /// The held calls waiting, oldest first.
    Held {
        items: Vec<HeldItem>,
    },
    /// The request was done.
    Done,
    /// A device starts a sync round: how much of the membership chain it has, and what it
    /// has applied (author id hex -> seq).
    SyncStart {
        chain_len: u64,
        vector: std::collections::BTreeMap<String, u64>,
        /// What the device has applied, for compaction (minor 1).
        #[serde(default)]
        applied: std::collections::BTreeMap<String, u64>,
    },
    /// The Node's answer: the chain entries the device lacks, and what the Node has applied.
    SyncState {
        entries: Vec<ChainEntry>,
        vector: std::collections::BTreeMap<String, u64>,
        /// Envelopes at or below this are no longer in the Node's log: a device that lacks
        /// any of them asks for a snapshot (minor 1).
        #[serde(default)]
        floor: std::collections::BTreeMap<String, u64>,
    },
    /// A device asks for the Node's current snapshot (minor 1).
    SnapshotRequest,
    /// The snapshot the Node has ready: the vector it covers and its sealed size.
    Snapshot {
        vector: std::collections::BTreeMap<String, u64>,
        size: u64,
    },
    /// A device asks for the sealed snapshot's bytes from `offset`.
    SnapshotPull {
        vector: std::collections::BTreeMap<String, u64>,
        offset: u64,
    },
    /// Up to 1 MiB of the sealed snapshot (base64); `more` when another pull gets more.
    SnapshotChunk {
        data: String,
        more: bool,
    },
    /// Envelopes (base64) for the Node to keep and apply.
    Push {
        envelopes: Vec<String>,
    },
    /// How many were new to the Node.
    Pushed {
        stored: u32,
    },
    /// A device asks for envelopes after its vector.
    Pull {
        vector: std::collections::BTreeMap<String, u64>,
    },
    /// Envelopes (base64) the device lacks; `more` when another `Pull` gets more.
    Envelopes {
        envelopes: Vec<String>,
        more: bool,
    },
    /// The request was refused; the reason is shown as plain text.
    Refused {
        reason: String,
    },
}

/// A held call as a paired device shows it: everything the review needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldItem {
    pub id: String,
    /// The add-on's id.
    pub plugin: String,
    /// The job's name.
    pub job: String,
    /// The capability id, e.g. `plugin.brief.send_mail`.
    pub tool: String,
    /// What the call does, in plain words (shown as plain text, never markup).
    pub summary: String,
    /// The exact arguments, as stored (JSON text).
    pub arguments: String,
    pub arguments_sha256: String,
    /// Unix seconds, as the Node's job clock keeps them.
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

/// One membership chain entry on the wire (base64).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainEntry {
    pub bytes: String,
    pub signature: String,
}

/// One device in the list a client shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// The device id (hex).
    pub id: String,
    pub name: String,
    /// `desktop`, `node` or `phone`.
    pub kind: String,
    /// Unix milliseconds.
    pub last_synced_at: Option<i64>,
    /// The device that asked.
    pub this_device: bool,
}

/// What a Node reports about itself (ADR-0014 §7, §8): its version, the secret store in
/// use, and how many devices are paired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeStatus {
    pub name: String,
    pub version: String,
    /// Stable id of the store, e.g. `systemd_credential` or `insecure_plaintext`.
    pub secret_store: String,
    /// What the status screen says, e.g. "Encrypted file, key from a systemd credential".
    pub secret_store_label: String,
    /// `false` for a plaintext store: every client shows a warning.
    pub protected_at_rest: bool,
    pub devices: u32,
    /// `true` when the person chose to accept connections from outside the local network
    /// (ADR-0014 §4a); every client says so. `None` from older Nodes, which don't report
    /// it: clients then don't show it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachable_from_outside: Option<bool>,
}

/// An open, authenticated channel whose versions match.
pub struct Channel<S> {
    tls: TlsStream<S>,
    max_frame: usize,
    peer: [u8; 32],
    binding: [u8; 32],
    peer_minor: u16,
}

impl<S> std::fmt::Debug for Channel<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Channel")
            .field("peer", &b64::encode(&self.peer))
            .field("peer_minor", &self.peer_minor)
            .finish_non_exhaustive()
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Channel<S> {
    fn open(tls: TlsStream<S>) -> Result<Self> {
        let peer = tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|certs| certs.first())
            .and_then(|c| ed25519_from_spki(c.as_ref()))
            .ok_or(SyncError::Protocol("no peer key"))?;
        let mut binding = [0u8; 32];
        match &tls {
            TlsStream::Client(s) => {
                s.get_ref()
                    .1
                    .export_keying_material(&mut binding, EXPORTER_LABEL, None)
            }
            TlsStream::Server(s) => {
                s.get_ref()
                    .1
                    .export_keying_material(&mut binding, EXPORTER_LABEL, None)
            }
        }
        .map_err(tls_error)?;
        Ok(Self {
            tls,
            max_frame: MAX_FRAME,
            peer,
            binding,
            peer_minor: 0,
        })
    }

    /// The other side's Ed25519 key, as checked in the TLS handshake.
    pub fn peer_signing_key(&self) -> &[u8; 32] {
        &self.peer
    }

    /// The channel binding (TLS exporter), the same on both sides of one connection.
    pub fn binding(&self) -> &[u8; 32] {
        &self.binding
    }

    /// Allows sync-sized frames ([`MAX_SYNC_FRAME`]). Call it only once the other side is
    /// a checked member (the Node after [`authorize_member`], a device after [`connect`]).
    pub fn allow_sync_frames(&mut self) {
        self.max_frame = MAX_SYNC_FRAME;
    }

    /// The other side's protocol minor version.
    pub fn peer_minor(&self) -> u16 {
        self.peer_minor
    }

    pub async fn send(&mut self, message: &Message) -> Result<()> {
        let body = serde_json::to_vec(message).map_err(|_| SyncError::Protocol("encode"))?;
        if body.len() > self.max_frame {
            return Err(SyncError::Protocol("frame too large"));
        }
        let len = u32::try_from(body.len()).map_err(|_| SyncError::Protocol("frame too large"))?;
        self.tls.write_all(&len.to_be_bytes()).await?;
        self.tls.write_all(&body).await?;
        self.tls.flush().await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Message> {
        let mut len = [0u8; 4];
        self.tls.read_exact(&mut len).await?;
        let len = u32::from_be_bytes(len) as usize;
        if len > self.max_frame {
            return Err(SyncError::Protocol("frame too large"));
        }
        let mut body = vec![0u8; len];
        self.tls.read_exact(&mut body).await?;
        serde_json::from_slice(&body).map_err(|_| SyncError::Protocol("unreadable message"))
    }

    fn hello() -> Message {
        Message::Hello {
            protocol: PROTOCOL.to_owned(),
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
        }
    }

    fn check_hello(&mut self, message: Message) -> Result<()> {
        let Message::Hello {
            protocol,
            major,
            minor,
        } = message
        else {
            return Err(SyncError::Protocol("expected hello"));
        };
        if protocol != PROTOCOL {
            return Err(SyncError::Protocol("not a Demios Node connection"));
        }
        if major != PROTOCOL_MAJOR {
            return Err(SyncError::VersionMismatch(VersionMismatch {
                ours: PROTOCOL_MAJOR,
                theirs: major,
            }));
        }
        self.peer_minor = minor;
        Ok(())
    }

    /// Shuts the TLS session down cleanly.
    pub async fn close(mut self) -> Result<()> {
        self.tls.shutdown().await?;
        Ok(())
    }
}

/// Connects to a Node whose Ed25519 key is `node_key`, and exchanges versions.
pub async fn connect<S: AsyncRead + AsyncWrite + Unpin>(
    io: S,
    keys: &DeviceKeys,
    node_key: &[u8; 32],
) -> Result<Channel<S>> {
    let (config, verifier) = client_config(keys, node_key)?;
    let name = ServerName::try_from(PROTOCOL).map_err(|_| SyncError::Protocol("server name"))?;
    let tls = match TlsConnector::from(Arc::new(config)).connect(name, io).await {
        Ok(tls) => tls,
        Err(_) if verifier.mismatch.load(Ordering::SeqCst) => {
            return Err(SyncError::NodeKeyMismatch);
        }
        Err(e) => return Err(SyncError::Tls(e.to_string())),
    };
    let mut channel = Channel::open(TlsStream::Client(tls))?;
    channel.send(&Channel::<S>::hello()).await?;
    let reply = channel.recv().await?;
    channel.check_hello(reply)?;
    Ok(channel)
}

/// Accepts a device's connection on the Node, and exchanges versions. The device is not
/// trusted yet: call [`authorize_member`], or [`read_join`] while pairing.
pub async fn accept<S: AsyncRead + AsyncWrite + Unpin>(
    io: S,
    keys: &DeviceKeys,
) -> Result<Channel<S>> {
    let config = server_config(keys)?;
    let tls = TlsAcceptor::from(Arc::new(config))
        .accept(io)
        .await
        .map_err(|e| SyncError::Tls(e.to_string()))?;
    let mut channel = Channel::open(TlsStream::Server(tls))?;
    let hello = channel.recv().await?;
    // Always answer with our version first, so the other side can say which to update.
    channel.send(&Channel::<S>::hello()).await?;
    channel.check_hello(hello)?;
    Ok(channel)
}

/// The paired device on the other end, or an error if it isn't a current member.
pub fn authorize_member<S>(conn: &Connection, channel: &Channel<S>) -> Result<Device> {
    let device =
        DeviceList::by_signing_key(conn, &channel.peer)?.ok_or(SyncError::UnknownDevice)?;
    if !device.is_member() {
        return Err(SyncError::Removed);
    }
    Ok(device)
}

/// A pairing device's request, checked against the offer. The person confirms the
/// [`numbers`](Self::numbers) before [`finish_join`] adds the device.
#[derive(Debug)]
pub struct JoinRequest {
    pub identity: PublicIdentity,
    pub name: String,
    pub kind: DeviceKind,
    pub numbers: CheckNumbers,
}

fn parse_kind(kind: &str) -> Option<DeviceKind> {
    Some(match kind {
        "desktop" => DeviceKind::Desktop,
        "phone" => DeviceKind::Phone,
        _ => return None,
    })
}

pub fn kind_name(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Desktop => "desktop",
        DeviceKind::Node => "node",
        DeviceKind::Phone => "phone",
    }
}

/// The joining device's side, after [`join`]: show the numbers, then wait for the Node.
pub struct JoinPending<S> {
    channel: Channel<S>,
    pub numbers: CheckNumbers,
}

impl<S: AsyncRead + AsyncWrite + Unpin> JoinPending<S> {
    /// Waits for the person to confirm on the Node. No time limit: they may be reading the
    /// numbers. The pairing code's own expiry still applies on the Node.
    pub async fn answer(mut self) -> Result<Channel<S>> {
        match self.channel.recv().await? {
            Message::JoinAnswer { accepted: true } => Ok(self.channel),
            Message::JoinAnswer { accepted: false } => Err(SyncError::Declined),
            _ => Err(SyncError::Protocol("expected the Node's answer")),
        }
    }

    /// Stops pairing (the numbers didn't match).
    pub async fn cancel(self) -> Result<()> {
        self.channel.close().await
    }
}

/// Pairs this device with the Node in `link`: connects pinned to the Node's key, proves
/// it has the code on this connection, and returns the numbers to compare.
pub async fn join<S: AsyncRead + AsyncWrite + Unpin>(
    io: S,
    keys: &DeviceKeys,
    link: &PairingLink,
    name: &str,
    kind: DeviceKind,
) -> Result<JoinPending<S>> {
    if kind == DeviceKind::Node {
        return Err(SyncError::Protocol("a Node doesn't join another Node"));
    }
    let mut channel = connect(io, keys, &link.node.ed25519).await?;
    let proof = join_proof(link, &keys.public(), channel.binding());
    channel
        .send(&Message::Join {
            x25519: b64::encode(&keys.public().x25519),
            name: name.to_owned(),
            kind: kind_name(kind).to_owned(),
            proof: b64::encode(&proof),
        })
        .await?;
    Ok(JoinPending {
        channel,
        numbers: CheckNumbers::new(&link.node, &keys.public()),
    })
}

fn lock_db(db: &Mutex<Connection>) -> std::sync::MutexGuard<'_, Connection> {
    db.lock().unwrap_or_else(|p| p.into_inner())
}

/// On the Node: reads a pairing request and checks it against the offer. A wrong or late
/// proof, or a device that was removed, gets a refusal and an error. The database is
/// locked only for the device-list lookup, never across a network wait.
pub async fn read_join<S: AsyncRead + AsyncWrite + Unpin>(
    db: &Mutex<Connection>,
    channel: &mut Channel<S>,
    offer: &mut PairingOffer,
    now: impl Fn() -> u64,
) -> Result<JoinRequest> {
    let result = check_join(db, channel, offer, now).await;
    if result.is_err() {
        // Best effort: the error itself is what's reported.
        let _ = channel.send(&Message::JoinAnswer { accepted: false }).await;
    }
    result
}

async fn check_join<S: AsyncRead + AsyncWrite + Unpin>(
    db: &Mutex<Connection>,
    channel: &mut Channel<S>,
    offer: &mut PairingOffer,
    now: impl Fn() -> u64,
) -> Result<JoinRequest> {
    let Message::Join {
        x25519,
        name,
        kind,
        proof,
    } = channel.recv().await?
    else {
        return Err(SyncError::Protocol("expected a pairing request"));
    };
    let existing = DeviceList::by_signing_key(&lock_db(db), &channel.peer)?;
    if let Some(existing) = existing {
        return Err(if existing.is_member() {
            SyncError::AlreadyPaired
        } else {
            SyncError::Removed
        });
    }
    let x25519 = b64::decode_array::<32>(&x25519).ok_or(SyncError::Protocol("bad key"))?;
    let kind = parse_kind(&kind).ok_or(SyncError::Protocol("bad device kind"))?;
    let proof = b64::decode_array::<32>(&proof).ok_or(SyncError::Protocol("bad proof"))?;
    let identity = PublicIdentity {
        ed25519: channel.peer,
        x25519,
    };
    // The name is shown to the person before the device is added: same rules as the list.
    let name = crate::devices::clean_name(&name)?;
    // The time the request arrived, not when the Node started waiting for it.
    let numbers = offer.accept(&identity, &proof, &channel.binding, now())?;
    Ok(JoinRequest {
        identity,
        name,
        kind,
        numbers,
    })
}

/// On the Node, after the person answered: adds the device if they confirmed the numbers,
/// and tells the joining device either way.
pub async fn finish_join<S: AsyncRead + AsyncWrite + Unpin>(
    db: &Mutex<Connection>,
    channel: &mut Channel<S>,
    request: &JoinRequest,
    confirmed_by: Option<crate::DeviceId>,
    confirmed: bool,
    now_ms: i64,
) -> Result<Option<Device>> {
    if !confirmed {
        channel
            .send(&Message::JoinAnswer { accepted: false })
            .await?;
        return Ok(None);
    }
    let added = DeviceList::add(
        &lock_db(db),
        &request.identity,
        &request.name,
        request.kind,
        confirmed_by,
        now_ms,
    );
    channel
        .send(&Message::JoinAnswer {
            accepted: added.is_ok(),
        })
        .await?;
    added.map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_store::MigrationSet;
    use tokio::io::{DuplexStream, duplex};

    const NOW: u64 = 1_800_000_000;

    fn db() -> Connection {
        dn_store::open_in_memory(&[MigrationSet::new("sync", crate::migrations())]).unwrap()
    }

    fn pipe() -> (DuplexStream, DuplexStream) {
        duplex(256 * 1024)
    }

    #[tokio::test]
    async fn a_device_pairs_over_the_channel_and_then_connects_as_a_member() {
        let db = Mutex::new(db());
        let conn_guard = || db.lock().unwrap();
        let node = DeviceKeys::generate().unwrap();
        let node_key = node.public().ed25519;
        let laptop = DeviceKeys::generate().unwrap();
        let mut offer = PairingOffer::new("nas:7443", node.public(), NOW).unwrap();
        let link = PairingLink::parse(&offer.link().to_url(), NOW).unwrap();

        let (a, b) = pipe();
        let node_side = async {
            let mut ch = accept(b, &node).await.unwrap();
            assert!(matches!(
                authorize_member(&conn_guard(), &ch),
                Err(SyncError::UnknownDevice)
            ));
            let req = read_join(&db, &mut ch, &mut offer, || NOW + 5)
                .await
                .unwrap();
            let device = finish_join(&db, &mut ch, &req, None, true, 1)
                .await
                .unwrap()
                .unwrap();
            (req.numbers, device)
        };
        let joiner_side = async {
            let pending = join(a, &laptop, &link, "MacBook Pro", DeviceKind::Desktop)
                .await
                .unwrap();
            let numbers = pending.numbers;
            pending.answer().await.unwrap();
            numbers
        };
        let ((node_numbers, device), joiner_numbers) = tokio::join!(node_side, joiner_side);
        assert_eq!(node_numbers, joiner_numbers);
        assert_eq!(device.keys, laptop.public());
        assert_eq!(device.name, "MacBook Pro");

        // Later: an ordinary connection, pinned to the Node's key from the device list.
        let (a, b) = pipe();
        let (server, client) = tokio::join!(accept(b, &node), connect(a, &laptop, &node_key));
        let server = server.unwrap();
        let client = client.unwrap();
        assert_eq!(
            authorize_member(&conn_guard(), &server).unwrap().id,
            laptop.id()
        );
        assert_eq!(server.binding(), client.binding());
        assert_eq!(client.peer_signing_key(), &node_key);
    }

    /// ADR-0014 §4: a proxy that terminates TLS breaks pairing on purpose. It presents its
    /// own key, which isn't the Node's key from the link, so the joining device stops
    /// before sending anything, and the Node's offer is untouched.
    #[tokio::test]
    async fn a_proxy_that_terminates_tls_breaks_pairing() {
        let node = DeviceKeys::generate().unwrap();
        let proxy = DeviceKeys::generate().unwrap();
        let laptop = DeviceKeys::generate().unwrap();
        let offer = PairingOffer::new("node.example.net:7443", node.public(), NOW).unwrap();
        let link = PairingLink::parse(&offer.link().to_url(), NOW).unwrap();

        // The joiner reaches the proxy, which terminates TLS with its own key.
        let (a, b) = pipe();
        let proxy_side = accept(b, &proxy);
        let joiner_side = join(a, &laptop, &link, "MacBook Pro", DeviceKind::Desktop);
        let (proxy_result, joiner_result) = tokio::join!(proxy_side, joiner_side);
        assert!(matches!(joiner_result, Err(SyncError::NodeKeyMismatch)));
        assert!(proxy_result.is_err());
        assert_eq!(offer.seconds_left(NOW), crate::pairing::OFFER_TTL_SECS);
    }

    /// A proxy that does hold a valid pairing code still can't pass on a proof made on
    /// another connection: the proof is bound to the TLS channel it was made on.
    #[tokio::test]
    async fn a_proof_relayed_from_another_connection_is_refused() {
        let db = Mutex::new(db());
        let conn_guard = || db.lock().unwrap();
        let node = DeviceKeys::generate().unwrap();
        let node_key = node.public().ed25519;
        let laptop = DeviceKeys::generate().unwrap();
        let mut offer = PairingOffer::new("nas:7443", node.public(), NOW).unwrap();
        let link = PairingLink::parse(&offer.link().to_url(), NOW).unwrap();

        let (a, b) = pipe();
        let node_side = async {
            let mut ch = accept(b, &node).await.unwrap();
            read_join(&db, &mut ch, &mut offer, || NOW).await
        };
        let relay_side = async {
            // Same keys as the laptop (as if it had them), but a proof from another channel.
            let mut ch = connect(a, &laptop, &node_key).await.unwrap();
            let stale = join_proof(&link, &laptop.public(), &[9; 32]);
            ch.send(&Message::Join {
                x25519: b64::encode(&laptop.public().x25519),
                name: "Laptop".into(),
                kind: "desktop".into(),
                proof: b64::encode(&stale),
            })
            .await
            .unwrap();
            ch.recv().await.unwrap()
        };
        let (node_result, answer) = tokio::join!(node_side, relay_side);
        assert!(matches!(node_result, Err(SyncError::WrongProof)));
        assert_eq!(answer, Message::JoinAnswer { accepted: false });
        assert!(DeviceList::members(&conn_guard()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_node_refuses_a_removed_device_on_connect_and_on_re_pairing() {
        let db = Mutex::new(db());
        let conn_guard = || db.lock().unwrap();
        let node = DeviceKeys::generate().unwrap();
        let node_key = node.public().ed25519;
        let phone = DeviceKeys::generate().unwrap();
        DeviceList::add(
            &conn_guard(),
            &phone.public(),
            "Work phone",
            DeviceKind::Phone,
            None,
            1,
        )
        .unwrap();
        DeviceList::remove(&conn_guard(), phone.id(), 2).unwrap();

        let (a, b) = pipe();
        let (server, client) = tokio::join!(accept(b, &node), connect(a, &phone, &node_key));
        let server = server.unwrap();
        client.unwrap();
        assert!(matches!(
            authorize_member(&conn_guard(), &server),
            Err(SyncError::Removed)
        ));

        // With a fresh, valid pairing code it is still refused.
        let mut offer = PairingOffer::new("nas:7443", node.public(), NOW).unwrap();
        let link = PairingLink::parse(&offer.link().to_url(), NOW).unwrap();
        let (a, b) = pipe();
        let node_side = async {
            let mut ch = accept(b, &node).await.unwrap();
            read_join(&db, &mut ch, &mut offer, || NOW).await
        };
        let joiner_side = async {
            join(a, &phone, &link, "Work phone", DeviceKind::Phone)
                .await
                .unwrap()
                .answer()
                .await
        };
        let (node_result, joiner_result) = tokio::join!(node_side, joiner_side);
        assert!(matches!(node_result, Err(SyncError::Removed)));
        assert!(matches!(joiner_result, Err(SyncError::Declined)));
    }

    #[tokio::test]
    async fn the_person_declining_on_the_node_adds_nothing() {
        let db = Mutex::new(db());
        let conn_guard = || db.lock().unwrap();
        let node = DeviceKeys::generate().unwrap();
        let laptop = DeviceKeys::generate().unwrap();
        let mut offer = PairingOffer::new("nas:7443", node.public(), NOW).unwrap();
        let link = PairingLink::parse(&offer.link().to_url(), NOW).unwrap();
        let (a, b) = pipe();
        let node_side = async {
            let mut ch = accept(b, &node).await.unwrap();
            let req = read_join(&db, &mut ch, &mut offer, || NOW).await.unwrap();
            finish_join(&db, &mut ch, &req, None, false, 1)
                .await
                .unwrap()
        };
        let joiner_side = async {
            join(a, &laptop, &link, "Laptop", DeviceKind::Desktop)
                .await
                .unwrap()
                .answer()
                .await
        };
        let (added, answer) = tokio::join!(node_side, joiner_side);
        assert!(added.is_none());
        assert!(matches!(answer, Err(SyncError::Declined)));
        assert!(DeviceList::members(&conn_guard()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn connecting_to_the_wrong_node_fails_the_handshake() {
        let node = DeviceKeys::generate().unwrap();
        let node_key = node.public().ed25519;
        let other = DeviceKeys::generate().unwrap();
        let laptop = DeviceKeys::generate().unwrap();
        let (a, b) = pipe();
        let (server, client) = tokio::join!(accept(b, &other), connect(a, &laptop, &node_key));
        assert!(matches!(client, Err(SyncError::NodeKeyMismatch)));
        assert!(server.is_err());
    }

    /// A peer speaking another major version: both sides refuse, and each names the side
    /// to update.
    #[tokio::test]
    async fn a_different_major_version_refuses_to_sync_and_names_the_side_to_update() {
        let node = DeviceKeys::generate().unwrap();
        let node_key = node.public().ed25519;
        let laptop = DeviceKeys::generate().unwrap();
        let (a, b) = pipe();
        let node_side = accept(b, &node);
        let newer_client = async {
            let (config, _) = client_config(&laptop, &node_key).unwrap();
            let tls = TlsConnector::from(Arc::new(config))
                .connect(ServerName::try_from(PROTOCOL).unwrap(), a)
                .await
                .unwrap();
            let mut ch = Channel::open(TlsStream::Client(tls)).unwrap();
            ch.send(&Message::Hello {
                protocol: PROTOCOL.into(),
                major: PROTOCOL_MAJOR + 1,
                minor: 0,
            })
            .await
            .unwrap();
            let reply = ch.recv().await.unwrap();
            // The newer side sees the older version.
            assert_eq!(
                reply,
                Message::Hello {
                    protocol: PROTOCOL.into(),
                    major: PROTOCOL_MAJOR,
                    minor: PROTOCOL_MINOR
                }
            );
        };
        let (node_result, ()) = tokio::join!(node_side, newer_client);
        let Err(SyncError::VersionMismatch(m)) = node_result else {
            panic!("expected a version mismatch");
        };
        assert_eq!(m.theirs, PROTOCOL_MAJOR + 1);
        assert!(m.to_string().ends_with("Update this app to sync"));
        let older = VersionMismatch { ours: 2, theirs: 1 };
        assert!(
            older
                .to_string()
                .ends_with("Update the other device to sync")
        );
    }

    #[tokio::test]
    async fn oversized_and_garbled_frames_are_refused() {
        let node = DeviceKeys::generate().unwrap();
        let node_key = node.public().ed25519;
        let laptop = DeviceKeys::generate().unwrap();
        let (a, b) = pipe();
        let (server, client) = tokio::join!(accept(b, &node), connect(a, &laptop, &node_key));
        let mut server = server.unwrap();
        let mut client = client.unwrap();
        client
            .tls
            .write_all(&((MAX_FRAME as u32) + 1).to_be_bytes())
            .await
            .unwrap();
        client.tls.flush().await.unwrap();
        assert!(matches!(
            server.recv().await,
            Err(SyncError::Protocol("frame too large"))
        ));
        let (a, b) = pipe();
        let (server, client) = tokio::join!(accept(b, &node), connect(a, &laptop, &node_key));
        let mut server = server.unwrap();
        let mut client = client.unwrap();
        client.tls.write_all(&3u32.to_be_bytes()).await.unwrap();
        client.tls.write_all(b"{{{").await.unwrap();
        client.tls.flush().await.unwrap();
        assert!(matches!(
            server.recv().await,
            Err(SyncError::Protocol("unreadable message"))
        ));
    }

    #[test]
    fn spki_round_trips_and_rejects_other_keys() {
        let key = [5u8; 32];
        assert_eq!(ed25519_from_spki(&ed25519_spki(&key)), Some(key));
        let mut other = ed25519_spki(&key);
        other[8] = 0x71; // Ed448
        assert_eq!(ed25519_from_spki(&other), None);
        assert_eq!(ed25519_from_spki(&ed25519_spki(&key)[..40]), None);
    }
}
