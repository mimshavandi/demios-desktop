//! Settings › Node: pairing this computer with a Demios Node, and the Node's status and
//! devices (ADR-0014 §2; design: NodeSettings and PairDevice in the design system).
//!
//! - **This device's keys** live in the system keychain (`device/identity`). If the
//!   keychain can't be read, Settings › Node says so with the keychain message (#200). It
//!   never makes new keys in that case.
//! - **Joining** takes the link from `dn node pair`:
//!   1. connect pinned to the Node's key from the link;
//!   2. prove we have the code on this connection;
//!   3. show the check numbers;
//!   4. wait while the person confirms on the Node.
//! - **The overview** connects to the Node on demand and asks for its status and devices.
//!   When the Node doesn't answer, it says since when.
//!
//! "Pair another device" from this computer needs the Node to relay a second device's
//! check numbers here; that's #269. Until then the screen points to `dn node pair`.

use crate::error::{IpcError, IpcResult};
use crate::state::AppState;
use dn_sync::SyncError;
use dn_sync::devices::DeviceKind;
use dn_sync::identity::DeviceKeys;
use dn_sync::link::{self, NodeLink};
use dn_sync::pairing::PairingLink;
use dn_sync::transport::{self, Channel, DeviceInfo, JoinPending, Message, NodeStatus};
use serde::Serialize;
use std::time::Duration;
use tokio::net::TcpStream;

/// How long reaching the Node may take before it counts as not answering. No person waits
/// on a prompt here.
const CONNECT_SECS: u64 = 10;

/// A join waiting for the person to confirm on the Node.
pub struct PendingJoin {
    join: JoinPending<TcpStream>,
    link: PairingLink,
}

#[derive(Default)]
pub struct NodeState {
    pending: tokio::sync::Mutex<Option<PendingJoin>>,
    /// Sync with the paired Node (crate::node_sync).
    pub sync: crate::node_sync::SyncControl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairedNode {
    pub name: String,
    pub address: String,
    /// Unix milliseconds.
    pub paired_at: i64,
    pub last_reached_at: Option<i64>,
}

/// What Settings › Node shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodeOverview {
    /// `None`: no Node paired.
    pub paired: Option<PairedNode>,
    /// The Node answered just now.
    pub online: bool,
    pub status: Option<NodeStatus>,
    pub devices: Vec<DeviceInfo>,
    /// Why the Node couldn't be used, in the user's words (version, removed, wrong key,
    /// keychain). Offline alone isn't a problem: `online` is false and the screen says since
    /// when.
    pub problem: Option<String>,
    /// Where sync stands.
    pub sync: crate::node_sync::SyncStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JoinStarted {
    /// Three groups of four digits.
    pub numbers: [String; 3],
    pub address: String,
}

fn keys(state: &AppState) -> IpcResult<DeviceKeys> {
    DeviceKeys::load_or_create(&*state.keys.store).map_err(|e| match e {
        SyncError::Secrets(e) => {
            let status = crate::secrets::KeychainStatus::from_error(&e);
            status
                .error()
                .unwrap_or_else(|| IpcError::new("key_store", e.to_string()))
        }
        SyncError::DamagedKeys(what) => IpcError::new(
            "device_keys_damaged",
            format!(
                "This computer's pairing keys in the system keychain are damaged ({what}). \
                 Remove the entry \"device/identity\" from the keychain, then pair again."
            ),
        ),
        other => IpcError::new("device_keys", other.to_string()),
    })
}

/// This computer's name on the Node's device list.
fn device_name() -> String {
    sysinfo::System::host_name()
        .map(|n| n.trim().trim_end_matches(".local").to_owned())
        .filter(|n| !n.is_empty() && n.chars().count() <= 64)
        .unwrap_or_else(|| "This computer".to_owned())
}

fn link_problem(e: &SyncError) -> IpcError {
    let msg = match e {
        SyncError::OfferClosed => {
            "This link has expired or was already used. Run dn node pair again for a new one."
                .to_owned()
        }
        SyncError::NewerLinkVersion => {
            "This link is from a newer Demios. Update this app, then pair.".to_owned()
        }
        SyncError::BadLink(why) => format!("That isn't a Demios pairing link: {why}."),
        other => other.to_string(),
    };
    IpcError::new("node_link", msg)
}

fn connect_problem(address: &str, e: &SyncError) -> IpcError {
    match e {
        SyncError::NodeKeyMismatch => IpcError::new(
            "node_key_mismatch",
            "Pairing stopped. The device at that address isn't the Node from your link. \
             Someone may be between them. Get a new link from the Node itself.",
        ),
        SyncError::VersionMismatch(m) => IpcError::new("node_version", m.to_string()),
        SyncError::Declined => {
            IpcError::new("node_declined", "The Node didn't accept this device.")
        }
        _ => IpcError::new(
            "node_unreachable",
            format!(
                "Couldn't reach {address}. Check that the Node is running and that this \
                 device is on the same network or VPN."
            ),
        ),
    }
}

async fn open(address: &str) -> Result<TcpStream, SyncError> {
    match tokio::time::timeout(
        Duration::from_secs(CONNECT_SECS),
        TcpStream::connect(address),
    )
    .await
    {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(SyncError::Io(e)),
        Err(_) => Err(SyncError::Io(std::io::ErrorKind::TimedOut.into())),
    }
}

pub(crate) async fn connect_member(
    keys: &DeviceKeys,
    link: &NodeLink,
) -> Result<Channel<TcpStream>, SyncError> {
    let stream = open(&link.address).await?;
    tokio::time::timeout(
        Duration::from_secs(CONNECT_SECS),
        transport::connect(stream, keys, &link.node.keys.ed25519),
    )
    .await
    .unwrap_or(Err(SyncError::Io(std::io::ErrorKind::TimedOut.into())))
}

async fn ask(ch: &mut Channel<TcpStream>, message: &Message) -> Result<Message, SyncError> {
    ch.send(message).await?;
    tokio::time::timeout(Duration::from_secs(CONNECT_SECS), ch.recv())
        .await
        .unwrap_or(Err(SyncError::Io(std::io::ErrorKind::TimedOut.into())))
}

fn paired_view(link: &NodeLink) -> PairedNode {
    PairedNode {
        name: link.node.name.clone(),
        address: link.address.clone(),
        paired_at: link.paired_at,
        last_reached_at: link.last_reached_at,
    }
}

/// The Node's status and devices, asked now.
pub async fn overview(state: &std::sync::Arc<AppState>) -> IpcResult<NodeOverview> {
    let st = state.clone();
    let loaded = tauri::async_runtime::spawn_blocking(move || -> IpcResult<_> {
        let link = link::get(&*st.db()?).map_err(|e| IpcError::new("node", e.to_string()))?;
        match link {
            None => Ok(None),
            Some(link) => Ok(Some((keys(&st)?, link))),
        }
    })
    .await
    .map_err(|e| IpcError::new("node", e.to_string()))?;
    let (keys, link) = match loaded {
        Ok(Some(found)) => found,
        Ok(None) => {
            return Ok(NodeOverview {
                paired: None,
                online: false,
                status: None,
                devices: Vec::new(),
                problem: None,
                sync: state.node.sync.status(),
            });
        }
        Err(e) => return Err(e),
    };
    let mut view = NodeOverview {
        paired: Some(paired_view(&link)),
        online: false,
        status: None,
        devices: Vec::new(),
        problem: None,
        sync: state.node.sync.status(),
    };
    let mut ch = match connect_member(&keys, &link).await {
        Ok(ch) => ch,
        Err(e @ (SyncError::NodeKeyMismatch | SyncError::VersionMismatch(_))) => {
            view.problem = Some(connect_problem(&link.address, &e).message);
            return Ok(view);
        }
        Err(_) => return Ok(view),
    };
    match (
        ask(&mut ch, &Message::StatusRequest).await,
        ask(&mut ch, &Message::DevicesRequest).await,
    ) {
        (Ok(Message::Status(status)), Ok(Message::Devices { devices })) => {
            let _ = ch.close().await;
            let name = status.name.clone();
            let db_state = state.clone();
            // Best kept, not essential: the last-reached time and the Node's current name.
            if let Err(e) = tauri::async_runtime::spawn_blocking(move || {
                db_state.db().map_err(|e| e.message).and_then(|db| {
                    link::reached(&db, &name, dn_store::now_ms()).map_err(|e| e.to_string())
                })
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
            {
                eprintln!("couldn't record that the Node answered: {e}");
            }
            view.online = true;
            if let Some(p) = view.paired.as_mut() {
                p.name = status.name.clone();
            }
            view.status = Some(status);
            view.devices = devices;
        }
        // The Node says why it refuses this computer (it was removed).
        (Ok(Message::Refused { reason }), _) => {
            view.problem = Some(format!(
                "Your Node refused this computer: {reason}. Forget the Node here, then pair \
                 again."
            ));
        }
        // The Node closed right after the handshake: it most likely removed this device.
        (Err(SyncError::Io(_)), _) => {
            view.problem = Some(
                "Your Node closed the connection; it may have removed this device. If so, \
                 forget it here, then pair again."
                    .into(),
            );
        }
        _ => {
            view.problem = Some("Your Node sent an answer this app doesn't understand.".into());
        }
    }
    Ok(view)
}

/// Starts joining the Node in `link`: returns the check numbers to compare.
pub async fn join_begin(
    state: &std::sync::Arc<AppState>,
    node: &NodeState,
    link_text: &str,
) -> IpcResult<JoinStarted> {
    let now = (dn_store::now_ms() / 1000) as u64;
    let link = PairingLink::parse(link_text, now).map_err(|e| link_problem(&e))?;
    let st = state.clone();
    let keys = tauri::async_runtime::spawn_blocking(move || -> IpcResult<DeviceKeys> {
        if link::get(&*st.db()?)
            .map_err(|e| IpcError::new("node", e.to_string()))?
            .is_some()
        {
            return Err(IpcError::new(
                "node_already_paired",
                "This computer is already paired with a Node. Unpair it first.",
            ));
        }
        keys(&st)
    })
    .await
    .map_err(|e| IpcError::new("node", e.to_string()))??;
    let mut pending = node.pending.lock().await;
    if let Some(old) = pending.take() {
        let _ = old.join.cancel().await;
    }
    let stream = open(&link.address)
        .await
        .map_err(|e| connect_problem(&link.address, &e))?;
    let join = tokio::time::timeout(
        Duration::from_secs(CONNECT_SECS),
        transport::join(stream, &keys, &link, &device_name(), DeviceKind::Desktop),
    )
    .await
    .unwrap_or(Err(SyncError::Io(std::io::ErrorKind::TimedOut.into())))
    .map_err(|e| connect_problem(&link.address, &e))?;
    let started = JoinStarted {
        numbers: join.numbers.groups(),
        address: link.address.clone(),
    };
    *pending = Some(PendingJoin { join, link });
    Ok(started)
}

/// Waits for the person to confirm on the Node (no time limit: they may be reading the
/// numbers), then records the Node.
pub async fn join_wait(
    state: &std::sync::Arc<AppState>,
    node: &NodeState,
) -> IpcResult<NodeOverview> {
    let Some(PendingJoin { join, link }) = node.pending.lock().await.take() else {
        return Err(IpcError::new("node_no_join", "No pairing is waiting."));
    };
    let ch = join
        .answer()
        .await
        .map_err(|e| connect_problem(&link.address, &e))?;
    let _ = ch.close().await;
    let st = state.clone();
    tauri::async_runtime::spawn_blocking(move || -> IpcResult<()> {
        link::save(
            &*st.db()?,
            &link.node,
            "Your Node",
            &link.address,
            dn_store::now_ms(),
        )
        .map(|_| ())
        .map_err(|e| IpcError::new("node", e.to_string()))
    })
    .await
    .map_err(|e| IpcError::new("node", e.to_string()))??;
    // Paired: sync starts (the first round seeds what's already here).
    crate::node_sync::start(state);
    overview(state).await
}

/// "Sync now" in Settings › Node.
pub async fn sync_now(state: &std::sync::Arc<AppState>) -> IpcResult<NodeOverview> {
    crate::node_sync::sync_now(state);
    overview(state).await
}

/// "They don't match" or Cancel: stops the join; nothing is added.
pub async fn join_cancel(node: &NodeState) {
    if let Some(p) = node.pending.lock().await.take() {
        let _ = p.join.cancel().await;
    }
}

async fn member_request(
    state: &std::sync::Arc<AppState>,
    message: Message,
) -> IpcResult<(DeviceKeys, Message)> {
    let st = state.clone();
    let (keys, link) = tauri::async_runtime::spawn_blocking(move || -> IpcResult<_> {
        let link = link::get(&*st.db()?)
            .map_err(|e| IpcError::new("node", e.to_string()))?
            .ok_or_else(|| {
                IpcError::new("node_not_paired", "This computer isn't paired with a Node.")
            })?;
        Ok((keys(&st)?, link))
    })
    .await
    .map_err(|e| IpcError::new("node", e.to_string()))??;
    let mut ch = connect_member(&keys, &link)
        .await
        .map_err(|e| connect_problem(&link.address, &e))?;
    let reply = ask(&mut ch, &message)
        .await
        .map_err(|e| connect_problem(&link.address, &e))?;
    let _ = ch.close().await;
    Ok((keys, reply))
}

/// Removes another device from the Node. It can't connect or sync again.
pub async fn remove_device(state: &std::sync::Arc<AppState>, id: &str) -> IpcResult<NodeOverview> {
    match member_request(state, Message::RemoveDevice { id: id.to_owned() })
        .await?
        .1
    {
        Message::Done => overview(state).await,
        Message::Refused { reason } => Err(IpcError::new("node_refused", reason)),
        _ => Err(IpcError::new(
            "node",
            "Your Node sent an answer this app doesn't understand.",
        )),
    }
}

/// Forgets the Node on this computer only, for a Node that is gone or has already removed
/// this computer. The Node keeps listing this computer until it's removed there; the
/// confirmation says so.
pub async fn forget(state: &std::sync::Arc<AppState>) -> IpcResult<NodeOverview> {
    join_cancel(&state.node).await;
    crate::node_sync::stop_and_wait(state).await;
    let st = state.clone();
    tauri::async_runtime::spawn_blocking(move || -> IpcResult<()> {
        crate::node_sync::leave(&st).map_err(|e| IpcError::new("node_sync", e))?;
        link::forget(&*st.db()?).map_err(|e| IpcError::new("node", e.to_string()))
    })
    .await
    .map_err(|e| IpcError::new("node", e.to_string()))??;
    overview(state).await
}

/// Unpairs this computer: the Node removes it, then this computer forgets the Node. If the
/// Node can't be reached, nothing changes and the error says so.
pub async fn unpair(state: &std::sync::Arc<AppState>) -> IpcResult<NodeOverview> {
    let st = state.clone();
    let me = tauri::async_runtime::spawn_blocking(move || keys(&st).map(|k| k.id()))
        .await
        .map_err(|e| IpcError::new("node", e.to_string()))??;
    match member_request(state, Message::RemoveDevice { id: me.to_hex() })
        .await?
        .1
    {
        Message::Done => {}
        Message::Refused { reason } => return Err(IpcError::new("node_refused", reason)),
        _ => {
            return Err(IpcError::new(
                "node",
                "Your Node sent an answer this app doesn't understand.",
            ));
        }
    }
    crate::node_sync::stop_and_wait(state).await;
    let st = state.clone();
    tauri::async_runtime::spawn_blocking(move || -> IpcResult<()> {
        crate::node_sync::leave(&st).map_err(|e| IpcError::new("node_sync", e))?;
        link::forget(&*st.db()?).map_err(|e| IpcError::new("node", e.to_string()))
    })
    .await
    .map_err(|e| IpcError::new("node", e.to_string()))??;
    overview(state).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_store::MigrationSet;
    use dn_sync::pairing::PairingOffer;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    /// A minimal Node: the first connection pairs (answering `confirm`), later ones answer
    /// the status and the device list. Returns the pairing link.
    async fn fake_node(confirm: bool) -> (String, tokio::task::JoinHandle<()>) {
        let keys = Arc::new(DeviceKeys::generate().unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let mut offer =
            PairingOffer::new(&addr, keys.public(), (dn_store::now_ms() / 1000) as u64).unwrap();
        let link = offer.link().to_url().to_string();
        let db = Arc::new(Mutex::new(
            dn_store::open_in_memory(&[MigrationSet::new("sync", dn_sync::migrations())]).unwrap(),
        ));
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ch = transport::accept(stream, &keys).await.unwrap();
            let now = || (dn_store::now_ms() / 1000) as u64;
            let Ok(req) = transport::read_join(&db, &mut ch, &mut offer, now).await else {
                return;
            };
            transport::finish_join(&db, &mut ch, &req, None, confirm, 2)
                .await
                .unwrap();
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let Ok(mut ch) = transport::accept(stream, &keys).await else {
                    continue;
                };
                while let Ok(m) = ch.recv().await {
                    let reply = match m {
                        Message::StatusRequest => Message::Status(NodeStatus {
                            name: "Home server".into(),
                            version: "1.4.0".into(),
                            secret_store: "systemd_credential".into(),
                            secret_store_label: "Encrypted file, key from a systemd credential"
                                .into(),
                            protected_at_rest: true,
                            devices: 2,
                        }),
                        Message::DevicesRequest => Message::Devices {
                            devices: Vec::new(),
                        },
                        _ => Message::Done,
                    };
                    if ch.send(&reply).await.is_err() {
                        break;
                    }
                }
            }
        });
        (link, task)
    }

    fn state() -> Arc<AppState> {
        Arc::new(AppState::in_memory().unwrap())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn joining_shows_numbers_then_records_the_node_and_reads_its_status() {
        let (link, _node) = fake_node(true).await;
        let st = state();
        let started = join_begin(&st, &st.node, &link).await.unwrap();
        assert_eq!(started.numbers.iter().map(String::len).sum::<usize>(), 12);
        let view = join_wait(&st, &st.node).await.unwrap();
        let paired = view.paired.unwrap();
        assert!(view.online, "{:?}", view.problem);
        assert_eq!(paired.name, "Home server", "renamed from the Node's status");
        assert_eq!(view.status.unwrap().secret_store, "systemd_credential");
        // Already paired: a second join is refused.
        assert_eq!(
            join_begin(&st, &st.node, &link).await.unwrap_err().code,
            "node_already_paired"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_declined_join_records_nothing() {
        let (link, _node) = fake_node(false).await;
        let st = state();
        join_begin(&st, &st.node, &link).await.unwrap();
        assert_eq!(
            join_wait(&st, &st.node).await.unwrap_err().code,
            "node_declined"
        );
        assert!(overview(&st).await.unwrap().paired.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_and_expired_links_say_what_to_do() {
        let st = state();
        let e = join_begin(&st, &st.node, "https://example.com")
            .await
            .unwrap_err();
        assert_eq!(e.code, "node_link");
        let keys = DeviceKeys::generate().unwrap();
        let old = PairingOffer::new("nas:7443", keys.public(), 1).unwrap();
        let e = join_begin(&st, &st.node, &old.link().to_url())
            .await
            .unwrap_err();
        assert!(e.message.contains("dn node pair"), "{}", e.message);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_node_that_does_not_answer_is_offline_not_an_error() {
        let st = state();
        let node = DeviceKeys::generate().unwrap().public();
        link::save(&st.db().unwrap(), &node, "Home server", "127.0.0.1:9", 5).unwrap();
        let view = overview(&st).await.unwrap();
        assert!(!view.online);
        assert!(view.problem.is_none());
        assert_eq!(view.paired.unwrap().last_reached_at, Some(5));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_node_paired_is_an_empty_overview() {
        let view = overview(&state()).await.unwrap();
        assert!(view.paired.is_none() && !view.online && view.devices.is_empty());
    }
}
