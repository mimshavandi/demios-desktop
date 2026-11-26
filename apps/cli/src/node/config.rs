//! The Node's configuration: `node.json` in its data folder, written once by
//! `dn node init` and read by `dn node run`. It holds no secrets: only the choice of
//! secret store, the address to listen on, the Node's name and its time zone offset.

use dn_secrets::server::{KeySource, ServerStoreChoice};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::net::SocketAddr;
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub v: u32,
    /// The Node's name on every device's list.
    pub name: String,
    pub listen: SocketAddr,
    pub secrets: SecretsConfig,
    /// Local time offset from UTC in minutes, for jobs at a time of day.
    pub utc_offset_minutes: i32,
}

impl NodeConfig {
    pub fn new(
        name: String,
        listen: SocketAddr,
        secrets: SecretsConfig,
        utc_offset_minutes: i32,
    ) -> Result<Self, String> {
        let config = Self {
            v: CONFIG_VERSION,
            name,
            listen,
            secrets,
            utc_offset_minutes,
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
        if !(-14 * 60..=14 * 60).contains(&self.utc_offset_minutes) {
            return Err("--utc-offset must be between -14:00 and +14:00".into());
        }
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
        let config: Self = serde_json::from_str(&text)
            .map_err(|e| format!("{} isn't valid: {e}", path.display()))?;
        config.check()?;
        Ok(config)
    }

    /// Writes `node.json`. Refuses to replace an existing one.
    pub fn create(&self, data_dir: &Path) -> Result<(), String> {
        create_private_dir(data_dir)?;
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

/// Parses `+01:00`, `-05:30` or `0` into minutes.
pub fn parse_utc_offset(text: &str) -> Result<i32, String> {
    let bad = || format!("`{text}` isn't a UTC offset like +01:00 or -05:30");
    if text == "0" || text.eq_ignore_ascii_case("z") {
        return Ok(0);
    }
    let (sign, rest) = match text.as_bytes().first() {
        Some(b'+') => (1, &text[1..]),
        Some(b'-') => (-1, &text[1..]),
        _ => return Err(bad()),
    };
    let (h, m) = rest.split_once(':').ok_or_else(bad)?;
    let h: i32 = h.parse().map_err(|_| bad())?;
    let m: i32 = m.parse().map_err(|_| bad())?;
    if h > 14 || !(0..60).contains(&m) || m % 15 != 0 {
        return Err(bad());
    }
    Ok(sign * (h * 60 + m))
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
                    && ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSPHRASE"]
                        .iter()
                        .any(|w| upper.contains(w)))
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
    fn config_round_trips_and_is_written_once() {
        let dir = std::env::temp_dir().join(format!("dn-node-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let c = NodeConfig::new(
            "Home server".into(),
            "127.0.0.1:7443".parse().unwrap(),
            SecretsConfig::ContainerSecret {
                name: "demios".into(),
            },
            60,
        )
        .unwrap();
        c.create(&dir).unwrap();
        assert_eq!(NodeConfig::load(&dir).unwrap(), c);
        assert!(c.create(&dir).unwrap_err().contains("already set up"));
        std::fs::write(NodeConfig::path(&dir), r#"{"v":1,"extra":true}"#).unwrap();
        assert!(NodeConfig::load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(NodeConfig::load(&dir).unwrap_err().contains("dn node init"));
    }

    #[test]
    fn utc_offsets_parse() {
        assert_eq!(parse_utc_offset("+01:00").unwrap(), 60);
        assert_eq!(parse_utc_offset("-05:30").unwrap(), -330);
        assert_eq!(parse_utc_offset("+05:45").unwrap(), 345);
        assert_eq!(parse_utc_offset("0").unwrap(), 0);
        for bad in ["1", "+1", "+15:00", "+01:07", "UTC"] {
            assert!(parse_utc_offset(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn secrets_in_the_environment_are_found_by_name_only() {
        let vars = [
            ("OPENAI_API_KEY", "sk-test"),
            ("PATH", "/usr/bin"),
            ("DEMIOS_PROVIDER_KEY", "x"),
            ("DEMIOS_DATA_DIR", "/var/lib/demios"),
            ("HF_TOKEN", ""),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        assert_eq!(
            secret_env_vars(vars),
            ["DEMIOS_PROVIDER_KEY", "OPENAI_API_KEY"]
        );
    }
}
