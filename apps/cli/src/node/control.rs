//! The Node's local control channel and pairing from the Node (`dn node pair`, ADR-0014 §2).
//!
//! **The channel:** `dn node run` listens on `<data folder>/control.sock`, a Unix socket
//! made owner-only inside the owner-only data folder. It also refuses a client whose user
//! isn't the folder's owner. Windows has no channel yet (#261).
//!
//! **Pairing:**
//! 1. `dn node pair` asks for an offer. The Node makes a one-time code (10 minutes) and
//!    returns the link to show.
//! 2. When a device connects with that code, the Node checks its proof and sends the check
//!    numbers to `dn node pair`. The person compares them with the device's screen and
//!    answers.
//! 3. Only "yes" adds the device. Closing `dn node pair`, Ctrl-C, or the code expiring
//!    ends the pairing, and nothing is added.
//!
//! **Devices** (#309): `dn node devices` lists the paired devices, and
//! `dn node remove-device <id>` removes one, with the same rules as a removal from a paired
//! device: its key is refused for good, and the group key rotates.
//!
//! **Rotating the records key** (`dn node rotate-records-key`, #335): the Node makes a new
//! key for its synced records, re-seals them in batches, reporting how many are left, then
//! deletes the old key and vacuums its database ([`dn_sync::at_rest`]).
//!
//! One pairing at a time. Messages are JSON, one per line, at most [`MAX_LINE`] bytes.

use dn_sync::devices::DeviceKind;
use dn_sync::pairing::{CheckNumbers, PairingOffer};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};

pub const SOCKET_FILE: &str = "control.sock";
pub const MAX_LINE: usize = 64 * 1024;

/// From `dn node pair` to the Node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Start pairing; devices will connect to `address` (`host:port`).
    Pair { address: String },
    /// The person's answer to the check numbers.
    Confirm { accepted: bool },
    /// List the paired devices (`dn node devices`).
    Devices,
    /// Remove a paired device (`dn node remove-device`).
    RemoveDevice { id: String },
    /// Rotate the key the synced records are sealed with.
    RotateRecordsKey,
}

/// One paired device, as `dn node devices` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRow {
    pub id: String,
    pub name: String,
    /// `desktop`, `node` or `phone`.
    pub kind: String,
    /// Unix milliseconds.
    pub last_synced_at: Option<i64>,
}

/// What the control channel needs from the running Node besides pairing.
pub trait NodeOps: Send + Sync {
    fn devices(&self) -> Result<Vec<DeviceRow>, String>;
    /// Removes the device with this id (hex); returns its name.
    fn remove(&self, id: &str) -> Result<String, String>;
}

/// From the Node to `dn node pair`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// The code to show: the link (also the QR payload) and when it expires (Unix seconds).
    Offer {
        link: String,
        expires_at: u64,
    },
    /// A device proved it has the code; compare the numbers.
    Joining {
        name: String,
        kind: String,
        numbers: String,
    },
    Paired {
        name: String,
    },
    Declined,
    Devices {
        devices: Vec<DeviceRow>,
    },
    Removed {
        name: String,
    },
    /// A rotation's progress: records and conflicts still under the old key.
    Resealing {
        key: u32,
        left: u64,
    },
    /// The rotation is done: everything is sealed with `key`, and the old keys are gone.
    Rotated {
        key: u32,
    },
    Error {
        message: String,
    },
}

/// What a rotation needs from the running Node.
#[derive(Clone)]
pub struct Records {
    pub db: Arc<std::sync::Mutex<dn_store::Connection>>,
    pub records: Arc<dn_sync::at_rest::NodeRecords>,
    pub secrets: Arc<dyn dn_secrets::SecretStore>,
}

/// Records and conflicts re-sealed per transaction, so devices aren't kept waiting long.
pub const RESEAL_BATCH: usize = 500;

/// A device that proved it has the code, waiting for the person's answer.
pub struct Joining {
    pub name: String,
    pub kind: DeviceKind,
    pub numbers: CheckNumbers,
    /// The person's answer.
    pub answer: oneshot::Sender<bool>,
    /// How it ended: the device's name, or why it failed.
    pub result: oneshot::Receiver<Result<String, String>>,
}

/// The pairing that is open, shared between the control session and device connections.
pub struct OpenPairing {
    /// Tells this pairing from a later one, so an offer is only ever put back into the
    /// pairing it was taken from.
    pub id: u64,
    /// Taken by the device connection that is checking a proof; put back on a wrong proof.
    pub offer: Option<PairingOffer>,
    pub joining: mpsc::Sender<Joining>,
}

pub type PairingState = Arc<Mutex<Option<OpenPairing>>>;

static NEXT_PAIRING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_pairing_id() -> u64 {
    NEXT_PAIRING.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub fn kind_name(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Desktop => "desktop",
        DeviceKind::Node => "node",
        DeviceKind::Phone => "phone",
    }
}

#[cfg(unix)]
pub use unix::{bind, client_pair, client_request, client_rotate, serve};

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::node::now_secs;
    use dn_sync::identity::PublicIdentity;
    use std::path::Path;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    async fn read_line<T: for<'de> Deserialize<'de>>(
        reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    ) -> Result<Option<T>, String> {
        let mut line = String::new();
        let n = (&mut *reader)
            .take(MAX_LINE as u64 + 1)
            .read_line(&mut line)
            .await
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(None);
        }
        if n > MAX_LINE {
            return Err("control message too large".into());
        }
        serde_json::from_str(line.trim_end())
            .map(Some)
            .map_err(|_| "unreadable control message".into())
    }

    async fn write_line<T: Serialize>(
        writer: &mut tokio::net::unix::OwnedWriteHalf,
        message: &T,
    ) -> Result<(), String> {
        let mut text = serde_json::to_string(message).map_err(|e| e.to_string())?;
        text.push('\n');
        writer
            .write_all(text.as_bytes())
            .await
            .map_err(|e| e.to_string())
    }

    /// Binds the control socket, replacing a stale one left by a crash.
    pub fn bind(data_dir: &Path) -> Result<UnixListener, String> {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        let path = data_dir.join(SOCKET_FILE);
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if !meta.file_type().is_socket() {
                return Err(format!(
                    "{} exists and isn't a socket; remove it",
                    path.display()
                ));
            }
            std::fs::remove_file(&path)
                .map_err(|e| format!("couldn't remove the old {}: {e}", path.display()))?;
        }
        let listener = UnixListener::bind(&path)
            .map_err(|e| format!("couldn't open the control socket {}: {e}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("couldn't protect {}: {e}", path.display()))?;
        Ok(listener)
    }

    fn owner_uid(data_dir: &Path) -> Result<u32, String> {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(data_dir)
            .map(|m| m.uid())
            .map_err(|e| format!("couldn't read {}: {e}", data_dir.display()))
    }

    /// Accepts control clients until the listener is dropped.
    pub async fn serve(
        listener: UnixListener,
        data_dir: std::path::PathBuf,
        node: PublicIdentity,
        pairing: PairingState,
        ops: std::sync::Arc<dyn NodeOps>,
        records: Records,
    ) {
        let Ok(owner) = owner_uid(&data_dir) else {
            eprintln!("dn node: control channel off: can't read the data folder's owner");
            return;
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            match stream.peer_cred() {
                Ok(cred) if cred.uid() == owner => {}
                _ => {
                    eprintln!("dn node: refused a control client that isn't the Node's user");
                    continue;
                }
            }
            let (node, pairing, ops, records) =
                (node, pairing.clone(), ops.clone(), records.clone());
            tokio::spawn(async move {
                if let Err(e) = session(stream, node, pairing, &*ops, records).await {
                    eprintln!("dn node: control: {e}");
                }
            });
        }
    }

    async fn session(
        stream: UnixStream,
        node: PublicIdentity,
        pairing: PairingState,
        ops: &dyn NodeOps,
        records: Records,
    ) -> Result<(), String> {
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let address = match read_line::<Request>(&mut reader).await? {
            Some(Request::Pair { address }) => address,
            Some(Request::Devices) => {
                let reply = match ops.devices() {
                    Ok(devices) => Reply::Devices { devices },
                    Err(message) => Reply::Error { message },
                };
                return write_line(&mut write, &reply).await;
            }
            Some(Request::RemoveDevice { id }) => {
                let reply = match ops.remove(&id) {
                    Ok(name) => Reply::Removed { name },
                    Err(message) => Reply::Error { message },
                };
                return write_line(&mut write, &reply).await;
            }
            Some(Request::RotateRecordsKey) => {
                let reply = match rotate(&records, &mut write).await {
                    Ok(key) => Reply::Rotated { key },
                    Err(message) => Reply::Error { message },
                };
                return write_line(&mut write, &reply).await;
            }
            _ => {
                return write_line(
                    &mut write,
                    &Reply::Error {
                        message: "expected a request".into(),
                    },
                )
                .await;
            }
        };
        let (tx, mut rx) = mpsc::channel(1);
        let expires_at = {
            let mut slot = pairing.lock().await;
            if slot.is_some() {
                return write_line(
                    &mut write,
                    &Reply::Error {
                        message: "a pairing is already open; finish or close it first".into(),
                    },
                )
                .await;
            }
            let offer = match PairingOffer::new(&address, node, now_secs() as u64) {
                Ok(o) => o,
                Err(e) => {
                    return write_line(
                        &mut write,
                        &Reply::Error {
                            message: e.to_string(),
                        },
                    )
                    .await;
                }
            };
            let link = offer.link().to_url().to_string();
            let expires_at = offer.link().expires_at;
            *slot = Some(OpenPairing {
                id: next_pairing_id(),
                offer: Some(offer),
                joining: tx,
            });
            if let Err(e) = write_line(&mut write, &Reply::Offer { link, expires_at }).await {
                // `dn node pair` went away before seeing the code: close it again.
                *slot = None;
                return Err(e);
            }
            expires_at
        };
        let result = pair_once(&mut reader, &mut write, &mut rx, expires_at).await;
        // However it ended, the code is gone.
        *pairing.lock().await = None;
        result
    }

    /// Rotates the records key, a batch at a time, reporting progress. The database is let
    /// go between batches, so devices keep syncing.
    async fn rotate(
        r: &Records,
        write: &mut tokio::net::unix::OwnedWriteHalf,
    ) -> Result<u32, String> {
        let lock = || r.db.lock().unwrap_or_else(|p| p.into_inner());
        let keys = r.records.keys();
        let started = keys
            .rotate(&lock(), &*r.secrets)
            .map_err(|e| format!("couldn't start the rotation: {e}"))?;
        let mut left = started.left;
        loop {
            write_line(
                write,
                &Reply::Resealing {
                    key: started.current,
                    left,
                },
            )
            .await?;
            if left == 0 {
                break;
            }
            left = keys
                .reseal_batch(&lock(), RESEAL_BATCH)
                .map_err(|e| format!("re-sealing stopped: {e}. Run it again to go on"))?;
            tokio::task::yield_now().await;
        }
        // Another rotation may have left something under the old key in between.
        if !keys
            .finish(&lock(), &*r.secrets)
            .map_err(|e| format!("couldn't remove the old key: {e}. Run it again to go on"))?
        {
            return Err("something is still sealed with the old key; run it again".into());
        }
        Ok(keys.current())
    }

    async fn pair_once(
        reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
        write: &mut tokio::net::unix::OwnedWriteHalf,
        rx: &mut mpsc::Receiver<Joining>,
        expires_at: u64,
    ) -> Result<(), String> {
        let left = expires_at.saturating_sub(now_secs() as u64);
        let joining = tokio::select! {
            j = rx.recv() => j.ok_or("the pairing closed")?,
            _ = tokio::time::sleep(Duration::from_secs(left)) => {
                return write_line(write, &Reply::Error {
                    message: "the code expired; run dn node pair again for a new one".into(),
                }).await;
            }
            closed = read_line::<Request>(reader) => {
                // The person closed `dn node pair` (or sent something unexpected).
                let _ = closed;
                return Ok(());
            }
        };
        write_line(
            write,
            &Reply::Joining {
                name: joining.name.clone(),
                kind: kind_name(joining.kind).into(),
                numbers: joining.numbers.to_string(),
            },
        )
        .await?;
        // No time limit: the person is comparing the numbers.
        let accepted = matches!(
            read_line::<Request>(reader).await,
            Ok(Some(Request::Confirm { accepted: true }))
        );
        let _ = joining.answer.send(accepted);
        let reply = match joining.result.await {
            Ok(Ok(name)) => Reply::Paired { name },
            Ok(Err(e)) if accepted => Reply::Error { message: e },
            _ => Reply::Declined,
        };
        write_line(write, &reply).await
    }

    /// One request and its reply, for `dn node devices` and `dn node remove-device`.
    pub async fn client_request(data_dir: &Path, request: &Request) -> Result<Reply, String> {
        let path = data_dir.join(SOCKET_FILE);
        let stream = UnixStream::connect(&path).await.map_err(|e| {
            format!(
                "couldn't reach the running Node at {}: {e}. Is `dn node run` running?",
                path.display()
            )
        })?;
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        write_line(&mut write, request).await?;
        match read_line::<Reply>(&mut reader).await? {
            Some(Reply::Error { message }) => Err(message),
            Some(reply) => Ok(reply),
            None => Err("the Node closed the connection".into()),
        }
    }

    /// `dn node rotate-records-key`: asks the running Node to rotate, showing progress.
    /// Returns the new key's number.
    pub async fn client_rotate(data_dir: &Path, mut show: impl FnMut(&str)) -> Result<u32, String> {
        let path = data_dir.join(SOCKET_FILE);
        let stream = UnixStream::connect(&path).await.map_err(|e| {
            format!(
                "couldn't reach the running Node at {}: {e}. Is `dn node run` running?",
                path.display()
            )
        })?;
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        write_line(&mut write, &Request::RotateRecordsKey).await?;
        loop {
            match read_line::<Reply>(&mut reader)
                .await?
                .ok_or("the Node closed the connection before the rotation finished; run it again to go on")?
            {
                Reply::Resealing { key, left } => {
                    show(&format!("Re-sealing with key {key}: {left} left"));
                }
                Reply::Rotated { key } => return Ok(key),
                Reply::Error { message } => return Err(message),
                _ => return Err("the Node sent an unexpected reply".into()),
            }
        }
    }

    /// `dn node pair`: asks the running Node for a code, shows it, and asks the person to
    /// compare the numbers.
    pub async fn client_pair(
        data_dir: &Path,
        address: String,
        mut ask: impl FnMut(&str, &str, &str) -> bool,
        mut show: impl FnMut(&str),
    ) -> Result<String, String> {
        let path = data_dir.join(SOCKET_FILE);
        let stream = UnixStream::connect(&path).await.map_err(|e| {
            format!(
                "couldn't reach the running Node at {}: {e}. Is `dn node run` running?",
                path.display()
            )
        })?;
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        write_line(&mut write, &Request::Pair { address }).await?;
        loop {
            let reply = read_line::<Reply>(&mut reader)
                .await?
                .ok_or("the Node closed the pairing")?;
            match reply {
                Reply::Offer { link, expires_at } => {
                    let minutes = expires_at.saturating_sub(now_secs() as u64).div_ceil(60);
                    show(&format!(
                        "Open this link on the device you're pairing, or paste it there. \
                         It works once, for {minutes} minutes:\n\n  {link}\n\nWaiting for the device…"
                    ));
                }
                Reply::Joining {
                    name,
                    kind,
                    numbers,
                } => {
                    let accepted = ask(&name, &kind, &numbers);
                    write_line(&mut write, &Request::Confirm { accepted }).await?;
                }
                Reply::Paired { name } => return Ok(name),
                Reply::Declined => {
                    return Err("Pairing stopped. Nothing was added.".into());
                }
                Reply::Error { message } => return Err(message),
                Reply::Devices { .. }
                | Reply::Removed { .. }
                | Reply::Resealing { .. }
                | Reply::Rotated { .. } => {
                    return Err("the Node sent an unexpected reply".into());
                }
            }
        }
    }
}
