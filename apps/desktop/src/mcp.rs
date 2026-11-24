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
        if !valid_value_name(name, what == "header") {
            return Err(invalid(format!(
                "The {what} name {name:?} has characters it can't contain."
            )));
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

/// What starting a local server will run, as the confirmation shows it. Plain env values are
/// shown and approved (`NODE_OPTIONS`, `LD_PRELOAD`… change what runs); secret values are
/// never shown, only their names.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LaunchRequest {
    pub server_name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Name and plain value; `None` for a secret.
    pub env: Vec<(String, Option<String>)>,
}

impl LaunchRequest {
    pub fn of(cfg: &McpServerConfig) -> Option<Self> {
        match &cfg.transport {
            McpTransport::Stdio {
                command, args, env, ..
            } => Some(Self {
                server_name: cfg.display_name.clone().unwrap_or_else(|| cfg.id.clone()),
                command: command.clone(),
                args: args.clone(),
                env: env
                    .iter()
                    .map(|(k, v)| {
                        let plain = match v {
                            ValueRef::Plain { value } => Some(value.clone()),
                            ValueRef::Secret { .. } => None,
                        };
                        (k.clone(), plain)
                    })
                    .collect(),
            }),
            McpTransport::StreamableHttp { .. } => None,
        }
    }

    /// What an approval is remembered for: the exact program, arguments, env var names and
    /// plain env values (a secret's value isn't part of it; its name is).
    pub fn command_line(&self) -> String {
        serde_json::to_string(&(&self.command, &self.args, &self.env)).unwrap_or_default()
    }

    /// The native dialog's text: every argument and plain env value on its own line, quoted,
    /// so nothing hides. Secret values are never shown.
    pub fn dialog_text(&self) -> String {
        let mut t = format!(
            "Demios is about to start this program for the tool server \"{}\":\n\n{:?}",
            self.server_name, self.command
        );
        for a in &self.args {
            t.push_str(&format!("\n    {a:?}"));
        }
        if !self.env.is_empty() {
            t.push_str("\n\nEnvironment variables:");
            for (name, value) in &self.env {
                match value {
                    Some(v) => t.push_str(&format!("\n    {name}={v:?}")),
                    None => t.push_str(&format!("\n    {name} (secret, not shown)")),
                }
            }
        }
        t.push_str(
            "\n\nIt runs with your permissions and can read and change your files. Allow it only if you trust it. You'll be asked again if the command changes.",
        );
        t
    }
}

/// Asks the user, outside the webview, whether a local server may start.
pub trait LaunchApprover: Send + Sync {
    fn ask(
        &self,
        request: LaunchRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>;
}

/// Env var and header names: no control characters (NUL, CR, LF…) or `=`; header names are
/// HTTP tokens (no spaces or `:`).
pub fn valid_value_name(name: &str, header: bool) -> bool {
    !name.is_empty()
        && name.chars().all(|c| {
            !c.is_control()
                && c != '='
                && !(header && (c.is_whitespace() || c == ':' || !c.is_ascii()))
        })
}

/// Where a server's data goes: its URL, or the program it starts. When this changes, stored
/// secrets and the sign-in must not follow it.
pub fn destination(cfg: &McpServerConfig) -> String {
    match &cfg.transport {
        // The arguments are part of what runs (`npx pkg-a` vs `npx pkg-b`).
        McpTransport::Stdio { command, args, .. } => format!(
            "stdio:{}",
            serde_json::to_string(&(command, args)).unwrap_or_default()
        ),
        McpTransport::StreamableHttp { url, .. } => format!("http:{}", url.trim()),
    }
}

/// An edit keeps what the form doesn't show: timeouts, annotation trust and the working
/// directory stay as they were.
pub fn merge_edit(existing: &McpServerConfig, edited: McpServerConfig) -> McpServerConfig {
    let mut out = existing.clone();
    out.display_name = edited.display_name;
    out.transport = match (edited.transport, &existing.transport) {
        (
            McpTransport::Stdio {
                command, args, env, ..
            },
            McpTransport::Stdio { cwd, .. },
        ) => McpTransport::Stdio {
            command,
            args,
            env,
            cwd: cwd.clone(),
        },
        (t, _) => t,
    };
    out
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
            local_servers_allowed: self.local_servers,
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
        let existing = editing.and_then(|old| servers.iter().find(|s| s.id == old).cloned());
        let (edited, writes) = config_from_input(input, self.mcp_has_stored(), self.local_servers)?;
        let moved = existing
            .as_ref()
            .is_some_and(|e| destination(e) != destination(&edited));
        if let Some(e) = &existing {
            if std::mem::discriminant(&e.transport) != std::mem::discriminant(&edited.transport) {
                return Err(invalid(
                    "A server can't switch between running on this computer and remote. Remove it and add it again.",
                ));
            }
            // Saved secrets were given for the old address or program; never send them to a
            // new one. The user types them again.
            if moved && let Some(name) = kept_secret(input) {
                return Err(invalid(format!(
                    "The server's address or program changed, so saved secrets aren't reused. Type {name} again."
                )));
            }
        }
        let cfg = match &existing {
            Some(e) => merge_edit(e, edited),
            None => edited,
        };
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
        if moved {
            // The sign-in and "allow in this chat" grants were for the old destination.
            self.forget_mcp_sign_in(&cfg.id)?;
            self.clear_mcp_grants(&cfg.id);
        }
        Ok(cfg)
    }

    /// After a sign-in finished: keeps the new token only if the server is still saved as it
    /// was when sign-in started; otherwise deletes the token and returns false. Blocking.
    pub fn keep_sign_in_for(&self, started_for: &McpServerConfig) -> IpcResult<bool> {
        match self.mcp_server(&started_for.id) {
            Ok(now) if &now == started_for => Ok(true),
            _ => self.forget_mcp_sign_in(&started_for.id).map(|()| false),
        }
    }

    /// Deletes the stored OAuth token of a server id (whether or not it uses sign-in now).
    pub fn forget_mcp_sign_in(&self, id: &str) -> IpcResult<()> {
        let sid = oauth_secret_id(id);
        self.keys.store.delete(&dn_secrets::SecretId::mcp(&sid))?;
        crate::settings::forget_mcp_secret_id(&*self.db()?, &sid)?;
        Ok(())
    }

    /// Forgets "allow in this chat" grants for every tool of a server.
    pub fn clear_mcp_grants(&self, id: &str) {
        if let Some(exec) = &self.executor {
            exec.gate().clear_capability_prefix(&format!("mcp.{id}."));
        }
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
        self.forget_mcp_sign_in(id)?;
        self.clear_mcp_grants(id);
        crate::settings::forget_mcp_launches(&*self.db()?, id)?;
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
pub fn safe_sign_in_url(raw: &str) -> bool {
    let Ok(u) = url::Url::parse(raw) else {
        return false;
    };
    if !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    match (u.scheme(), u.host()) {
        ("https", Some(url::Host::Domain(d))) => !d.is_empty(),
        ("https", Some(_)) => true,
        ("http", Some(url::Host::Domain(d))) => d.eq_ignore_ascii_case("localhost"),
        ("http", Some(url::Host::Ipv4(ip))) => ip.is_loopback(),
        ("http", Some(url::Host::Ipv6(ip))) => ip.is_loopback(),
        _ => false,
    }
}

/// The first secret the form keeps (no new value), if any.
fn kept_secret(input: &ServerInput) -> Option<String> {
    let vals = match &input.transport {
        TransportInput::Stdio { env, .. } => env,
        TransportInput::StreamableHttp { headers, .. } => headers,
    };
    vals.iter()
        .find(|v| v.secret && v.value.as_deref().is_none_or(|x| x.trim().is_empty()))
        .map(|v| v.name.trim().to_owned())
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

    // ---- review fixes

    /// App state with a tool executor; the returned receiver answers approval requests.
    fn app_with_exec() -> (
        Arc<AppState>,
        Arc<dn_tools::Executor>,
        crate::secrets::Keys,
        tokio::sync::mpsc::Receiver<dn_tools::ApprovalRequest>,
    ) {
        let conn = dn_store::open_in_memory(&crate::state::migration_sets()).unwrap();
        let keys = crate::secrets::Keys::memory();
        let mut st = AppState::new(conn, crate::models::ModelRegistry::default(), keys.clone());
        let ledger = dn_store::open_in_memory(&crate::state::migration_sets()).unwrap();
        let (gate, rx) = dn_tools::ApprovalGate::new(4);
        let exec = Arc::new(dn_tools::Executor::new(
            Arc::new(gate),
            Arc::new(std::sync::Mutex::new(ledger)),
        ));
        st.executor = Some(Arc::clone(&exec));
        (Arc::new(st), exec, keys, rx)
    }

    /// Grants "allow in this chat" for `cap` in `conv` through the real gate.
    async fn grant(
        exec: &dn_tools::Executor,
        rx: &mut tokio::sync::mpsc::Receiver<dn_tools::ApprovalRequest>,
        conv: &str,
        cap: &str,
    ) {
        use dn_core::capability::{ApprovalPolicy, RiskTier};
        let prompt = dn_tools::ApprovalPrompt {
            request_id: dn_core::Id::new(),
            scope: conv.into(),
            capability_id: cap.into(),
            title: cap.into(),
            description: String::new(),
            risk: RiskTier::LowWrite,
            policy: ApprovalPolicy::AskOncePerConversation,
            arguments: serde_json::json!({}),
        };
        let (_, req) = tokio::join!(exec.gate().check(prompt), rx.recv());
        let _ = req;
    }

    fn granted(exec: &dn_tools::Executor, conv: &str, cap: &str) -> bool {
        exec.gate().is_satisfied(
            conv,
            cap,
            dn_core::capability::ApprovalPolicy::AskOncePerConversation,
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_address_needs_secrets_retyped_and_drops_sign_in_and_grants() {
        let (st, exec, keys, mut rx) = app_with_exec();
        let responder = tokio::spawn(async move {
            while let Some(r) = rx.recv().await {
                r.respond(dn_tools::ApprovalDecision::ApproveForConversation);
            }
        });
        st.save_mcp_server(
            &http(
                "work",
                vec![secret("Authorization", Some("Bearer t0k"))],
                true,
            ),
            None,
        )
        .unwrap();
        st.mcp_secret_store().set("mcp.work.oauth", "{}").unwrap();
        let mut dummy = tokio::sync::mpsc::channel(1).1;
        grant(&exec, &mut dummy, "conv:a", "mcp.work.write").await;
        assert!(granted(&exec, "conv:a", "mcp.work.write"));

        let moved = |headers| ServerInput {
            id: "work".into(),
            name: None,
            transport: TransportInput::StreamableHttp {
                url: "https://evil.example/mcp".into(),
                headers,
                oauth: true,
                scopes: vec![],
                client_id: None,
            },
        };
        // Keeping the saved secret for a new host is refused (it would be sent there).
        let err = st
            .save_mcp_server(&moved(vec![secret("Authorization", None)]), Some("work"))
            .unwrap_err();
        assert!(
            err.message.contains("Type Authorization again"),
            "{}",
            err.message
        );
        assert!(has(&keys, "mcp.work.oauth"), "nothing changed on refusal");

        // Retyped: saved, and the old sign-in and chat grants are gone.
        st.save_mcp_server(
            &moved(vec![secret("Authorization", Some("Bearer new"))]),
            Some("work"),
        )
        .unwrap();
        assert!(!has(&keys, "mcp.work.oauth"));
        assert!(!granted(&exec, "conv:a", "mcp.work.write"));
        responder.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn removing_a_server_clears_its_grants() {
        let (st, exec, _keys, mut rx) = app_with_exec();
        let responder = tokio::spawn(async move {
            while let Some(r) = rx.recv().await {
                r.respond(dn_tools::ApprovalDecision::ApproveForConversation);
            }
        });
        st.save_mcp_server(&http("work", vec![], false), None)
            .unwrap();
        let mut dummy = tokio::sync::mpsc::channel(1).1;
        grant(&exec, &mut dummy, "conv:a", "mcp.work.write").await;
        grant(&exec, &mut dummy, "conv:a", "mcp.other.write").await;
        st.remove_mcp_server_config("work").unwrap();
        assert!(!granted(&exec, "conv:a", "mcp.work.write"));
        assert!(granted(&exec, "conv:a", "mcp.other.write"));
        responder.abort();
    }

    #[test]
    fn an_edit_keeps_hidden_settings_and_never_flips_trust() {
        let (st, _) = app();
        let mut cfg = McpServerConfig::http("work", "https://mcp.example.test/mcp");
        cfg.trust_annotations = Some(false);
        cfg.request_timeout_ms = 5_000;
        let mut local = McpServerConfig::stdio("files", "/bin/mcp-files", vec![]);
        if let McpTransport::Stdio { cwd, .. } = &mut local.transport {
            *cwd = Some("/srv".into());
        }
        st.set_mcp_servers(vec![cfg, local]).unwrap();

        st.save_mcp_server(&http("work", vec![], false), Some("work"))
            .unwrap();
        let w = st.mcp_server("work").unwrap();
        assert_eq!(w.trust_annotations, Some(false));
        assert_eq!(w.request_timeout_ms, 5_000);
        assert_eq!(w.display_name.as_deref(), Some("Work"));

        let edit = ServerInput {
            id: "files".into(),
            name: None,
            transport: TransportInput::Stdio {
                command: "/bin/mcp-files".into(),
                args: vec!["-v".into()],
                env: vec![],
            },
        };
        st.save_mcp_server(&edit, Some("files")).unwrap();
        let McpTransport::Stdio { cwd, args, .. } = st.mcp_server("files").unwrap().transport
        else {
            panic!()
        };
        assert_eq!(cwd.as_deref(), Some("/srv"));
        assert_eq!(args, ["-v"]);

        // Switching kind is refused.
        let err = st
            .save_mcp_server(&http("files", vec![], false), Some("files"))
            .unwrap_err();
        assert!(err.message.contains("can't switch"));
    }

    #[test]
    fn names_with_control_characters_or_equals_are_refused() {
        assert!(valid_value_name("API_TOKEN", false));
        assert!(valid_value_name("X-Team", true));
        for bad in ["A=B", "A\nB", "A\rB", "A\0B"] {
            assert!(!valid_value_name(bad, false), "{bad:?}");
        }
        assert!(!valid_value_name("X Team", true));
        assert!(!valid_value_name("X:Team", true));
        let input = http("work", vec![secret("X\r\nInjected", Some("v"))], false);
        assert!(config_from_input(&input, |_| false, true).is_err());
    }

    #[test]
    fn sign_in_urls_are_parsed_not_split() {
        assert!(!safe_sign_in_url("http://evil.com\\\u{ff20}127.0.0.1/"));
        assert!(!safe_sign_in_url("http://evil.com\\@127.0.0.1/"));
        assert!(!safe_sign_in_url("https://user:pw@auth.example.com/"));
        assert!(safe_sign_in_url("https://auth.example.com/authorize"));
        assert!(safe_sign_in_url("http://[::1]:9/cb"));
    }

    /// Answers every launch question with `allow` and counts the questions.
    struct FakeApprover {
        allow: bool,
        asked: std::sync::atomic::AtomicUsize,
    }

    impl LaunchApprover for FakeApprover {
        fn ask(
            &self,
            _request: LaunchRequest,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let allow = self.allow;
            Box::pin(async move { allow })
        }
    }

    fn local(id: &str, args: &[&str]) -> McpServerConfig {
        // A program that doesn't exist: connecting fails fast after the approval step.
        McpServerConfig::stdio(
            id,
            "/nonexistent/demios-test-mcp",
            args.iter().map(|a| a.to_string()).collect(),
        )
    }

    fn state_of(st: &AppState, id: &str) -> Option<McpServerStatus> {
        st.mcp_statuses().get(id).cloned()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_server_starts_only_after_a_native_yes_per_command_line() {
        let (st, exec, _keys, _rx) = app_with_exec();
        let approver = Arc::new(FakeApprover {
            allow: false,
            asked: Default::default(),
        });
        let _ = st.launch_approver.set(approver.clone());
        st.set_mcp_servers(vec![local("files", &["--root", "/tmp"])])
            .unwrap();
        let epoch = st.mcp_epoch_now();

        st.connect_mcp_server(&exec, local("files", &["--root", "/tmp"]), epoch)
            .await;
        assert_eq!(approver.asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        let s = state_of(&st, "files").unwrap();
        assert_eq!(s.state, "failed");
        assert!(s.message.unwrap().contains("didn't allow"));
        assert!(exec.client("files").is_none(), "never started");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_approval_is_remembered_until_the_command_line_changes() {
        let (st, exec, _keys, _rx) = app_with_exec();
        let approver = Arc::new(FakeApprover {
            allow: true,
            asked: Default::default(),
        });
        let _ = st.launch_approver.set(approver.clone());
        let asked = || approver.asked.load(std::sync::atomic::Ordering::SeqCst);
        let epoch = st.mcp_epoch_now();

        st.set_mcp_servers(vec![local("files", &["--root", "/tmp"])])
            .unwrap();
        st.connect_mcp_server(&exec, local("files", &["--root", "/tmp"]), epoch)
            .await;
        st.connect_mcp_server(&exec, local("files", &["--root", "/tmp"]), epoch)
            .await;
        assert_eq!(asked(), 1, "remembered for the same command line");

        st.set_mcp_servers(vec![local("files", &["--root", "/"])])
            .unwrap();
        st.connect_mcp_server(&exec, local("files", &["--root", "/"]), epoch)
            .await;
        assert_eq!(asked(), 2, "a changed argument asks again");

        // A plain env value is part of what runs (NODE_OPTIONS, LD_PRELOAD…): asks again.
        let mut with_env = local("files", &["--root", "/"]);
        if let McpTransport::Stdio { env, .. } = &mut with_env.transport {
            env.insert(
                "NODE_OPTIONS".into(),
                ValueRef::plain("--require /tmp/x.js"),
            );
        }
        st.set_mcp_servers(vec![with_env.clone()]).unwrap();
        st.connect_mcp_server(&exec, with_env.clone(), epoch).await;
        assert_eq!(asked(), 3, "a changed plain env value asks again");
        if let McpTransport::Stdio { env, .. } = &mut with_env.transport {
            env.insert(
                "NODE_OPTIONS".into(),
                ValueRef::plain("--require /tmp/y.js"),
            );
        }
        st.set_mcp_servers(vec![with_env.clone()]).unwrap();
        st.connect_mcp_server(&exec, with_env.clone(), epoch).await;
        assert_eq!(asked(), 4);
        st.set_mcp_servers(vec![local("files", &["--root", "/"])])
            .unwrap();

        // Removing the server forgets its approvals.
        st.remove_mcp_server_config("files").unwrap();
        assert!(
            !crate::settings::mcp_launch_approved(
                &st.db().unwrap(),
                "files",
                &LaunchRequest::of(&local("files", &["--root", "/"]))
                    .unwrap()
                    .command_line()
            )
            .unwrap()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_local_server_starts_where_programs_cant_run_or_without_an_approver() {
        let (st, exec, _keys, _rx) = app_with_exec();
        st.set_mcp_servers(vec![local("files", &[])]).unwrap();
        // No approver installed: refused, not started. (Starting would register a client
        // even for a program that fails to run, so this fails if the check is skipped.)
        st.connect_mcp_server(&exec, local("files", &[]), st.mcp_epoch_now())
            .await;
        let s = state_of(&st, "files").unwrap();
        assert_eq!(s.state, "failed");
        assert!(s.message.unwrap().contains("didn't allow"));
        assert!(exec.client("files").is_none(), "never started");

        let mut flat = AppState::new(
            dn_store::open_in_memory(&crate::state::migration_sets()).unwrap(),
            crate::models::ModelRegistry::default(),
            crate::secrets::Keys::memory(),
        );
        flat.local_servers = false;
        let flat = Arc::new(flat);
        let approver = Arc::new(FakeApprover {
            allow: true,
            asked: Default::default(),
        });
        let _ = flat.launch_approver.set(approver.clone());
        flat.set_mcp_servers(vec![local("files", &[])]).unwrap();
        flat.connect_mcp_server(&exec, local("files", &[]), flat.mcp_epoch_now())
            .await;
        let s = state_of(&flat, "files").unwrap();
        assert_eq!(s.state, "failed");
        assert!(s.message.unwrap().contains("can't start programs"));
        assert_eq!(approver.asked.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_connect_for_a_changed_or_removed_server_does_nothing() {
        let (st, exec, _keys, _rx) = app_with_exec();
        let approver = Arc::new(FakeApprover {
            allow: true,
            asked: Default::default(),
        });
        let _ = st.launch_approver.set(approver.clone());
        st.set_mcp_servers(vec![local("files", &["new"])]).unwrap();
        // A stale config (e.g. a save's background connect after another edit).
        st.connect_mcp_server(&exec, local("files", &["old"]), st.mcp_epoch_now())
            .await;
        // Not in settings at all.
        st.connect_mcp_server(&exec, local("gone", &[]), st.mcp_epoch_now())
            .await;
        assert_eq!(approver.asked.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(state_of(&st, "files").is_none() && state_of(&st, "gone").is_none());
    }

    #[test]
    fn the_launch_dialog_shows_every_argument_and_plain_env_values_but_no_secrets() {
        let mut cfg = local("files", &["--root", "/tmp; rm -rf ~", ""]);
        if let McpTransport::Stdio { env, .. } = &mut cfg.transport {
            env.insert(
                "API_TOKEN".into(),
                ValueRef::secret("mcp.files.env.API_TOKEN"),
            );
            env.insert("LOG".into(), ValueRef::plain("shown-plain-value"));
        }
        let r = LaunchRequest::of(&cfg).unwrap();
        let t = r.dialog_text();
        assert!(t.contains("\"/nonexistent/demios-test-mcp\""));
        assert!(t.contains("\"/tmp; rm -rf ~\""), "{t}");
        assert!(t.contains("\"\""), "an empty argument is visible");
        assert!(t.contains("API_TOKEN (secret, not shown)"), "{t}");
        assert!(t.contains("LOG=\"shown-plain-value\""), "{t}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_all_local_data_forgets_launch_approvals() {
        let (st, exec, _keys, _rx) = app_with_exec();
        let approver = Arc::new(FakeApprover {
            allow: true,
            asked: Default::default(),
        });
        let _ = st.launch_approver.set(approver.clone());
        let cfg = local("files", &["a"]);
        st.set_mcp_servers(vec![cfg.clone()]).unwrap();
        st.connect_mcp_server(&exec, cfg.clone(), st.mcp_epoch_now())
            .await;
        let line = LaunchRequest::of(&cfg).unwrap().command_line();
        assert!(crate::settings::mcp_launch_approved(&st.db().unwrap(), "files", &line).unwrap());
        assert!(st.delete_all_local_data().await.is_empty());
        assert!(!crate::settings::mcp_launch_approved(&st.db().unwrap(), "files", &line).unwrap());
    }

    #[test]
    fn a_sign_in_for_a_changed_or_removed_server_is_thrown_away() {
        let (st, keys) = app();
        st.save_mcp_server(&http("work", vec![], true), None)
            .unwrap();
        let started_for = st.mcp_server("work").unwrap();
        let store = st.mcp_secret_store();

        // Unchanged: kept.
        store.set("mcp.work.oauth", "{}").unwrap();
        assert!(st.keep_sign_in_for(&started_for).unwrap());
        assert!(has(&keys, "mcp.work.oauth"));

        // Edited while the browser was open: discarded.
        let moved = ServerInput {
            id: "work".into(),
            name: Some("Work".into()),
            transport: TransportInput::StreamableHttp {
                url: "https://elsewhere.example/mcp".into(),
                headers: vec![],
                oauth: true,
                scopes: vec![],
                client_id: None,
            },
        };
        st.save_mcp_server(&moved, Some("work")).unwrap();
        store.set("mcp.work.oauth", "{}").unwrap(); // the token the browser just delivered
        assert!(!st.keep_sign_in_for(&started_for).unwrap());
        assert!(!has(&keys, "mcp.work.oauth"));

        // Removed meanwhile: discarded too.
        let now = st.mcp_server("work").unwrap();
        st.remove_mcp_server_config("work").unwrap();
        store.set("mcp.work.oauth", "{}").unwrap();
        assert!(!st.keep_sign_in_for(&now).unwrap());
        assert!(!has(&keys, "mcp.work.oauth"));
    }

    #[test]
    fn new_arguments_count_as_a_new_destination_for_saved_secrets() {
        let (st, keys) = app();
        let input = |args: Vec<String>, value: Option<&str>| ServerInput {
            id: "pkg".into(),
            name: None,
            transport: TransportInput::Stdio {
                command: "npx".into(),
                args,
                env: vec![secret("API_TOKEN", value)],
            },
        };
        st.save_mcp_server(&input(vec!["pkg-a".into()], Some("t0k")), None)
            .unwrap();
        let err = st
            .save_mcp_server(&input(vec!["pkg-b".into()], None), Some("pkg"))
            .unwrap_err();
        assert!(
            err.message.contains("Type API_TOKEN again"),
            "{}",
            err.message
        );
        // Same arguments: the saved secret is kept.
        st.save_mcp_server(&input(vec!["pkg-a".into()], None), Some("pkg"))
            .unwrap();
        assert!(has(&keys, "mcp.pkg.env.API_TOKEN"));
    }
}
