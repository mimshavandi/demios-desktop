//! The iPhone app's Rust core (ADR-0019 §2), exported to Swift with UniFFI.
//!
//! **This first step is read-only:**
//! - pair with a Node over the paired channel (dn-sync: TLS 1.3, RFC 7250 keys, check
//!   numbers);
//! - read the Node's status;
//! - read the held calls waiting for a person, with their exact arguments;
//! - unpair.
//!
//! Answering with Face ID and the Secure Enclave key, the phone's sealed subset, the Relay
//! and notifications come in later steps.
//!
//! **Keys:** this phone's transport keys live in the iOS Keychain, through the Swift
//! [`KeyStore`] (this-device-only items, never synced to iCloud). They are made once and
//! never replaced quietly: an unreadable Keychain is an error.
//!
//! **Data:** a small SQLite file in the app's container, which the Swift side keeps under
//! Data Protection *Complete*. It holds the paired Node and its keys, nothing else yet.
//!
//! **Unsafe code:** none is written here. UniFFI's generated scaffolding is the only
//! `unsafe` in this crate, and `scripts/check-unsafe.py` fails CI otherwise
//! (ADR-0009, ADR-0019 §2).

use dn_secrets::{Secret, SecretError, SecretId, SecretStore};
use dn_store::{Connection, MigrationSet};
use dn_sync::SyncError;
use dn_sync::devices::DeviceKind;
use dn_sync::identity::DeviceKeys;
use dn_sync::link::{self, NodeLink};
use dn_sync::pairing::PairingLink;
use dn_sync::transport::{self, Channel, JoinPending, Message};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;

uniffi::setup_scaffolding!();

/// How long reaching the Node may take before it counts as not answering.
const CONNECT_SECS: u64 = 10;

/// What went wrong, with words the app shows as they are.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MobileError {
    /// The link isn't a Demios pairing link, or has expired.
    #[error("{reason}")]
    Link { reason: String },
    /// The Node is reachable only through a Relay, which this version can't use yet.
    #[error(
        "This Node can be reached only through a Relay, and this version of Demios can't pair \
         through a Relay yet. Pair on the Node's network."
    )]
    RelayOnly,
    #[error("Couldn't reach {address}: {reason}")]
    Unreachable { address: String, reason: String },
    /// The device at the address isn't the Node from the link (or the paired one).
    #[error(
        "Pairing stopped. The device at that address isn't the Node from your link. Someone \
         may be between them. Get a new link from the Node itself."
    )]
    NodeKeyMismatch,
    #[error("The Node didn't accept this phone.")]
    Declined,
    #[error("{reason}")]
    VersionMismatch { reason: String },
    #[error("This phone isn't paired with a Node.")]
    NotPaired,
    #[error("This phone is already paired with a Node. Unpair it first.")]
    AlreadyPaired,
    #[error("No pairing is waiting.")]
    NoPairing,
    #[error("The Node refused: {reason}")]
    Refused { reason: String },
    /// The Keychain couldn't be read or written. Keys are never replaced because of it.
    #[error("The Keychain failed: {reason}")]
    Keychain { reason: String },
    #[error("This phone's data couldn't be read: {reason}")]
    Storage { reason: String },
}

impl From<SyncError> for MobileError {
    fn from(e: SyncError) -> Self {
        match e {
            SyncError::BadLink(_) | SyncError::NewerLinkVersion | SyncError::OfferClosed => {
                Self::Link {
                    reason: link_reason(&e),
                }
            }
            SyncError::NodeKeyMismatch => Self::NodeKeyMismatch,
            SyncError::Declined => Self::Declined,
            SyncError::VersionMismatch(v) => Self::VersionMismatch {
                reason: v.to_string(),
            },
            SyncError::AlreadyPaired => Self::AlreadyPaired,
            SyncError::Refused(reason) => Self::Refused { reason },
            SyncError::Secrets(e) => Self::Keychain {
                reason: e.to_string(),
            },
            SyncError::DamagedKeys(why) => Self::Keychain {
                reason: format!("this phone's stored keys are damaged ({why})"),
            },
            SyncError::Db(e) => Self::Storage {
                reason: e.to_string(),
            },
            other => Self::Refused {
                reason: other.to_string(),
            },
        }
    }
}

fn link_reason(e: &SyncError) -> String {
    match e {
        SyncError::OfferClosed => {
            "This link has expired or was already used. Make a new one on the Node.".into()
        }
        SyncError::NewerLinkVersion => {
            "This link is from a newer Demios. Update this app, then pair.".into()
        }
        SyncError::BadLink(why) => format!("That isn't a Demios pairing link: {why}."),
        other => other.to_string(),
    }
}

/// The Keychain, implemented in Swift (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, no
/// iCloud sync). `name` is a short account name such as `device/identity`.
#[uniffi::export(with_foreign)]
pub trait KeyStore: Send + Sync {
    fn get(&self, name: String) -> Result<Option<String>, MobileError>;
    fn set(&self, name: String, value: String) -> Result<(), MobileError>;
    /// Returns whether something was deleted.
    fn delete(&self, name: String) -> Result<bool, MobileError>;
}

/// dn-sync's secret store, over the Swift Keychain.
struct Keychain(Arc<dyn KeyStore>);

fn keychain_error(id: &SecretId, e: MobileError) -> SecretError {
    SecretError::Backend {
        id: id.account(),
        message: e.to_string(),
    }
}

impl SecretStore for Keychain {
    fn get(&self, id: &SecretId) -> Result<Option<Secret>, SecretError> {
        self.0
            .get(id.account())
            .map(|v| v.map(Secret::new))
            .map_err(|e| keychain_error(id, e))
    }

    fn set(&self, id: &SecretId, value: &Secret) -> Result<(), SecretError> {
        self.0
            .set(id.account(), value.expose().to_owned())
            .map_err(|e| keychain_error(id, e))
    }

    fn delete(&self, id: &SecretId) -> Result<bool, SecretError> {
        self.0
            .delete(id.account())
            .map_err(|e| keychain_error(id, e))
    }
}

/// The paired Node, as the app shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PairedNode {
    pub name: String,
    /// `host:port`.
    pub address: String,
    /// Unix milliseconds.
    pub paired_at: i64,
    pub last_reached_at: Option<i64>,
}

/// The Node's status.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NodeInfo {
    pub name: String,
    pub version: String,
    pub secret_store_label: String,
    pub protected_at_rest: bool,
    pub devices: u32,
    /// The person chose to accept connections from outside the Node's network (#308).
    /// `None` from an older Node that doesn't say: the app doesn't show it.
    pub reachable_from_outside: Option<bool>,
}

/// A held call, with everything the review shows (ADR-0019 §4).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HeldCall {
    pub id: String,
    pub plugin: String,
    pub job: String,
    pub tool: String,
    /// In plain words; shown as plain text.
    pub summary: String,
    /// The exact arguments (JSON text), shown in full.
    pub arguments: String,
    pub arguments_sha256: String,
    /// Unix seconds.
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

/// What a pairing link holds, before connecting.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LinkPreview {
    /// `None` for a Node reachable only through a Relay.
    pub address: Option<String>,
    pub through_relay: bool,
    /// Unix seconds.
    pub expires_at: u64,
}

fn now_secs() -> u64 {
    (dn_store::now_ms() / 1000) as u64
}

/// The words the app shows for `error`. Swift's own description of the enum is for
/// debugging; this is the same text as on the desktop.
#[uniffi::export]
pub fn error_message(error: MobileError) -> String {
    error.to_string()
}

/// Reads a pairing link (from the camera or pasted) without connecting.
#[uniffi::export]
pub fn read_link(text: String) -> Result<LinkPreview, MobileError> {
    let link = PairingLink::parse(&text, now_secs())?;
    Ok(LinkPreview {
        address: link.address.clone(),
        through_relay: link.relay.is_some(),
        expires_at: link.expires_at,
    })
}

struct Pending {
    join: JoinPending<TcpStream>,
    link: PairingLink,
    address: String,
}

/// This phone: its keys, its small store, and a pairing in progress.
#[derive(uniffi::Object)]
pub struct Phone {
    keys: DeviceKeys,
    db: Mutex<Connection>,
    store: Keychain,
    pending: tokio::sync::Mutex<Option<Pending>>,
}

fn lock(db: &Mutex<Connection>) -> std::sync::MutexGuard<'_, Connection> {
    db.lock().unwrap_or_else(|p| p.into_inner())
}

async fn open_tcp(address: &str) -> Result<TcpStream, MobileError> {
    let unreachable = |reason: String| MobileError::Unreachable {
        address: address.to_owned(),
        reason,
    };
    match tokio::time::timeout(
        Duration::from_secs(CONNECT_SECS),
        TcpStream::connect(address),
    )
    .await
    {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(unreachable(format!(
            "{e}. Check that the Node is running and that this phone is on its network."
        ))),
        Err(_) => Err(unreachable(
            "it didn't answer. Check that the Node is running and that this phone is on its \
             network."
                .into(),
        )),
    }
}

async fn timed<T>(
    f: impl std::future::Future<Output = Result<T, SyncError>>,
) -> Result<T, SyncError> {
    tokio::time::timeout(Duration::from_secs(CONNECT_SECS), f)
        .await
        .unwrap_or(Err(SyncError::Io(std::io::ErrorKind::TimedOut.into())))
}

impl Phone {
    fn link(&self) -> Result<NodeLink, MobileError> {
        link::get(&lock(&self.db))?.ok_or(MobileError::NotPaired)
    }

    async fn member(&self) -> Result<(NodeLink, Channel<TcpStream>), MobileError> {
        let node = self.link()?;
        let stream = open_tcp(&node.address).await?;
        let ch = timed(transport::connect(
            stream,
            &self.keys,
            &node.node.keys.ed25519,
        ))
        .await
        .map_err(|e| match e {
            SyncError::Io(io) => MobileError::Unreachable {
                address: node.address.clone(),
                reason: io.to_string(),
            },
            other => other.into(),
        })?;
        Ok((node, ch))
    }

    async fn ask(&self, message: Message) -> Result<Message, MobileError> {
        let (node, mut ch) = self.member().await?;
        ch.send(&message).await?;
        let reply = timed(ch.recv()).await?;
        let _ = ch.close().await;
        link::reached(&lock(&self.db), &node.node.name, dn_store::now_ms())?;
        match reply {
            Message::Refused { reason } => Err(MobileError::Refused { reason }),
            other => Ok(other),
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Phone {
    /// Opens this phone's store at `db_path` and its keys in the Keychain, making the keys
    /// once on first use. An unreadable Keychain is an error, never new keys.
    #[uniffi::constructor]
    pub fn open(store: Arc<dyn KeyStore>, db_path: String) -> Result<Arc<Self>, MobileError> {
        let store = Keychain(store);
        let keys = DeviceKeys::load_or_create(&store)?;
        let db = dn_store::open(
            std::path::Path::new(&db_path),
            &[MigrationSet::new("sync", dn_sync::migrations())],
        )
        .map_err(|e| MobileError::Storage {
            reason: e.to_string(),
        })?;
        Ok(Arc::new(Self {
            keys,
            db: Mutex::new(db),
            store,
            pending: tokio::sync::Mutex::new(None),
        }))
    }

    /// This phone's device id (hex), as the Node's device list shows it.
    pub fn device_id(&self) -> String {
        self.keys.id().to_hex()
    }

    /// The paired Node, if any.
    pub fn paired_node(&self) -> Result<Option<PairedNode>, MobileError> {
        Ok(link::get(&lock(&self.db))?.map(|l| PairedNode {
            name: l.node.name,
            address: l.address,
            paired_at: l.paired_at,
            last_reached_at: l.last_reached_at,
        }))
    }

    /// Starts pairing with the Node in `link_text`: connects pinned to the Node's key from
    /// the link, proves it has the code, and returns the check numbers (three groups of
    /// four digits) to compare with the Node.
    pub async fn start_pairing(
        &self,
        link_text: String,
        phone_name: String,
    ) -> Result<Vec<String>, MobileError> {
        let link = PairingLink::parse(&link_text, now_secs())?;
        let Some(address) = link.address.clone() else {
            return Err(MobileError::RelayOnly);
        };
        if link::get(&lock(&self.db))?.is_some() {
            return Err(MobileError::AlreadyPaired);
        }
        let mut pending = self.pending.lock().await;
        if let Some(old) = pending.take() {
            let _ = old.join.cancel().await;
        }
        let stream = open_tcp(&address).await?;
        let join = timed(transport::join(
            stream,
            &self.keys,
            &link,
            &phone_name,
            DeviceKind::Phone,
        ))
        .await?;
        let numbers = join.numbers.groups().to_vec();
        *pending = Some(Pending {
            join,
            link,
            address,
        });
        Ok(numbers)
    }

    /// Waits while the person confirms on the Node (no time limit: they may be reading the
    /// numbers), then records the Node.
    pub async fn finish_pairing(&self) -> Result<PairedNode, MobileError> {
        let Some(Pending {
            join,
            link,
            address,
        }) = self.pending.lock().await.take()
        else {
            return Err(MobileError::NoPairing);
        };
        let ch = join.answer().await?;
        let _ = ch.close().await;
        link::save(
            &lock(&self.db),
            &link.node,
            "Your Node",
            &address,
            dn_store::now_ms(),
        )?;
        // The Node's own name comes with its status; the phone works without it.
        if let Ok(info) = self.status().await {
            let _ = link::reached(&lock(&self.db), &info.name, dn_store::now_ms());
        }
        self.paired_node()?.ok_or(MobileError::NotPaired)
    }

    /// Stops a pairing in progress (the numbers didn't match). Nothing is recorded.
    pub async fn cancel_pairing(&self) {
        if let Some(p) = self.pending.lock().await.take() {
            let _ = p.join.cancel().await;
        }
    }

    /// The Node's status, asked now.
    pub async fn status(&self) -> Result<NodeInfo, MobileError> {
        match self.ask(Message::StatusRequest).await? {
            Message::Status(s) => Ok(NodeInfo {
                name: s.name,
                version: s.version,
                secret_store_label: s.secret_store_label,
                protected_at_rest: s.protected_at_rest,
                devices: s.devices,
                reachable_from_outside: s.reachable_from_outside,
            }),
            _ => Err(MobileError::Refused {
                reason: "the Node answered something else".into(),
            }),
        }
    }

    /// The held calls waiting for a person, oldest first, asked now.
    pub async fn waiting(&self) -> Result<Vec<HeldCall>, MobileError> {
        match self.ask(Message::HeldRequest).await? {
            Message::Held { items } => Ok(items
                .into_iter()
                .map(|h| HeldCall {
                    id: h.id,
                    plugin: h.plugin,
                    job: h.job,
                    tool: h.tool,
                    summary: h.summary,
                    arguments: h.arguments,
                    arguments_sha256: h.arguments_sha256,
                    created_at: h.created_at,
                    expires_at: h.expires_at,
                })
                .collect()),
            _ => Err(MobileError::Refused {
                reason: "the Node answered something else".into(),
            }),
        }
    }

    /// Unpairs: asks the Node to remove this phone when it can be reached, then forgets the
    /// Node here and deletes this phone's keys. Returns whether the Node confirmed; the
    /// local part happens either way.
    pub async fn unpair(&self) -> Result<bool, MobileError> {
        let id = self.keys.id().to_hex();
        let confirmed = matches!(
            self.ask(Message::RemoveDevice { id }).await,
            Ok(Message::Done)
        );
        link::forget(&lock(&self.db))?;
        self.store
            .delete(&dn_sync::identity::secret_id())
            .map_err(|e| MobileError::Keychain {
                reason: e.to_string(),
            })?;
        Ok(confirmed)
    }
}

#[cfg(test)]
mod tests;
