//! MCP servers screen (T-SHELL-08, issue #49): add, edit and remove tool servers, sign in
//! and out of OAuth servers, and per-server status.
//!
//! Secrets never reach the webview: the UI sends a new value only when the user types one,
//! and gets back whether a value is stored. Values are written to the keychain under ids
//! derived from the server (`mcp.<server>.env.<NAME>`, `mcp.<server>.header.<NAME>`,
//! `mcp.<server>.oauth`) through the tracked store, so "Delete all local data" finds them.

use crate::error::{IpcError, IpcResult};
use crate::state::{AppState, McpServerStatus};
use dn_tools::{McpServerConfig, McpTransport, OAuthSettings, ValueRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// One env var or header as the screen shows it. Secret values are never included.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ValueView {
    pub name: String,
    pub secret: bool,
    /// Plain values only.
    pub value: Option<String>,
    /// Secret values: whether one is stored.
    pub stored: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OAuthView {
    pub scopes: Vec<String>,
    pub client_id: Option<String>,
    pub signed_in: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransportView {
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<ValueView>,
    },
    StreamableHttp {
        url: String,
        headers: Vec<ValueView>,
        oauth: Option<OAuthView>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ServerView {
    pub id: String,
    pub name: String,
    pub transport: TransportView,
    pub status: Option<McpServerStatus>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ServersView {
    pub servers: Vec<ServerView>,
    /// False in the Flatpak build, which can't start local programs: HTTP servers only.
    pub local_servers_allowed: bool,
}

/// An env var or header from the form. For a secret, `value: None` keeps the stored one.
#[derive(Clone, Debug, Deserialize)]
pub struct ValueInput {
    pub name: String,
    pub secret: bool,
    pub value: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransportInput {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<ValueInput>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        headers: Vec<ValueInput>,
        #[serde(default)]
        oauth: bool,
        #[serde(default)]
        scopes: Vec<String>,
        #[serde(default)]
        client_id: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize)]
pub struct ServerInput {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub transport: TransportInput,
}

/// Whether this build may start local (stdio) servers. Flatpak builds can't run host
/// programs, so they take HTTP servers only.
pub fn local_servers_allowed() -> bool {
    std::env::var_os("FLATPAK_ID").is_none()
}

pub fn env_secret_id(server: &str, name: &str) -> String {
    format!("mcp.{server}.env.{name}")
}
pub fn header_secret_id(server: &str, name: &str) -> String {
    format!("mcp.{server}.header.{}", name.to_ascii_lowercase())
}
pub fn oauth_secret_id(server: &str) -> String {
    format!("mcp.{server}.oauth")
}

fn invalid(msg: impl Into<String>) -> IpcError {
    IpcError::invalid(msg)
}

/// Secret writes a save will make (id, value), applied only after the config validates.
type SecretWrites = Vec<(String, String)>;

fn values(
    inputs: &[ValueInput],
    secret_id: impl Fn(&str) -> String,
    has_stored: impl Fn(&str) -> bool,
    what: &str,
    writes: &mut SecretWrites,
) -> IpcResult<BTreeMap<String, ValueRef>> {
    let mut out = BTreeMap::new();
    for v in inputs {
        let name = v.name.trim();
        if name.is_empty() {
            return Err(invalid(format!("Give every {what} a name.")));
        }
        if out.contains_key(name) {
            return Err(invalid(format!("The {what} {name} is listed twice.")));
        }
        let r = if v.secret {
            let id = secret_id(name);
            match v.value.as_deref().map(str::trim) {
                Some("") | None if !has_stored(&id) => {
                    return Err(invalid(format!("Enter a value for {name}.")));
                }
                Some(val) if !val.is_empty() => writes.push((id.clone(), val.to_owned())),
                _ => {} // keep the stored value
            }
            ValueRef::secret(id)
        } else {
            ValueRef::plain(v.value.clone().unwrap_or_default())
        };
        out.insert(name.to_owned(), r);
    }
    Ok(out)
}

/// Builds the config from the form and the secret writes it needs. Validates with dn-tools'
/// rules (ids, URLs, no plain values for secret-looking names). Nothing is stored here.
pub fn config_from_input(
    input: &ServerInput,
    has_stored: impl Fn(&str) -> bool,
    local_allowed: bool,
) -> IpcResult<(McpServerConfig, SecretWrites)> {
    let id = input.id.trim().to_owned();
    let mut writes = Vec::new();
    let mut cfg = match &input.transport {
        TransportInput::Stdio { command, args, env } => {
            if !local_allowed {
                return Err(invalid(
                    "This version of Demios can't start programs on your computer. Add an HTTP server instead.",
                ));
            }
            if command.trim().is_empty() {
                return Err(invalid("Enter the command that starts the server."));
            }
            let mut c = McpServerConfig::stdio(&id, command.trim(), args.clone());
            if let McpTransport::Stdio { env: e, .. } = &mut c.transport {
                *e = values(
                    env,
                    |n| env_secret_id(&id, n),
                    &has_stored,
                    "variable",
                    &mut writes,
                )?;
            }
            c
        }
        TransportInput::StreamableHttp {
            url,
            headers,
            oauth,
            scopes,
            client_id,
        } => {
            let mut c = McpServerConfig::http(&id, url.trim());
            if let McpTransport::StreamableHttp {
                headers: h,
                oauth: o,
                ..
            } = &mut c.transport
            {
                *h = values(
                    headers,
                    |n| header_secret_id(&id, n),
                    &has_stored,
                    "header",
                    &mut writes,
                )?;
                *o = oauth.then(|| OAuthSettings {
                    credential_secret_id: oauth_secret_id(&id),
                    scopes: scopes
                        .iter()
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty())
                        .collect(),
                    client_id: client_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                });
            }
            c
        }
    };
    cfg.display_name = input
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    cfg.validate().map_err(|e| {
        invalid(format!(
            "Check the server settings: {}",
            e.developer_message
        ))
    })?;
    Ok((cfg, writes))
}

fn value_views(
    m: &BTreeMap<String, ValueRef>,
    has_stored: &impl Fn(&str) -> bool,
) -> Vec<ValueView> {
    m.iter()
        .map(|(name, v)| match v {
            ValueRef::Plain { value } => ValueView {
                name: name.clone(),
                secret: false,
                value: Some(value.clone()),
                stored: false,
            },
            ValueRef::Secret { secret_id } => ValueView {
                name: name.clone(),
                secret: true,
                value: None,
                stored: has_stored(secret_id),
            },
        })
        .collect()
}

pub fn view(
    cfg: &McpServerConfig,
    has_stored: impl Fn(&str) -> bool,
    status: Option<McpServerStatus>,
) -> ServerView {
    let transport = match &cfg.transport {
        McpTransport::Stdio {
            command, args, env, ..
        } => TransportView::Stdio {
            command: command.clone(),
            args: args.clone(),
            env: value_views(env, &has_stored),
        },
        McpTransport::StreamableHttp {
            url,
            headers,
            oauth,
        } => TransportView::StreamableHttp {
            url: url.clone(),
            headers: value_views(headers, &has_stored),
            oauth: oauth.as_ref().map(|o| OAuthView {
                scopes: o.scopes.clone(),
                client_id: o.client_id.clone(),
                signed_in: has_stored(&o.credential_secret_id),
            }),
        },
    };
    ServerView {
        id: cfg.id.clone(),
        name: cfg.display_name.clone().unwrap_or_else(|| cfg.id.clone()),
        transport,
        status,
    }
}

impl AppState {
    fn mcp_has_stored(&self) -> impl Fn(&str) -> bool + '_ {
        |id| {
            self.keys
                .store
                .get(&dn_secrets::SecretId::mcp(id))
                .is_ok_and(|v| v.is_some())
        }
    }

    /// The servers screen. Blocking (keychain).
    pub fn mcp_servers_view(&self) -> IpcResult<ServersView> {
        let settings = self.settings()?;
        let statuses = self.mcp_statuses();
        let has = self.mcp_has_stored();
        Ok(ServersView {
            servers: settings
                .mcp_servers
                .iter()
                .map(|c| view(c, &has, statuses.get(&c.id).cloned()))
                .collect(),
            local_servers_allowed: local_servers_allowed(),
        })
    }

    /// Adds (`editing: None`) or replaces a server. Stores new secret values, saves the
    /// list (secrets no server refers to any more are deleted) and returns the config to
    /// connect. Blocking.
    pub fn save_mcp_server(
        self: &Arc<Self>,
        input: &ServerInput,
        editing: Option<&str>,
    ) -> IpcResult<McpServerConfig> {
        let mut servers = self.settings()?.mcp_servers;
        let id = input.id.trim();
        match editing {
            None if servers.iter().any(|s| s.id == id) => {
                return Err(invalid(format!(
                    "A server with the id {id} already exists. Choose another id."
                )));
            }
            Some(old) if old != id => {
                return Err(invalid(
                    "A server's id can't change. Remove it and add it again.",
                ));
            }
            Some(old) if !servers.iter().any(|s| s.id == old) => {
                return Err(IpcError::new("not_found", "That server no longer exists."));
            }
            _ => {}
        }
        let (cfg, writes) =
            config_from_input(input, self.mcp_has_stored(), local_servers_allowed())?;
        let store = self.mcp_secret_store();
        for (sid, value) in &writes {
            store.set(sid, value).map_err(|e| {
                eprintln!("could not store an MCP secret: {e}");
                IpcError::new(
                    "keychain",
                    "Couldn't save the secret to your system keychain.",
                )
            })?;
        }
        match servers.iter_mut().find(|s| s.id == cfg.id) {
            Some(s) => *s = cfg.clone(),
            None => servers.push(cfg.clone()),
        }
        self.set_mcp_servers(servers)?;
        Ok(cfg)
    }

    /// Removes a server from the list and deletes its secrets and sign-in. Blocking; the
    /// caller stops the running client.
    pub fn remove_mcp_server_config(&self, id: &str) -> IpcResult<()> {
        let mut servers = self.settings()?.mcp_servers;
        let before = servers.len();
        servers.retain(|s| s.id != id);
        if servers.len() == before {
            return Err(IpcError::new("not_found", "That server no longer exists."));
        }
        self.set_mcp_servers(servers)?;
        self.clear_mcp_status(id);
        Ok(())
    }

    /// Deletes a server's stored OAuth tokens. Blocking; the caller stops the running client.
    pub fn sign_out_mcp_server(&self, id: &str) -> IpcResult<McpServerConfig> {
        let cfg = self.mcp_server(id)?;
        let Some(sid) = oauth_id(&cfg) else {
            return Err(invalid("This server doesn't use sign-in."));
        };
        self.keys.store.delete(&dn_secrets::SecretId::mcp(&sid))?;
        crate::settings::forget_mcp_secret_id(&*self.db()?, &sid)?;
        Ok(cfg)
    }

    pub fn mcp_server(&self, id: &str) -> IpcResult<McpServerConfig> {
        self.settings()?
            .mcp_servers
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| IpcError::new("not_found", "That server no longer exists."))
    }
}

/// The sign-in page comes from the server's OAuth metadata: open it only if it is https, or
/// http on this computer (local test servers).
pub fn safe_sign_in_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.split(']').next())
        .unwrap_or_else(|| host.split(':').next().unwrap_or(""));
    match scheme.to_ascii_lowercase().as_str() {
        "https" => !host.is_empty(),
        "http" => matches!(host, "127.0.0.1" | "localhost" | "::1"),
        _ => false,
    }
}

pub fn oauth_id(cfg: &McpServerConfig) -> Option<String> {
    match &cfg.transport {
        McpTransport::StreamableHttp { oauth: Some(o), .. } => Some(o.credential_secret_id.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(id: &str, headers: Vec<ValueInput>, oauth: bool) -> ServerInput {
        ServerInput {
            id: id.into(),
            name: Some("Work".into()),
            transport: TransportInput::StreamableHttp {
                url: "https://mcp.example.test/mcp".into(),
                headers,
                oauth,
                scopes: vec!["read".into(), " ".into()],
                client_id: Some("  ".into()),
            },
        }
    }

    fn secret(name: &str, value: Option<&str>) -> ValueInput {
        ValueInput {
            name: name.into(),
            secret: true,
            value: value.map(str::to_owned),
        }
    }

    #[test]
    fn secrets_go_to_derived_ids_and_never_into_the_config() {
        let input = http(
            "work",
            vec![secret("Authorization", Some("Bearer t0k"))],
            true,
        );
        let (cfg, writes) = config_from_input(&input, |_| false, true).unwrap();
        assert_eq!(
            writes,
            [(
                "mcp.work.header.authorization".to_owned(),
                "Bearer t0k".to_owned()
            )]
        );
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("t0k"), "{json}");
        let McpTransport::StreamableHttp { oauth, .. } = &cfg.transport else {
            panic!()
        };
        let o = oauth.as_ref().unwrap();
        assert_eq!(o.credential_secret_id, "mcp.work.oauth");
        assert_eq!(o.scopes, ["read"]);
        assert_eq!(o.client_id, None);
        assert_eq!(cfg.display_name.as_deref(), Some("Work"));
    }

    #[test]
    fn an_empty_secret_keeps_the_stored_value_or_asks_for_one() {
        let input = http("work", vec![secret("Authorization", None)], false);
        let (_, writes) =
            config_from_input(&input, |id| id == "mcp.work.header.authorization", true).unwrap();
        assert!(writes.is_empty(), "kept");
        let err = config_from_input(&input, |_| false, true).unwrap_err();
        assert!(err.message.contains("Enter a value for Authorization"));
    }

    #[test]
    fn invalid_forms_say_what_to_fix() {
        let dup = http(
            "work",
            vec![secret("X-Key", Some("a")), secret("X-Key", Some("b"))],
            false,
        );
        assert!(
            config_from_input(&dup, |_| false, true)
                .unwrap_err()
                .message
                .contains("listed twice")
        );

        let bad_id = http("Not Valid!", vec![], false);
        assert!(
            config_from_input(&bad_id, |_| false, true)
                .unwrap_err()
                .message
                .starts_with("Check the server settings")
        );

        // A plain value for a secret-looking header is refused by dn-tools' rules.
        let plain_token = http(
            "work",
            vec![ValueInput {
                name: "Authorization".into(),
                secret: false,
                value: Some("Bearer x".into()),
            }],
            false,
        );
        assert!(config_from_input(&plain_token, |_| false, true).is_err());
    }

    #[test]
    fn only_secure_sign_in_pages_are_opened() {
        for ok in [
            "https://auth.example.com/authorize?x=1",
            "http://127.0.0.1:8123/authorize",
            "http://localhost/a",
            "http://[::1]:9/a",
        ] {
            assert!(safe_sign_in_url(ok), "{ok}");
        }
        for bad in [
            "http://auth.example.com/authorize",
            "http://127.0.0.1.evil.com/a",
            "http://user@evil.com/a",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://",
            "nonsense",
        ] {
            assert!(!safe_sign_in_url(bad), "{bad}");
        }
    }

    #[test]
    fn local_servers_are_refused_where_they_cant_run() {
        let input = ServerInput {
            id: "files".into(),
            name: None,
            transport: TransportInput::Stdio {
                command: "/usr/local/bin/mcp-files".into(),
                args: vec!["--root".into(), "/tmp".into()],
                env: vec![],
            },
        };
        assert!(config_from_input(&input, |_| false, true).is_ok());
        let err = config_from_input(&input, |_| false, false).unwrap_err();
        assert!(err.message.contains("can't start programs"));
    }

    #[test]
    fn views_show_whether_a_secret_is_stored_but_never_its_value() {
        let input = http(
            "work",
            vec![secret("Authorization", Some("Bearer t0k"))],
            true,
        );
        let (cfg, _) = config_from_input(&input, |_| false, true).unwrap();
        let v = view(&cfg, |id| id == "mcp.work.header.authorization", None);
        let json = serde_json::to_string(&v).unwrap();
        assert!(!json.contains("t0k"));
        let TransportView::StreamableHttp { headers, oauth, .. } = v.transport else {
            panic!()
        };
        assert!(headers[0].stored);
        assert_eq!(headers[0].value, None);
        assert!(!oauth.unwrap().signed_in);
    }

    fn app() -> (Arc<AppState>, crate::secrets::Keys) {
        let conn = dn_store::open_in_memory(&crate::state::migration_sets()).unwrap();
        let keys = crate::secrets::Keys::memory();
        let st = AppState::new(conn, crate::models::ModelRegistry::default(), keys.clone());
        (Arc::new(st), keys)
    }

    fn has(keys: &crate::secrets::Keys, id: &str) -> bool {
        keys.store
            .get(&dn_secrets::SecretId::mcp(id))
            .unwrap()
            .is_some()
    }

    #[test]
    fn add_edit_and_remove_keep_the_keychain_in_step() {
        let (st, keys) = app();
        let input = http(
            "work",
            vec![
                secret("Authorization", Some("Bearer t0k")),
                secret("X-Team", Some("blue")),
            ],
            true,
        );
        st.save_mcp_server(&input, None).unwrap();
        assert!(has(&keys, "mcp.work.header.authorization"));
        assert!(has(&keys, "mcp.work.header.x-team"));
        let err = st.save_mcp_server(&input, None).unwrap_err();
        assert!(err.message.contains("already exists"), "{}", err.message);

        // Edit: keep Authorization (no new value), drop X-Team.
        let edited = http("work", vec![secret("Authorization", None)], true);
        st.save_mcp_server(&edited, Some("work")).unwrap();
        assert!(has(&keys, "mcp.work.header.authorization"), "kept");
        assert!(
            !has(&keys, "mcp.work.header.x-team"),
            "removed header's secret deleted"
        );
        assert!(
            st.save_mcp_server(&edited, Some("other")).is_err(),
            "id can't change"
        );

        // A sign-in token written by dn-tools, then sign out.
        st.mcp_secret_store().set("mcp.work.oauth", "{}").unwrap();
        let v = st.mcp_servers_view().unwrap();
        let TransportView::StreamableHttp { oauth, .. } = &v.servers[0].transport else {
            panic!()
        };
        assert!(oauth.as_ref().unwrap().signed_in);
        st.sign_out_mcp_server("work").unwrap();
        assert!(!has(&keys, "mcp.work.oauth"));
        assert!(
            has(&keys, "mcp.work.header.authorization"),
            "sign-out keeps other secrets"
        );

        // Remove: everything of the server goes.
        st.mcp_secret_store().set("mcp.work.oauth", "{}").unwrap();
        st.remove_mcp_server_config("work").unwrap();
        assert!(st.mcp_servers_view().unwrap().servers.is_empty());
        assert!(!has(&keys, "mcp.work.header.authorization"));
        assert!(!has(&keys, "mcp.work.oauth"));
        assert!(st.remove_mcp_server_config("work").is_err());
    }

    #[test]
    fn sign_out_of_a_server_without_sign_in_is_refused() {
        let (st, _) = app();
        st.save_mcp_server(&http("plain", vec![], false), None)
            .unwrap();
        assert!(st.sign_out_mcp_server("plain").is_err());
        assert!(st.sign_out_mcp_server("missing").is_err());
    }
}
