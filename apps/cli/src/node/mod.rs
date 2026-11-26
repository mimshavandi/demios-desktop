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
    }
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
            let (keys, db, base) = (self.keys.clone(), self.db.clone(), base.clone());
            tokio::spawn(async move {
                handle(stream, peer, &keys, &db, &base).await;
                drop(permit);
            });
        }
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
                d
            }
            Err(e) => {
                eprintln!("dn node: refused {peer}: {e}");
                return;
            }
        }
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
}
