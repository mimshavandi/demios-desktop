//! The Node's configuration: `node.json` in its data folder, written once by
//! `dn node init` and read by `dn node run`. It holds no secrets: only the choice of
//! secret store, the address to listen on, the Node's name and its time zone offset.

use dn_secrets::server::{KeySource, ServerStoreChoice};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = "node.json";
pub const DATABASE_FILE: &str = "node.db";
const CONFIG_VERSION: u32 = 1;

/// The secret store, as `node.json` records it (ADR-0014 §7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "store", rename_all = "snake_case", deny_unknown_fields)]
pub enum SecretsConfig {
    /// The platform keychain under the Node's service account.
    Keychain,
    SystemdCredential {
        name: String,
    },
    ContainerSecret {
        name: String,
    },
    KeyFile {
        path: PathBuf,
    },
    Passphrase,
    /// Only for test setups; every client shows a warning.
    InsecurePlaintext,
}

impl SecretsConfig {
    pub fn choice(&self) -> ServerStoreChoice {
        match self {
            Self::Keychain => ServerStoreChoice::Keychain,
            Self::SystemdCredential { name } => {
                ServerStoreChoice::EncryptedFile(KeySource::SystemdCredential {
                    name: name.clone(),
                })
            }
            Self::ContainerSecret { name } => {
                ServerStoreChoice::EncryptedFile(KeySource::ContainerSecret { name: name.clone() })
            }
            Self::KeyFile { path } => {
                ServerStoreChoice::EncryptedFile(KeySource::KeyFile { path: path.clone() })
            }
            Self::Passphrase => ServerStoreChoice::EncryptedFile(KeySource::Passphrase),
            Self::InsecurePlaintext => ServerStoreChoice::InsecurePlaintext,
        }
    }

    /// Reads `--secrets`: `keychain`, `systemd-credential:<name>`,
    /// `container-secret:<name>`, `key-file:<path>`, `passphrase` or
    /// `insecure-plaintext`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (kind, arg) = match text.split_once(':') {
            Some((k, a)) => (k, Some(a)),
            None => (text, None),
        };
        let need = |what: &str| {
            arg.filter(|a| !a.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("--secrets {kind} needs a {what}, e.g. {kind}:<{what}>"))
        };
        Ok(match kind {
            "keychain" if arg.is_none() => Self::Keychain,
            "systemd-credential" => Self::SystemdCredential {
                name: need("name")?,
            },
            "container-secret" => Self::ContainerSecret {
                name: need("name")?,
            },
            "key-file" => Self::KeyFile {
                path: PathBuf::from(need("path")?),
            },
            "passphrase" if arg.is_none() => Self::Passphrase,
            "insecure-plaintext" if arg.is_none() => Self::InsecurePlaintext,
            _ => {
                return Err(format!(
                    "unknown secret store `{text}`: use keychain, systemd-credential:<name>, \
                     container-secret:<name>, key-file:<path>, passphrase or insecure-plaintext"
                ));
            }
        })
    }
}

/// Who may connect to the Node (ADR-0014 §4a).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// The default: devices on the local network only, from private, link-local and
    /// loopback addresses. Anything else is refused before the TLS handshake.
    #[default]
    LocalNetwork,
    /// Chosen by the person (`dn node init --reachable-from-outside`): any address, for a
    /// VPN or overlay, a forwarded port or a TLS-passthrough proxy. Pairing and the pinned
    /// keys still decide who gets in; the Node sees every attempt.
    Outside,
}

impl Reach {
    fn is_default(&self) -> bool {
        *self == Self::LocalNetwork
    }

    /// Whether a connection from `peer` may go on to the handshake.
    pub fn allows(self, peer: IpAddr) -> bool {
        self == Self::Outside || is_local_network(peer)
    }
}

/// Private (RFC 1918), link-local and loopback IPv4; unique-local (`fc00::/7`), link-local
/// (`fe80::/10`) and loopback IPv6, and IPv4 addresses mapped into IPv6.
pub fn is_local_network(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_local_network(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub v: u32,
    /// The Node's name on every device's list.
    pub name: String,
    pub listen: SocketAddr,
    pub secrets: SecretsConfig,
    /// The IANA time zone for jobs at a time of day, e.g. `Europe/Berlin`. Daylight saving
    /// follows it.
    pub time_zone: String,
    /// Who may connect. Left out of `node.json` while it's the default, so files written
    /// before this setting read the same.
    #[serde(default, skip_serializing_if = "Reach::is_default")]
    pub reach: Reach,
}

impl NodeConfig {
    pub fn new(
        name: String,
        listen: SocketAddr,
        secrets: SecretsConfig,
        time_zone: String,
    ) -> Result<Self, String> {
        let config = Self {
            v: CONFIG_VERSION,
            name,
            listen,
            secrets,
            time_zone,
            reach: Reach::LocalNetwork,
        };
        config.check()?;
        Ok(config)
    }

    fn check(&self) -> Result<(), String> {
        if self.v != CONFIG_VERSION {
            return Err(format!(
                "{CONFIG_FILE} is version {}; this dn reads version {CONFIG_VERSION}",
                self.v
            ));
        }
        let len = self.name.trim().chars().count();
        if len == 0 || len > 64 || self.name.chars().any(char::is_control) {
            return Err("the Node's name must be 1 to 64 characters".into());
        }
        crate::node::zone::Zone::named(&self.time_zone)?;
        Ok(())
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(CONFIG_FILE)
    }

    pub fn load(data_dir: &Path) -> Result<Self, String> {
        let path = Self::path(data_dir);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "couldn't read {}: {e}. Set the Node up first with `dn node init`",
                path.display()
            )
        })?;
        // A node.json from before time zones: say exactly what to change.
        if let Ok(serde_json::Value::Object(o)) = serde_json::from_str(&text)
            && o.contains_key("utc_offset_minutes")
            && !o.contains_key("time_zone")
        {
            let zone = crate::node::zone::system_name(
                &|k| std::env::var_os(k),
                Path::new("/etc/localtime"),
            )
            .unwrap_or_else(|_| "Europe/Berlin".into());
            return Err(format!(
                "{} sets `utc_offset_minutes`, which this dn replaced with a time zone (daylight \
                 saving included). Replace that line with\n  \"time_zone\": \"{zone}\"\nusing \
                 the Node's own IANA time zone name, then start it again",
                path.display()
            ));
        }
        let config: Self = serde_json::from_str(&text)
            .map_err(|e| format!("{} isn't valid: {e}", path.display()))?;
        config.check()?;
        Ok(config)
    }

    /// Writes `node.json`. Refuses to replace an existing one.
    pub fn create(&self, data_dir: &Path) -> Result<(), String> {
        create_private_dir(data_dir)?;
        check_private_dir(data_dir)?;
        let path = Self::path(data_dir);
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => format!(
                "{} already exists; this Node is already set up",
                path.display()
            ),
            _ => format!("couldn't write {}: {e}", path.display()),
        })?;
        std::io::Write::write_all(&mut file, text.as_bytes())
            .map_err(|e| format!("couldn't write {}: {e}", path.display()))
    }
}

/// Refuses a data folder that other users can read or enter: it holds the database and,
/// with some stores, the secrets file (Unix; Windows relies on the service account's ACLs).
pub fn check_private_dir(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta =
            std::fs::metadata(dir).map_err(|e| format!("couldn't read {}: {e}", dir.display()))?;
        if meta.mode() & 0o077 != 0 {
            return Err(format!(
                "{} can be read by other users; make it owner-only (chmod 700)",
                dir.display()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn create_private_dir(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    let made = {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    };
    #[cfg(not(unix))]
    let made = std::fs::create_dir_all(dir);
    made.map_err(|e| format!("couldn't create {}: {e}", dir.display()))
}

/// Environment variables that commonly carry API keys or tokens.
const SECRET_VARS: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "MISTRAL_API_KEY",
    "GROQ_API_KEY",
    "OPENROUTER_API_KEY",
    "DEEPSEEK_API_KEY",
    "XAI_API_KEY",
    "TOGETHER_API_KEY",
    "HF_TOKEN",
    "HUGGING_FACE_HUB_TOKEN",
    "GITHUB_TOKEN",
];

/// ADR-0014 §7: secrets in environment variables are refused, and the Node won't start
/// with them, because they leak into process listings, crash dumps and child processes.
/// Returns the offending variable names (never their values).
pub fn secret_env_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<String> {
    let mut found: Vec<String> = vars
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| {
            let upper = k.to_ascii_uppercase();
            SECRET_VARS.contains(&upper.as_str())
                || (upper.starts_with("DEMIOS_")
                    && upper
                        .split('_')
                        .any(|w| ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSPHRASE"].contains(&w)))
        })
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_stores_parse_strictly() {
        assert_eq!(
            SecretsConfig::parse("keychain").unwrap(),
            SecretsConfig::Keychain
        );
        assert_eq!(
            SecretsConfig::parse("systemd-credential:demios").unwrap(),
            SecretsConfig::SystemdCredential {
                name: "demios".into()
            }
        );
        assert_eq!(
            SecretsConfig::parse("key-file:/mnt/keys/node.key").unwrap(),
            SecretsConfig::KeyFile {
                path: "/mnt/keys/node.key".into()
            }
        );
        for bad in [
            "",
            "plaintext",
            "keychain:x",
            "systemd-credential",
            "container-secret:",
        ] {
            assert!(SecretsConfig::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_local_network_peers_pass_by_default() {
        for ok in [
            "10.0.0.5",
            "172.16.4.1",
            "172.31.255.255",
            "192.168.1.20",
            "169.254.10.1",
            "127.0.0.1",
            "::1",
            "fd00::1",
            "fc12::7",
            "fe80::1",
            "::ffff:192.168.1.20",
        ] {
            let ip: IpAddr = ok.parse().unwrap();
            assert!(Reach::LocalNetwork.allows(ip), "{ok}");
        }
        for public in [
            "8.8.8.8",
            "172.32.0.1",
            "100.64.0.1",
            "192.169.0.1",
            "2001:db8::1",
            "2a00:1450::1",
            "::ffff:8.8.8.8",
            "0.0.0.0",
            "::",
        ] {
            let ip: IpAddr = public.parse().unwrap();
            assert!(!Reach::LocalNetwork.allows(ip), "{public}");
            assert!(Reach::Outside.allows(ip), "{public}");
        }
    }

    #[test]
    fn the_reach_is_written_only_when_chosen() {
        let mut c = NodeConfig::new(
            "VPS".into(),
            "0.0.0.0:7443".parse().unwrap(),
            SecretsConfig::InsecurePlaintext,
            "UTC".into(),
        )
        .unwrap();
        let local = serde_json::to_string(&c).unwrap();
        assert!(!local.contains("reach"), "{local}");
        assert_eq!(
            serde_json::from_str::<NodeConfig>(&local).unwrap().reach,
            Reach::LocalNetwork
        );
        c.reach = Reach::Outside;
        let outside = serde_json::to_string(&c).unwrap();
        assert!(outside.contains(r#""reach":"outside""#), "{outside}");
        assert_eq!(serde_json::from_str::<NodeConfig>(&outside).unwrap(), c);
    }

    #[test]
    fn config_round_trips_and_is_written_once() {
        let dir = std::env::temp_dir().join(format!("dn-node-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let c = NodeConfig::new(
            "Home server".into(),
            "127.0.0.1:7443".parse().unwrap(),
            SecretsConfig::ContainerSecret {
                name: "demios".into(),
            },
            "Europe/Berlin".into(),
        )
        .unwrap();
        c.create(&dir).unwrap();
        assert_eq!(NodeConfig::load(&dir).unwrap(), c);
        assert!(c.create(&dir).unwrap_err().contains("already set up"));
        std::fs::write(NodeConfig::path(&dir), r#"{"v":1,"extra":true}"#).unwrap();
        assert!(NodeConfig::load(&dir).is_err());
        // An unknown time zone is refused at run too.
        let text = serde_json::to_string(&c)
            .unwrap()
            .replace("Europe/Berlin", "Mars/Olympus");
        std::fs::write(NodeConfig::path(&dir), text).unwrap();
        assert!(NodeConfig::load(&dir).unwrap_err().contains("Mars/Olympus"));
        // A node.json from before time zones names the file and the line to add.
        let old = serde_json::to_string(&c).unwrap().replace(
            r#""time_zone":"Europe/Berlin""#,
            r#""utc_offset_minutes":60"#,
        );
        std::fs::write(NodeConfig::path(&dir), old).unwrap();
        let e = NodeConfig::load(&dir).unwrap_err();
        assert!(
            e.contains("node.json") && e.contains("\"time_zone\": \""),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(NodeConfig::load(&dir).unwrap_err().contains("dn node init"));
    }

    #[cfg(unix)]
    #[test]
    fn a_data_folder_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("dn-node-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(check_private_dir(&dir).unwrap_err().contains("chmod 700"));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(check_private_dir(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn secrets_in_the_environment_are_found_by_name_only() {
        let vars = [
            ("OPENAI_API_KEY", "sk-test"),
            ("PATH", "/usr/bin"),
            ("DEMIOS_PROVIDER_KEY", "x"),
            ("DEMIOS_DATA_DIR", "/var/lib/demios"),
            ("DEMIOS_KEYBOARD_LAYOUT", "de"),
            ("HF_TOKEN", ""),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        assert_eq!(
            secret_env_vars(vars),
            ["DEMIOS_PROVIDER_KEY", "OPENAI_API_KEY"]
        );
    }
}
