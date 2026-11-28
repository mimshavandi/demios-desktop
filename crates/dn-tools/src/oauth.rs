//! OAuth for remote MCP servers (T-CON-04): authorization code + PKCE (S256) with discovery per
//! the MCP authorization spec (RFC 9728 protected-resource metadata → RFC 8414 / OIDC
//! authorization-server metadata, dynamic client registration when no client id is
//! configured). The protocol work is `rmcp`'s `auth` feature; this module wires it to the
//! app: tokens live in the [`SecretStore`] (keychain via `dn-secrets`), never in config files.
//!
//! Flow for the UI:
//! 1. [`OAuthManager::begin`] → open [`PendingAuthorization::authorize_url`] in the browser.
//! 2. Receive the redirect (e.g. with [`LoopbackRedirect`]).
//! 3. [`PendingAuthorization::complete`] with the full redirect URL → tokens are stored.
//! 4. Reconnect the server; [`McpClient`](crate::client::McpClient) picks the tokens up.

use crate::client::HttpAuthProvider;
use crate::config::{McpServerConfig, McpTransport, OAuthSettings};
use crate::error::{ToolError, ToolErrorKind};
use crate::secrets::SecretStore;
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationRequest, AuthorizationSession, CredentialStore,
    StoredCredentials,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CLIENT_NAME: &str = "Demios Desktop";

/// `rmcp` credential store backed by a [`SecretStore`] entry (JSON of the token set).
///
/// The trait is declared with `#[async_trait]`; we implement its desugared form by hand to
/// avoid an extra direct dependency.
pub struct SecretCredentialStore {
    secrets: Arc<dyn SecretStore>,
    secret_id: String,
}

impl SecretCredentialStore {
    pub fn new(secrets: Arc<dyn SecretStore>, secret_id: impl Into<String>) -> Self {
        Self {
            secrets,
            secret_id: secret_id.into(),
        }
    }

    fn load_sync(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let raw = self
            .secrets
            .get(&self.secret_id)
            .map_err(|e| AuthError::CredentialStoreError(e.to_string()))?;
        raw.map(|s| {
            serde_json::from_str(&s).map_err(|e| AuthError::CredentialStoreError(e.to_string()))
        })
        .transpose()
    }

    fn save_sync(&self, credentials: &StoredCredentials) -> Result<(), AuthError> {
        let s = serde_json::to_string(credentials)
            .map_err(|e| AuthError::CredentialStoreError(e.to_string()))?;
        self.secrets
            .set(&self.secret_id, &s)
            .map_err(|e| AuthError::CredentialStoreError(e.to_string()))
    }

    fn clear_sync(&self) -> Result<(), AuthError> {
        self.secrets
            .delete(&self.secret_id)
            .map_err(|e| AuthError::CredentialStoreError(e.to_string()))
    }
}

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

impl CredentialStore for SecretCredentialStore {
    fn load<'life0, 'async_trait>(
        &'life0 self,
    ) -> BoxFut<'async_trait, Result<Option<StoredCredentials>, AuthError>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.load_sync() })
    }

    fn save<'life0, 'async_trait>(
        &'life0 self,
        credentials: StoredCredentials,
    ) -> BoxFut<'async_trait, Result<(), AuthError>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.save_sync(&credentials) })
    }

    fn clear<'life0, 'async_trait>(&'life0 self) -> BoxFut<'async_trait, Result<(), AuthError>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.clear_sync() })
    }
}

/// Creates authorization managers for servers with `oauth` settings.
#[derive(Clone)]
pub struct OAuthManager {
    secrets: Arc<dyn SecretStore>,
}

impl std::fmt::Debug for OAuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthManager").finish_non_exhaustive()
    }
}

fn err(config: &McpServerConfig, kind: ToolErrorKind, msg: impl Into<String>) -> ToolError {
    ToolError::new(format!("mcp.{}", config.id), kind, msg)
}

fn auth_err(config: &McpServerConfig, e: AuthError) -> ToolError {
    let kind = match e {
        AuthError::AuthorizationRequired
        | AuthError::TokenExpired
        | AuthError::TokenRefreshRejected(_)
        | AuthError::InsufficientScope { .. } => ToolErrorKind::AuthRequired,
        AuthError::HttpError(_) | AuthError::DiscoveryRequestFailed { .. } => {
            ToolErrorKind::Connection
        }
        _ => ToolErrorKind::Config,
    };
    err(config, kind, format!("oauth: {e}"))
}

fn http_oauth(config: &McpServerConfig) -> Result<(&str, &OAuthSettings), ToolError> {
    match &config.transport {
        McpTransport::StreamableHttp {
            url,
            oauth: Some(o),
            ..
        } => Ok((url.as_str(), o)),
        _ => Err(err(
            config,
            ToolErrorKind::Config,
            "server is not an HTTP server with oauth settings",
        )),
    }
}

/// A started authorization, waiting for the browser redirect.
pub struct PendingAuthorization {
    session: AuthorizationSession,
    config: McpServerConfig,
}

impl std::fmt::Debug for PendingAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingAuthorization")
            .field("server_id", &self.config.id)
            .finish_non_exhaustive()
    }
}

impl PendingAuthorization {
    /// Open this in the user's browser. Contains `state` and the PKCE `code_challenge`.
    pub fn authorize_url(&self) -> &str {
        self.session.get_authorization_url()
    }

    pub fn redirect_uri(&self) -> &str {
        &self.session.redirect_uri
    }

    /// Exchange the code from the redirect URL (validates `state`, sends the PKCE verifier)
    /// and store the tokens.
    pub async fn complete(self, callback_url: &str) -> Result<(), ToolError> {
        self.session
            .handle_callback_url(callback_url)
            .await
            .map(|_| ())
            .map_err(|e| auth_err(&self.config, e))
    }
}

impl OAuthManager {
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self { secrets }
    }

    async fn manager(&self, config: &McpServerConfig) -> Result<AuthorizationManager, ToolError> {
        let (url, settings) = http_oauth(config)?;
        // `new` permits a loopback authorization server only when the MCP server itself is
        // loopback (local dev/test servers) and always rejects link-local/metadata addresses.
        let mut manager = AuthorizationManager::new(url)
            .await
            .map_err(|e| auth_err(config, e))?;
        manager.set_credential_store(SecretCredentialStore::new(
            self.secrets.clone(),
            settings.credential_secret_id.clone(),
        ));
        Ok(manager)
    }

    /// Start the authorization code + PKCE flow. `redirect_uri` must be where the app receives
    /// the redirect (see [`LoopbackRedirect`]).
    pub async fn begin(
        &self,
        config: &McpServerConfig,
        redirect_uri: &str,
    ) -> Result<PendingAuthorization, ToolError> {
        let (_, settings) = http_oauth(config)?;
        let mut manager = self.manager(config).await?;
        let resolution = manager
            .resolve_metadata()
            .await
            .map_err(|e| auth_err(config, e))?;
        manager.set_metadata(resolution.metadata);
        let mut request = AuthorizationRequest::new(redirect_uri).with_client_name(CLIENT_NAME);
        if !settings.scopes.is_empty() {
            request = request.with_scopes(settings.scopes.iter().cloned());
        }
        if let Some(client_id) = &settings.client_id {
            request = request.with_preregistered_client(client_id.clone());
        }
        let session = AuthorizationSession::new(manager, request)
            .await
            .map_err(|(_, e)| auth_err(config, e))?;
        Ok(PendingAuthorization {
            session,
            config: config.clone(),
        })
    }

    /// Whether tokens are stored for this server.
    pub fn has_credentials(&self, config: &McpServerConfig) -> Result<bool, ToolError> {
        let (_, settings) = http_oauth(config)?;
        self.secrets
            .get(&settings.credential_secret_id)
            .map(|v| v.is_some())
            .map_err(|e| err(config, ToolErrorKind::Config, e.to_string()))
    }

    /// Forget stored tokens ("disconnect account").
    pub fn sign_out(&self, config: &McpServerConfig) -> Result<(), ToolError> {
        let (_, settings) = http_oauth(config)?;
        self.secrets
            .delete(&settings.credential_secret_id)
            .map_err(|e| err(config, ToolErrorKind::Config, e.to_string()))
    }
}

impl HttpAuthProvider for OAuthManager {
    fn authorization_manager(
        &self,
        config: &McpServerConfig,
    ) -> Pin<Box<dyn Future<Output = Result<AuthorizationManager, ToolError>> + Send + '_>> {
        let config = config.clone();
        Box::pin(async move {
            let mut manager = self.manager(&config).await?;
            let ready = manager
                .initialize_from_store()
                .await
                .map_err(|e| auth_err(&config, e))?;
            if !ready {
                return Err(err(
                    &config,
                    ToolErrorKind::AuthRequired,
                    "no stored OAuth credentials; sign in first",
                ));
            }
            Ok(manager)
        })
    }
}

/// One-shot loopback HTTP listener for the OAuth redirect (RFC 8252 §7.3).
#[derive(Debug)]
pub struct LoopbackRedirect {
    listener: tokio::net::TcpListener,
    redirect_uri: String,
}

impl LoopbackRedirect {
    /// Bind `127.0.0.1` on an ephemeral port; redirect URI is `http://127.0.0.1:<port>/callback`.
    pub async fn bind() -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}/callback"),
        })
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Like [`Self::wait`], giving up with `TimedOut` after `timeout` (the user closed the
    /// browser tab, …).
    pub async fn wait_timeout(self, timeout: std::time::Duration) -> std::io::Result<String> {
        tokio::time::timeout(timeout, self.wait())
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "no OAuth redirect"))?
    }

    /// Wait for the browser to hit `/callback`; returns the full redirect URL. The page only
    /// says to return to the app: whether sign-in worked is known after
    /// [`PendingAuthorization::complete`]. Requests to other paths (favicon…) get a 404, and a
    /// connection that doesn't send a request line within 5 s is dropped.
    pub async fn wait(self) -> std::io::Result<String> {
        loop {
            let (mut sock, _) = self.listener.accept().await?;
            let mut buf = vec![0u8; 8192];
            let mut len = 0;
            let read_head = async {
                while len < buf.len() {
                    let n = sock.read(&mut buf[len..]).await?;
                    if n == 0 {
                        break;
                    }
                    len += n;
                    if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Ok::<(), std::io::Error>(())
            };
            match tokio::time::timeout(std::time::Duration::from_secs(5), read_head).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => continue,
            }
            let head = String::from_utf8_lossy(&buf[..len]);
            let target = head
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("")
                .to_owned();
            if target.starts_with("/callback") {
                let body = "<html><body>Almend received the sign-in response. You can close \
                            this window and return to the app.</body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await?;
                let base = self.redirect_uri.trim_end_matches("/callback");
                return Ok(format!("{base}{target}"));
            }
            let _ = sock
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecretStore;

    #[tokio::test]
    async fn credential_store_round_trip() {
        let secrets = Arc::new(MemorySecretStore::new());
        let store = SecretCredentialStore::new(secrets.clone(), "mcp.x.oauth");
        assert!(store.load().await.unwrap().is_none());
        store
            .save(StoredCredentials::new(
                "client-1".into(),
                None,
                vec!["a".into()],
                None,
            ))
            .await
            .unwrap();
        let back = store.load().await.unwrap().unwrap();
        assert_eq!(back.client_id, "client-1");
        assert!(secrets.get("mcp.x.oauth").unwrap().is_some());
        store.clear().await.unwrap();
        assert!(secrets.get("mcp.x.oauth").unwrap().is_none());
    }

    #[tokio::test]
    async fn loopback_captures_callback() {
        let lb = LoopbackRedirect::bind().await.unwrap();
        let uri = lb.redirect_uri().to_owned();
        let waiter = tokio::spawn(lb.wait());
        let addr = uri
            .trim_start_matches("http://")
            .trim_end_matches("/callback")
            .to_owned();
        let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
        s.write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
        s.write_all(b"GET /callback?code=abc&state=xyz HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let got = waiter.await.unwrap().unwrap();
        assert_eq!(got, format!("{uri}?code=abc&state=xyz"));
    }
}
