//! `dn node`: the Demios Node, the same engine run headless as a service on the user's
//! own machine (ADR-0014).
//!
//! - `dn node init` writes `node.json` in the data folder: the secret store, the address to
//!   listen on, the Node's name and its UTC offset.
//! - `dn node run` starts the Node:
//!   1. It refuses to start with secrets in environment variables (§7).
//!   2. It opens exactly the configured secret store. A passphrase store waits for
//!      `dn node unlock` (#255).
//!   3. It loads or creates the Node's device keys and lists itself as the group's Node.
//!   4. It opens `node.db`, with the sync and jobs tables.
//!   5. It runs the job scheduler (dn-jobs) and accepts paired devices over the paired
//!      channel (dn-sync).
//!
//! Pairing from the Node (`dn node pair`, #254) comes next. Until then, only devices
//! already in the list are accepted.

pub mod config;
pub mod control;

use clap::Subcommand;
use config::{NodeConfig, SecretsConfig, parse_utc_offset, secret_env_vars};
use dn_jobs::JobKey;
use dn_jobs::scheduler::{Clock, Event, JobHost, Scheduler};
use dn_plugin_api::{BoxFuture, JobContext, PluginError};
use dn_secrets::SecretStore;
use dn_secrets::server::{self, StoreReport};
use dn_store::{Connection, MigrationSet};
use dn_sync::SyncError;
use dn_sync::devices::{DeviceKind, DeviceList};
use dn_sync::identity::DeviceKeys;
use dn_sync::transport::{self, Message, NodeStatus};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore};

/// A device that hasn't finished the TLS handshake and the version exchange by then is
/// dropped. No person waits on this step.
const HANDSHAKE_SECS: u64 = 10;
/// A paired device that sends nothing for this long is disconnected; it reconnects.
const IDLE_SECS: u64 = 15 * 60;
/// An unpaired device must send its pairing request within this time after the handshake.
/// No person waits on this step (the check numbers come after it).
const JOIN_SECS: u64 = 30;
/// Connections handled at once; more wait for a slot.
const MAX_CONNECTIONS: usize = 64;

#[derive(Subcommand)]
pub enum NodeCmd {
    /// Set up a Node in a data folder: its name, address, secret store and time offset.
    Init {
        /// The Node's data folder (created owner-only).
        #[arg(long)]
        data_dir: PathBuf,
        /// The Node's name on your devices.
        #[arg(long)]
        name: String,
        /// Address to accept devices on.
        #[arg(long, default_value = "0.0.0.0:7443")]
        listen: SocketAddr,
        /// Where secrets live: keychain, systemd-credential:<name>, container-secret:<name>,
        /// key-file:<path>, passphrase or insecure-plaintext (test setups only).
        #[arg(long)]
        secrets: String,
        /// Local time offset for jobs at a time of day, e.g. +01:00.
        #[arg(long, default_value = "0", allow_hyphen_values = true)]
        utc_offset: String,
    },
    /// Run the Node in the foreground, as its service manager starts it.
    Run {
        #[arg(long)]
        data_dir: PathBuf,
    },
    /// Pair a device with the running Node: shows a one-time link, then asks you to
    /// compare the check numbers with the device.
    Pair {
        #[arg(long)]
        data_dir: PathBuf,
        /// The address devices use to reach this Node (`host:port`). Defaults to the
        /// listen address when that is a specific address.
        #[arg(long)]
        address: Option<String>,
    },
}

pub async fn command(cmd: NodeCmd) -> Result<(), String> {
    match cmd {
        NodeCmd::Init {
            data_dir,
            name,
            listen,
            secrets,
            utc_offset,
        } => {
            let config = NodeConfig::new(
                name,
                listen,
                SecretsConfig::parse(&secrets)?,
                parse_utc_offset(&utc_offset)?,
            )?;
            config.create(&data_dir)?;
            eprintln!(
                "Node set up in {}. Start it with `dn node run --data-dir {}`.",
                data_dir.display(),
                data_dir.display()
            );
            Ok(())
        }
        NodeCmd::Run { data_dir } => {
            let node =
                Node::start(&data_dir, std::env::vars_os(), &|k| std::env::var_os(k)).await?;
            let stop = Arc::new(Notify::new());
            let on_signal = {
                let stop = stop.clone();
                async move {
                    shutdown_signal().await;
                    eprintln!("dn node: stopping");
                    stop.notify_waiters();
                }
            };
            tokio::spawn(on_signal);
            node.serve(stop).await
        }
        NodeCmd::Pair { data_dir, address } => pair_command(&data_dir, address).await,
    }
}

/// The address devices should use, from `--address` or a specific listen address.
fn pair_address(config: &NodeConfig, address: Option<String>) -> Result<String, String> {
    match address {
        Some(a) => Ok(a),
        None if !config.listen.ip().is_unspecified() => Ok(config.listen.to_string()),
        None => Err(format!(
            "the Node listens on all addresses ({}); say which one devices use, e.g. \
             --address 192.168.1.20:{} or --address node.example.net:{}",
            config.listen,
            config.listen.port(),
            config.listen.port()
        )),
    }
}

#[cfg(unix)]
async fn pair_command(data_dir: &Path, address: Option<String>) -> Result<(), String> {
    let config = NodeConfig::load(data_dir)?;
    let address = pair_address(&config, address)?;
    let ask = |name: &str, kind: &str, numbers: &str| -> bool {
        eprintln!(
            "\n{name} ({kind}) wants to join.\nCheck numbers: {numbers}\n\
             Do they match the numbers on the device? Type yes to add it: "
        );
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("yes")
    };
    let name = control::client_pair(data_dir, address, ask, |text| eprintln!("{text}")).await?;
    eprintln!("Paired with {name}.");
    Ok(())
}

#[cfg(not(unix))]
async fn pair_command(_data_dir: &Path, _address: Option<String>) -> Result<(), String> {
    Err("dn node pair isn't available on Windows yet (#261)".into())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn now_secs() -> i64 {
    dn_store::now_ms() / 1000
}

/// A fixed local offset (#257 adds the time zone with daylight saving).
struct FixedOffsetClock(i64);

impl Clock for FixedOffsetClock {
    fn now(&self) -> i64 {
        now_secs()
    }
    fn utc_offset_secs(&self, _at: i64) -> i64 {
        self.0
    }
}

/// This build registers no add-ons (ADR-0013: the open-source build has none), so the
/// scheduler has no jobs to run. It still recovers runs and held calls a crash left
/// behind.
struct NoAddOns;

impl JobHost for NoAddOns {
    fn run_job<'a>(
        &'a self,
        job: &'a JobKey,
        _ctx: &'a JobContext,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        let message = format!("{job} belongs to an add-on this build doesn't have");
        Box::pin(async move { Err(PluginError::unavailable(message)) })
    }

    fn changed(&self, event: Event) {
        eprintln!("dn node: job {event:?}");
    }
}

/// A started Node, before it serves.
pub struct Node {
    data_dir: PathBuf,
    pairing: control::PairingState,
    #[cfg(unix)]
    control: tokio::net::UnixListener,
    keys: Arc<DeviceKeys>,
    db: Arc<Mutex<Connection>>,
    store: StoreReport,
    name: String,
    listener: TcpListener,
    scheduler: Arc<Scheduler>,
}

fn lock(db: &Mutex<Connection>) -> std::sync::MutexGuard<'_, Connection> {
    db.lock().unwrap_or_else(|p| p.into_inner())
}

impl Node {
    /// Everything up to listening. Fails with the reason, and nothing is replaced or
    /// fallen back to.
    pub async fn start(
        data_dir: &Path,
        vars: impl IntoIterator<Item = (OsString, OsString)>,
        env: server::Env<'_>,
    ) -> Result<Self, String> {
        let found = secret_env_vars(vars);
        if !found.is_empty() {
            return Err(format!(
                "secrets in environment variables aren't allowed on a Node: {}. \
                 Unset them; the Node keeps keys in its secret store",
                found.join(", ")
            ));
        }
        config::check_private_dir(data_dir)?;
        let config = NodeConfig::load(data_dir)?;
        let store = server::open(&config.secrets.choice(), data_dir, env)
            .map_err(|e| format!("couldn't open the secret store: {e}"))?;
        let report = store.report();
        if report.locked {
            return Err(
                "the secret store is protected by a passphrase, and unlocking a running Node \
                 isn't available yet (#255). Choose another secret store with `dn node init`"
                    .into(),
            );
        }
        if !report.protected_at_rest {
            eprintln!(
                "dn node: WARNING: secrets are stored unencrypted ({}). Use this for tests only",
                report.label
            );
        }
        let store_ref: &dyn SecretStore = &*store;
        let keys = DeviceKeys::load_or_create(store_ref)
            .map_err(|e| format!("couldn't load this Node's keys: {e}"))?;
        let conn = dn_store::open(
            &data_dir.join(config::DATABASE_FILE),
            &[
                MigrationSet::new("sync", dn_sync::migrations()),
                MigrationSet::new("jobs", dn_jobs::migrations()),
            ],
        )
        .map_err(|e| format!("couldn't open the Node's database: {e}"))?;
        match DeviceList::by_signing_key(&conn, &keys.public().ed25519) {
            Ok(Some(me)) if me.is_member() => {}
            Ok(Some(_)) => return Err("this Node's own keys are marked removed".into()),
            Ok(None) => {
                DeviceList::add(
                    &conn,
                    &keys.public(),
                    &config.name,
                    DeviceKind::Node,
                    None,
                    dn_store::now_ms(),
                )
                .map_err(|e| format!("couldn't list this Node as a device: {e}"))?;
            }
            Err(e) => return Err(format!("couldn't read the device list: {e}")),
        }
        let listener = TcpListener::bind(config.listen)
            .await
            .map_err(|e| format!("couldn't listen on {}: {e}", config.listen))?;
        #[cfg(unix)]
        let control = control::bind(data_dir)?;
        let db = Arc::new(Mutex::new(conn));
        let scheduler = Arc::new(Scheduler::new(
            Arc::new(NoAddOns),
            Vec::new(),
            db.clone(),
            Arc::new(FixedOffsetClock(i64::from(config.utc_offset_minutes) * 60)),
        ));
        eprintln!(
            "dn node: {} ({}) on {}, secrets: {}",
            config.name,
            keys.id(),
            listener.local_addr().map_err(|e| e.to_string())?,
            report.label
        );
        Ok(Self {
            data_dir: data_dir.to_owned(),
            pairing: Arc::new(tokio::sync::Mutex::new(None)),
            #[cfg(unix)]
            control,
            keys: Arc::new(keys),
            db,
            store: report,
            name: config.name,
            listener,
            scheduler,
        })
    }

    #[cfg(test)]
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    #[cfg(test)]
    pub fn device_id(&self) -> dn_sync::DeviceId {
        self.keys.id()
    }

    /// Serves until `stop` is notified.
    pub async fn serve(self, stop: Arc<Notify>) -> Result<(), String> {
        let scheduler = self.scheduler.clone();
        let jobs_stop = Arc::new(Notify::new());
        let jobs = {
            let jobs_stop = jobs_stop.clone();
            tokio::spawn(async move { scheduler.run(&jobs_stop).await })
        };
        #[cfg(unix)]
        let control = tokio::spawn(control::serve(
            self.control,
            self.data_dir.clone(),
            self.keys.public(),
            self.pairing.clone(),
        ));
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let base = Arc::new(StatusBase {
            name: self.name,
            store: self.store,
        });
        loop {
            let accepted = tokio::select! {
                () = stop.notified() => break,
                accepted = self.listener.accept() => accepted,
            };
            let (stream, peer) = match accepted {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("dn node: accept failed: {e}");
                    continue;
                }
            };
            let permit = tokio::select! {
                () = stop.notified() => break,
                permit = slots.clone().acquire_owned() => match permit {
                    Ok(p) => p,
                    Err(_) => break,
                },
            };
            let (keys, db, base, pairing) = (
                self.keys.clone(),
                self.db.clone(),
                base.clone(),
                self.pairing.clone(),
            );
            tokio::spawn(async move {
                handle(stream, peer, &keys, &db, &base, &pairing).await;
                drop(permit);
            });
        }
        #[cfg(unix)]
        control.abort();
        jobs_stop.notify_waiters();
        self.scheduler.cancel_current();
        match jobs.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("the job scheduler stopped: {e}")),
            Err(e) => Err(format!("the job scheduler failed: {e}")),
        }
    }
}

struct StatusBase {
    name: String,
    store: StoreReport,
}

fn status(base: &StatusBase, db: &Mutex<Connection>) -> Result<NodeStatus, SyncError> {
    let devices = DeviceList::members(&lock(db))?.len();
    Ok(NodeStatus {
        name: base.name.clone(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        secret_store: base.store.kind.to_owned(),
        secret_store_label: base.store.label.clone(),
        protected_at_rest: base.store.protected_at_rest,
        devices: u32::try_from(devices).unwrap_or(u32::MAX),
    })
}

/// One device's connection. Logs events and errors only, never message content.
async fn handle(
    stream: TcpStream,
    peer: SocketAddr,
    keys: &DeviceKeys,
    db: &Mutex<Connection>,
    base: &StatusBase,
    pairing: &control::PairingState,
) {
    let mut channel = match tokio::time::timeout(
        Duration::from_secs(HANDSHAKE_SECS),
        transport::accept(stream, keys),
    )
    .await
    {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            eprintln!("dn node: refused {peer}: {e}");
            return;
        }
        Err(_) => {
            eprintln!("dn node: refused {peer}: no handshake within {HANDSHAKE_SECS} s");
            return;
        }
    };
    let device = {
        let conn = lock(db);
        match transport::authorize_member(&conn, &channel) {
            Ok(d) => {
                if let Err(e) = DeviceList::mark_synced(&conn, d.id, dn_store::now_ms()) {
                    eprintln!("dn node: couldn't record when {} last synced: {e}", d.id);
                }
                Some(d)
            }
            Err(SyncError::UnknownDevice) => None,
            Err(e) => {
                eprintln!("dn node: refused {peer}: {e}");
                return;
            }
        }
    };
    let Some(device) = device else {
        pair_device(channel, peer, keys, db, pairing).await;
        return;
    };
    loop {
        let message =
            match tokio::time::timeout(Duration::from_secs(IDLE_SECS), channel.recv()).await {
                Ok(Ok(m)) => m,
                Ok(Err(SyncError::Io(_))) | Err(_) => break,
                Ok(Err(e)) => {
                    eprintln!("dn node: {} ({}): {e}", device.name, device.id);
                    break;
                }
            };
        // Checked again for every message: a device removed while connected is cut off
        // at its next request.
        if let Err(e) = transport::authorize_member(&lock(db), &channel) {
            eprintln!("dn node: disconnected {} ({}): {e}", device.name, device.id);
            break;
        }
        let reply = match message {
            Message::StatusRequest => match status(base, db) {
                Ok(s) => Message::Status(s),
                Err(e) => {
                    eprintln!("dn node: status failed: {e}");
                    break;
                }
            },
            _ => {
                eprintln!(
                    "dn node: {} ({}) sent a message this Node doesn't expect",
                    device.name, device.id
                );
                break;
            }
        };
        if let Err(e) = channel.send(&reply).await {
            eprintln!("dn node: {} ({}): {e}", device.name, device.id);
            break;
        }
    }
    let _ = channel.close().await;
}

/// A device that isn't paired: it may join only while `dn node pair` has a code open.
async fn pair_device(
    mut channel: transport::Channel<TcpStream>,
    peer: SocketAddr,
    keys: &DeviceKeys,
    db: &Mutex<Connection>,
    pairing: &control::PairingState,
) {
    let taken = {
        let mut slot = pairing.lock().await;
        slot.as_mut()
            .and_then(|p| p.offer.take().map(|o| (o, p.joining.clone(), p.id)))
    };
    let Some((mut offer, joining_tx, pairing_id)) = taken else {
        eprintln!("dn node: refused {peer}: not a paired device, and no pairing is open");
        let _ = channel.close().await;
        return;
    };
    let request = tokio::time::timeout(
        Duration::from_secs(JOIN_SECS),
        transport::read_join(db, &mut channel, &mut offer, || now_secs() as u64),
    )
    .await
    .unwrap_or(Err(SyncError::Protocol("no pairing request in time")));
    let request = match request {
        Ok(r) => r,
        Err(e) => {
            eprintln!("dn node: pairing from {peer} refused: {e}");
            // Wrong proofs and late or bad requests count against the offer; it goes back
            // only into the same pairing, and only while that pairing is still open.
            if matches!(
                e,
                SyncError::WrongProof | SyncError::Protocol(_) | SyncError::BadName
            ) && let Some(p) = pairing.lock().await.as_mut()
                && p.id == pairing_id
                && p.offer.is_none()
            {
                p.offer = Some(offer);
            }
            return;
        }
    };
    let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let asked = joining_tx
        .send(control::Joining {
            name: request.name.clone(),
            kind: request.kind,
            numbers: request.numbers,
            answer: answer_tx,
            result: result_rx,
        })
        .await
        .is_ok();
    // No time limit: the person is comparing the numbers. Closing `dn node pair` answers no.
    let accepted = asked && answer_rx.await.unwrap_or(false);
    let finished = transport::finish_join(
        db,
        &mut channel,
        &request,
        Some(keys.id()),
        accepted,
        dn_store::now_ms(),
    )
    .await;
    let outcome = match finished {
        Ok(Some(device)) => {
            eprintln!("dn node: paired {} ({})", device.name, device.id);
            Ok(device.name)
        }
        Ok(None) => Err("declined".to_owned()),
        Err(e) => Err(format!("couldn't add the device: {e}")),
    };
    let _ = result_tx.send(outcome);
    let _ = channel.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_sync::identity::PublicIdentity;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "dn-node-{tag}-{}-{}",
                std::process::id(),
                dn_store::now_ms()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    fn init(dir: &Path, secrets: SecretsConfig) {
        NodeConfig::new(
            "Home server".into(),
            "127.0.0.1:0".parse().unwrap(),
            secrets,
            60,
        )
        .unwrap()
        .create(dir)
        .unwrap();
    }

    async fn started(dir: &Path) -> Node {
        Node::start(dir, Vec::new(), &no_env).await.unwrap()
    }

    fn add_device(dir: &Path, keys: &PublicIdentity, name: &str) {
        let conn = dn_store::open(
            &dir.join(config::DATABASE_FILE),
            &[
                MigrationSet::new("sync", dn_sync::migrations()),
                MigrationSet::new("jobs", dn_jobs::migrations()),
            ],
        )
        .unwrap();
        DeviceList::add(&conn, keys, name, DeviceKind::Desktop, None, 1).unwrap();
    }

    async fn ask_status(
        addr: SocketAddr,
        keys: &DeviceKeys,
        node: &[u8; 32],
    ) -> Result<Message, SyncError> {
        let stream = TcpStream::connect(addr).await.unwrap();
        let mut ch = transport::connect(stream, keys, node).await?;
        ch.send(&Message::StatusRequest).await?;
        ch.recv().await
    }

    #[tokio::test]
    async fn a_paired_device_gets_the_status_and_others_are_refused() {
        let dir = TempDir::new("status");
        init(&dir.0, SecretsConfig::InsecurePlaintext);
        // First start creates the Node's keys and lists it; the store keeps them.
        let node_key = {
            let node = started(&dir.0).await;
            let conn = lock(&node.db);
            DeviceList::members(&conn).unwrap()[0].keys.ed25519
        };
        let laptop = DeviceKeys::generate().unwrap();
        add_device(&dir.0, &laptop.public(), "MacBook Pro");

        let node = started(&dir.0).await;
        let addr = node.local_addr().unwrap();
        let stop = Arc::new(Notify::new());
        let serving = tokio::spawn(node.serve(stop.clone()));

        let Message::Status(s) = ask_status(addr, &laptop, &node_key).await.unwrap() else {
            panic!("expected the status");
        };
        assert_eq!(s.name, "Home server");
        assert_eq!(s.secret_store, "insecure_plaintext");
        assert!(!s.protected_at_rest);
        assert_eq!(s.devices, 2);

        // A stranger completes TLS but is refused right after.
        let stranger = DeviceKeys::generate().unwrap();
        assert!(ask_status(addr, &stranger, &node_key).await.is_err());

        stop.notify_waiters();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_removed_device_is_refused() {
        let dir = TempDir::new("removed");
        init(&dir.0, SecretsConfig::InsecurePlaintext);
        let phone = DeviceKeys::generate().unwrap();
        let node = started(&dir.0).await;
        let node_key = DeviceList::members(&lock(&node.db)).unwrap()[0]
            .keys
            .ed25519;
        {
            let conn = lock(&node.db);
            DeviceList::add(
                &conn,
                &phone.public(),
                "Work phone",
                DeviceKind::Phone,
                None,
                1,
            )
            .unwrap();
            DeviceList::remove(&conn, phone.id(), 2).unwrap();
        }
        let addr = node.local_addr().unwrap();
        let stop = Arc::new(Notify::new());
        let serving = tokio::spawn(node.serve(stop.clone()));
        assert!(ask_status(addr, &phone, &node_key).await.is_err());
        stop.notify_waiters();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn the_node_refuses_to_start_with_secrets_in_the_environment() {
        let dir = TempDir::new("env");
        init(&dir.0, SecretsConfig::InsecurePlaintext);
        let vars = vec![(
            OsString::from("ANTHROPIC_API_KEY"),
            OsString::from("sk-test"),
        )];
        let Err(e) = Node::start(&dir.0, vars, &no_env).await else {
            panic!("expected a refusal");
        };
        assert!(e.contains("ANTHROPIC_API_KEY"));
        assert!(!e.contains("sk-test"));
    }

    #[tokio::test]
    async fn a_passphrase_store_is_refused_until_unlock_exists() {
        let dir = TempDir::new("locked");
        init(&dir.0, SecretsConfig::Passphrase);
        let Err(e) = Node::start(&dir.0, Vec::new(), &no_env).await else {
            panic!("expected a refusal");
        };
        assert!(e.contains("#255"));
    }

    #[tokio::test]
    async fn a_store_that_cannot_open_stops_the_node_with_the_reason() {
        let dir = TempDir::new("nostore");
        init(
            &dir.0,
            SecretsConfig::SystemdCredential {
                name: "demios".into(),
            },
        );
        let Err(e) = Node::start(&dir.0, Vec::new(), &no_env).await else {
            panic!("expected a refusal");
        };
        assert!(e.contains("systemd unit"), "{e}");
        assert!(
            !dir.0.join(config::DATABASE_FILE).exists(),
            "nothing else was set up"
        );
    }

    #[tokio::test]
    async fn the_node_keeps_its_identity_across_restarts() {
        let dir = TempDir::new("identity");
        init(&dir.0, SecretsConfig::InsecurePlaintext);
        let first = started(&dir.0).await.device_id();
        let again = started(&dir.0).await;
        assert_eq!(first, again.device_id());
        assert_eq!(DeviceList::members(&lock(&again.db)).unwrap().len(), 1);
    }

    #[cfg(unix)]
    mod pairing {
        use super::*;
        use dn_sync::pairing::{CheckNumbers, PairingLink};

        /// Starts a Node, runs `dn node pair` with `answer`, and has `laptop` join with the
        /// link it shows. Returns what `dn node pair` and the device each saw.
        async fn pair_with(
            dir: &Path,
            laptop: &DeviceKeys,
            answer: bool,
        ) -> (Result<String, String>, Result<(), SyncError>, Node2) {
            let node = started(dir).await;
            let addr = node.local_addr().unwrap();
            let node_pub = DeviceList::members(&lock(&node.db)).unwrap()[0].keys;
            let db = node.db.clone();
            let stop = Arc::new(Notify::new());
            let serving = tokio::spawn(node.serve(stop.clone()));
            let (link_tx, mut link_rx) = tokio::sync::watch::channel(String::new());
            let want = CheckNumbers::new(&node_pub, &laptop.public()).to_string();
            let cli = control::client_pair(
                dir,
                addr.to_string(),
                move |_, _, numbers| {
                    assert_eq!(numbers, want, "both sides show the same numbers");
                    answer
                },
                move |text| {
                    if let Some(link) = text
                        .lines()
                        .map(str::trim)
                        .find(|l| l.starts_with("demios://"))
                    {
                        let _ = link_tx.send(link.to_owned());
                    }
                },
            );
            let device = async {
                link_rx.changed().await.unwrap();
                let link =
                    PairingLink::parse(&link_rx.borrow().clone(), now_secs() as u64).unwrap();
                let stream = TcpStream::connect(addr).await.unwrap();
                let pending =
                    transport::join(stream, laptop, &link, "MacBook Pro", DeviceKind::Desktop)
                        .await?;
                pending.answer().await.map(|_| ())
            };
            let (cli, device) = tokio::join!(cli, device);
            (cli, device, Node2 { db, stop, serving })
        }

        struct Node2 {
            db: Arc<Mutex<Connection>>,
            stop: Arc<Notify>,
            serving: tokio::task::JoinHandle<Result<(), String>>,
        }

        impl Node2 {
            async fn stop(self) {
                self.stop.notify_waiters();
                self.serving.await.unwrap().unwrap();
            }
        }

        #[tokio::test]
        async fn dn_node_pair_adds_a_device_only_after_the_numbers_are_confirmed() {
            let dir = TempDir::new("pair");
            init(&dir.0, SecretsConfig::InsecurePlaintext);
            let laptop = DeviceKeys::generate().unwrap();
            let (cli, device, node) = pair_with(&dir.0, &laptop, true).await;
            assert_eq!(cli.unwrap(), "MacBook Pro");
            device.unwrap();
            let members = DeviceList::members(&lock(&node.db)).unwrap();
            assert!(members.iter().any(|d| d.keys == laptop.public()));
            node.stop().await;
        }

        #[tokio::test]
        async fn answering_no_adds_nothing() {
            let dir = TempDir::new("decline");
            init(&dir.0, SecretsConfig::InsecurePlaintext);
            let laptop = DeviceKeys::generate().unwrap();
            let (cli, device, node) = pair_with(&dir.0, &laptop, false).await;
            assert!(cli.unwrap_err().contains("Nothing was added"));
            assert!(matches!(device, Err(SyncError::Declined)));
            assert_eq!(DeviceList::members(&lock(&node.db)).unwrap().len(), 1);
            node.stop().await;
        }

        #[tokio::test]
        async fn one_pairing_at_a_time_and_closing_it_ends_the_code() {
            let dir = TempDir::new("one");
            init(&dir.0, SecretsConfig::InsecurePlaintext);
            let node = started(&dir.0).await;
            let addr = node.local_addr().unwrap();
            let stop = Arc::new(Notify::new());
            let serving = tokio::spawn(node.serve(stop.clone()));
            // The first `dn node pair` holds the code open until it's dropped.
            let (shown_tx, shown_rx) = tokio::sync::oneshot::channel();
            let mut shown_tx = Some(shown_tx);
            let first = tokio::spawn({
                let dir = dir.0.clone();
                async move {
                    control::client_pair(
                        &dir,
                        addr.to_string(),
                        |_, _, _| false,
                        move |_| {
                            if let Some(tx) = shown_tx.take() {
                                let _ = tx.send(());
                            }
                        },
                    )
                    .await
                }
            });
            shown_rx.await.unwrap();
            let second =
                control::client_pair(&dir.0, addr.to_string(), |_, _, _| true, |_| {}).await;
            assert!(second.unwrap_err().contains("already open"));
            first.abort();
            let _ = first.await;
            // With no pairing open, an unknown device is refused again.
            let stranger = DeviceKeys::generate().unwrap();
            let node_key = {
                let conn = dn_store::open(
                    &dir.0.join(config::DATABASE_FILE),
                    &[
                        MigrationSet::new("sync", dn_sync::migrations()),
                        MigrationSet::new("jobs", dn_jobs::migrations()),
                    ],
                )
                .unwrap();
                DeviceList::members(&conn).unwrap()[0].keys.ed25519
            };
            let mut refused = false;
            for _ in 0..50 {
                if ask_status(addr, &stranger, &node_key).await.is_err() {
                    refused = true;
                    break;
                }
            }
            assert!(refused);
            let third = control::client_pair(&dir.0, addr.to_string(), |_, _, _| false, |_| {});
            // A new pairing can start once the first one closed.
            let third = tokio::time::timeout(Duration::from_millis(500), third).await;
            assert!(third.is_err(), "it waits for a device, so it's still open");
            stop.notify_waiters();
            serving.await.unwrap().unwrap();
        }

        #[test]
        fn the_address_must_be_one_devices_can_reach() {
            let config = |listen: &str| {
                NodeConfig::new(
                    "N".into(),
                    listen.parse().unwrap(),
                    SecretsConfig::InsecurePlaintext,
                    0,
                )
                .unwrap()
            };
            assert!(
                pair_address(&config("0.0.0.0:7443"), None)
                    .unwrap_err()
                    .contains("--address")
            );
            assert_eq!(
                pair_address(&config("192.168.1.20:7443"), None).unwrap(),
                "192.168.1.20:7443"
            );
            assert_eq!(
                pair_address(&config("0.0.0.0:7443"), Some("nas:7443".into())).unwrap(),
                "nas:7443"
            );
        }
    }
}
