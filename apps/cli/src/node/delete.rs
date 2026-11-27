//! `dn node delete-all`: deletes the Node and everything on it (ADR-0014 §8, #309).
//!
//! 1. It refuses while `dn node run` is running: the running Node holds the database and
//!    the keys.
//! 2. It says what it deletes, and asks for the Node's name (or takes `--confirm <name>`).
//! 3. It deletes the Node's secrets first: its device keys and every group key, from the
//!    secret store it was set up with. A store inside the data folder goes with the folder.
//!    A keychain that can't be opened stops everything, so no key is left behind unnoticed.
//! 4. It deletes the database, the store files, the control socket and `node.json` last, so
//!    a failure part-way can be run again.
//! 5. It removes the data folder if nothing else is in it, and otherwise names what's left.
//!
//! Paired devices keep their own copies. They can no longer reach this Node; Settings ›
//! Node on each offers **Forget this Node**.

use super::config::{CONFIG_FILE, DATABASE_FILE, NodeConfig, SecretsConfig};
use dn_secrets::SecretStore;
use dn_secrets::server::{self, ENCRYPTED_FILE, PLAINTEXT_FILE};
use dn_store::MigrationSet;
use dn_sync::devices::{DeviceKind, DeviceList};
use dn_sync::membership;
use std::path::Path;

/// What was deleted, and what wasn't.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub keys_deleted: usize,
    pub files_deleted: Vec<String>,
    /// Entries left in the data folder that `dn node` didn't create.
    pub left: Vec<String>,
    pub folder_removed: bool,
}

/// What the confirmation shows.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub name: String,
    pub devices: usize,
    pub store: String,
}

const SOCKET_FILE: &str = "control.sock";
const OWN_FILES: [&str; 8] = [
    "node.db",
    "node.db-wal",
    "node.db-shm",
    "node.db-journal",
    ENCRYPTED_FILE,
    PLAINTEXT_FILE,
    SOCKET_FILE,
    CONFIG_FILE,
];

/// Refuses while the Node runs: its control socket answers.
#[cfg(unix)]
pub async fn ensure_stopped(data_dir: &Path) -> Result<(), String> {
    let path = data_dir.join(SOCKET_FILE);
    if tokio::net::UnixStream::connect(&path).await.is_ok() {
        return Err(
            "the Node is running. Stop it first (for example `systemctl stop demios-node`, or \
             Ctrl-C on `dn node run`), then run dn node delete-all again"
                .into(),
        );
    }
    Ok(())
}

fn open_db(data_dir: &Path) -> Result<dn_store::Connection, String> {
    dn_store::open(
        &data_dir.join(DATABASE_FILE),
        &[
            MigrationSet::new("sync", dn_sync::migrations()),
            MigrationSet::new("jobs", dn_jobs::migrations()),
        ],
    )
    .map_err(|e| format!("couldn't open the Node's database: {e}"))
}

/// Reads what the confirmation shows. Fails if `data_dir` isn't a Node's data folder.
pub fn plan(data_dir: &Path) -> Result<Plan, String> {
    let config = NodeConfig::load(data_dir)?;
    let devices = if data_dir.join(DATABASE_FILE).exists() {
        let conn = open_db(data_dir)?;
        DeviceList::members(&conn)
            .map_err(|e| format!("couldn't read the device list: {e}"))?
            .iter()
            .filter(|d| d.kind != DeviceKind::Node)
            .count()
    } else {
        0
    };
    Ok(Plan {
        name: config.name,
        devices,
        store: store_name(&config.secrets),
    })
}

fn store_name(secrets: &SecretsConfig) -> String {
    match secrets {
        SecretsConfig::Keychain => "the system keychain".into(),
        SecretsConfig::InsecurePlaintext => format!("the unencrypted file {PLAINTEXT_FILE}"),
        _ => format!("the encrypted file {ENCRYPTED_FILE}"),
    }
}

/// Deletes the Node's keys from its secret store: its own identity and every group key the
/// membership chain names. A store kept as a file in the data folder that can't be opened
/// is left to the file deletion; a keychain that can't be opened is an error.
fn delete_keys(
    data_dir: &Path,
    config: &NodeConfig,
    env: server::Env<'_>,
) -> Result<usize, String> {
    let store = match server::open(&config.secrets.choice(), data_dir, env) {
        Ok(s) => s,
        Err(e) if config.secrets != SecretsConfig::Keychain => {
            eprintln!(
                "dn node: the secret store couldn't be opened ({e}); its file is deleted below"
            );
            return Ok(0);
        }
        Err(e) => {
            return Err(format!(
                "couldn't open the keychain to delete the Node's keys: {e}. Nothing was deleted"
            ));
        }
    };
    if store.report().locked {
        // A passphrase store is one file in the data folder: deleting it deletes the keys.
        return Ok(0);
    }
    let store: &dyn SecretStore = &*store;
    let mut deleted = 0;
    if !data_dir.join(DATABASE_FILE).exists() && config.secrets == SecretsConfig::Keychain {
        // Without the database the group's keys can't be listed, so they can't be found.
        eprintln!(
            "dn node: {DATABASE_FILE} is gone, so the group keys in the keychain can't be \
             listed. Remove the keychain items named device/group.* by hand"
        );
    }
    if data_dir.join(DATABASE_FILE).exists() {
        let conn = open_db(data_dir)?;
        let node = DeviceList::members(&conn)
            .map_err(|e| format!("couldn't read the device list: {e}"))?
            .into_iter()
            .find(|d| d.kind == DeviceKind::Node);
        if let Some(node) = node
            && let Some(chain) = membership::load(&conn, &node.keys)
                .map_err(|e| format!("couldn't read the device group: {e}"))?
        {
            for epoch in 0..=chain.epoch() {
                membership::delete_key(store, &chain.group(), epoch)
                    .map_err(|e| format!("couldn't delete a group key: {e}"))?;
                deleted += 1;
            }
        }
    }
    if store
        .delete(&dn_sync::identity::secret_id())
        .map_err(|e| format!("couldn't delete the Node's keys: {e}"))?
    {
        deleted += 1;
    }
    Ok(deleted)
}

/// Deletes everything. The caller has checked that the Node is stopped and that the person
/// confirmed.
pub fn delete_all(data_dir: &Path, env: server::Env<'_>) -> Result<Report, String> {
    let config = NodeConfig::load(data_dir)?;
    let mut report = Report {
        keys_deleted: delete_keys(data_dir, &config, env)?,
        ..Report::default()
    };
    for file in OWN_FILES {
        let path = data_dir.join(file);
        match std::fs::remove_file(&path) {
            Ok(()) => report.files_deleted.push(file.to_owned()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("couldn't delete {}: {e}", path.display())),
        }
    }
    let mut left: Vec<String> = std::fs::read_dir(data_dir)
        .map_err(|e| format!("couldn't read {}: {e}", data_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    if left.is_empty() {
        std::fs::remove_dir(data_dir)
            .map_err(|e| format!("couldn't remove {}: {e}", data_dir.display()))?;
        report.folder_removed = true;
    }
    report.left = left;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_secrets::{Secret, SecretId};

    fn node_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dn-node-del-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        NodeConfig::new(
            "Test node".into(),
            "127.0.0.1:0".parse().unwrap(),
            SecretsConfig::InsecurePlaintext,
            "UTC".into(),
        )
        .unwrap()
        .create(&dir)
        .unwrap();
        dir
    }

    fn no_env(_: &str) -> Option<std::ffi::OsString> {
        None
    }

    #[test]
    fn delete_all_removes_the_keys_the_files_and_the_folder() {
        let dir = node_dir("all");
        // What `dn node run` leaves: keys in the store, a group, the database.
        let store =
            server::open(&SecretsConfig::InsecurePlaintext.choice(), &dir, &no_env).unwrap();
        let keys = dn_sync::identity::DeviceKeys::load_or_create(&*store).unwrap();
        let conn = open_db(&dir).unwrap();
        DeviceList::add(
            &conn,
            &keys.public(),
            "Test node",
            DeviceKind::Node,
            None,
            1,
        )
        .unwrap();
        dn_sync::group::NodeGroup::load_or_create(&conn, &*store, &keys, "Test node").unwrap();
        store
            .set(
                &SecretId::device("other"),
                &Secret::new("kept until the file goes"),
            )
            .unwrap();
        drop((store, conn));

        let plan = plan(&dir).unwrap();
        assert_eq!(plan.name, "Test node");
        assert_eq!(plan.devices, 0, "the Node itself isn't counted");

        let report = delete_all(&dir, &no_env).unwrap();
        assert_eq!(report.keys_deleted, 2, "identity and the epoch-0 group key");
        assert!(report.files_deleted.contains(&CONFIG_FILE.to_owned()));
        assert!(report.files_deleted.contains(&"node.db".to_owned()));
        assert!(report.files_deleted.contains(&PLAINTEXT_FILE.to_owned()));
        assert!(report.folder_removed);
        assert!(!dir.exists());
    }

    #[test]
    fn files_it_did_not_make_are_left_and_named() {
        let dir = node_dir("left");
        std::fs::write(dir.join("backup.tar"), b"x").unwrap();
        let report = delete_all(&dir, &no_env).unwrap();
        assert_eq!(report.left, vec!["backup.tar".to_owned()]);
        assert!(!report.folder_removed);
        assert!(!dir.join(CONFIG_FILE).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_that_isnt_a_node_is_refused() {
        let dir = std::env::temp_dir().join(format!("dn-node-del-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(plan(&dir).is_err());
        assert!(delete_all(&dir, &no_env).is_err());
        assert!(dir.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_running_node_is_refused() {
        let dir = node_dir("running");
        let _listener = tokio::net::UnixListener::bind(dir.join(SOCKET_FILE)).unwrap();
        assert!(ensure_stopped(&dir).await.unwrap_err().contains("running"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
