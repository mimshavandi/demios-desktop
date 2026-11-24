//! OAuth (authorization code + PKCE, discovery, dynamic registration) against an in-process
//! authorization server + protected MCP server. Tokens end up in the secret store.

mod common;

use axum::Json;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use common::fixture::Fixture;
use dn_tools::{
    ConnectionStatus, McpClient, McpServerConfig, McpTransport, MemorySecretStore, OAuthManager,
    OAuthSettings, SecretStore,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const ACCESS_TOKEN: &str = "test-access-token";

#[derive(Clone, Default)]
struct AuthState {
    base: Arc<Mutex<String>>,
    challenge: Arc<Mutex<Option<String>>>,
    token_requests: Arc<Mutex<Vec<HashMap<String, String>>>>,
}

async fn protected_resource(State(s): State<AuthState>) -> impl IntoResponse {
    let base = s.base.lock().unwrap().clone();
    Json(serde_json::json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
    }))
}

async fn as_metadata(State(s): State<AuthState>) -> impl IntoResponse {
    let base = s.base.lock().unwrap().clone();
    Json(serde_json::json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    }))
}

async fn register(Json(body): Json<serde_json::Value>) -> impl IntoResponse {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "client_id": "dyn-client-1",
            "redirect_uris": body["redirect_uris"],
            "token_endpoint_auth_method": "none",
        })),
    )
}

async fn token(
    State(s): State<AuthState>,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    s.token_requests.lock().unwrap().push(form.clone());
    let ok = form.get("grant_type").map(String::as_str) == Some("authorization_code")
        && form.get("code").map(String::as_str) == Some("the-code")
        && form.get("code_verifier").is_some_and(|v| v.len() >= 43);
    if !ok {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_grant"})),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "access_token": ACCESS_TOKEN,
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "test-refresh-token",
        })),
    )
}

async fn spawn() -> (String, AuthState) {
    let state = AuthState::default();
    let st = state.clone();
    let url = common::spawn_http_router(
        move |router| {
            let guard_state = st.clone();
            let guarded = router.layer(axum::middleware::from_fn(
                move |headers: HeaderMap,
                      req: axum::extract::Request,
                      next: axum::middleware::Next| {
                    let s = guard_state.clone();
                    async move {
                        let ok = headers
                            .get(header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            == Some(&format!("Bearer {ACCESS_TOKEN}"));
                        if ok {
                            next.run(req).await
                        } else {
                            let base = s.base.lock().unwrap().clone();
                            (
                                StatusCode::UNAUTHORIZED,
                                [(
                                    header::WWW_AUTHENTICATE,
                                    format!(
                                        "Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\""
                                    ),
                                )],
                            )
                                .into_response()
                        }
                    }
                },
            ));
            let auth_routes = axum::Router::new()
                .route("/.well-known/oauth-protected-resource/mcp", get(protected_resource))
                .route("/.well-known/oauth-protected-resource", get(protected_resource))
                .route("/.well-known/oauth-authorization-server", get(as_metadata))
                .route("/register", post(register))
                .route("/token", post(token))
                .with_state(st);
            guarded.merge(auth_routes)
        },
        Fixture::default(),
    )
    .await;
    *state.base.lock().unwrap() = url.trim_end_matches("/mcp").to_owned();
    (url, state)
}

#[tokio::test]
async fn pkce_flow_stores_tokens_and_connects() {
    let (url, state) = spawn().await;
    let secrets = Arc::new(MemorySecretStore::new());
    let oauth = Arc::new(OAuthManager::new(secrets.clone()));
    let mut cfg = McpServerConfig::http("secured", url);
    if let McpTransport::StreamableHttp { oauth, .. } = &mut cfg.transport {
        *oauth = Some(OAuthSettings {
            credential_secret_id: "mcp.secured.oauth".into(),
            scopes: vec!["tools".into()],
            client_id: None,
        });
    }

    // Before sign-in: connecting reports NeedsAuth (→ preflight NeedsConnection).
    let client = McpClient::new(cfg.clone(), secrets.clone())
        .unwrap()
        .with_auth(oauth.clone());
    let e = client.connect().await.unwrap_err();
    assert_eq!(e.kind, dn_tools::error::ToolErrorKind::AuthRequired);
    assert_eq!(client.status(), ConnectionStatus::NeedsAuth);
    assert!(!oauth.has_credentials(&cfg).unwrap());

    // Begin: discovery + dynamic registration + PKCE authorize URL.
    let redirect = "http://127.0.0.1:1/callback";
    let pending = oauth.begin(&cfg, redirect).await.unwrap();
    let auth_url = reqwest::Url::parse(pending.authorize_url()).unwrap();
    let q: HashMap<String, String> = auth_url.query_pairs().into_owned().collect();
    assert_eq!(auth_url.path(), "/authorize");
    assert_eq!(q.get("response_type").map(String::as_str), Some("code"));
    assert_eq!(q.get("client_id").map(String::as_str), Some("dyn-client-1"));
    assert_eq!(
        q.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert!(q.get("code_challenge").is_some_and(|c| c.len() >= 43));
    assert_eq!(q.get("redirect_uri").map(String::as_str), Some(redirect));
    *state.challenge.lock().unwrap() = q.get("code_challenge").cloned();
    let csrf = q.get("state").cloned().unwrap();

    // A forged state is rejected.
    let pending_bad = oauth.begin(&cfg, redirect).await.unwrap();
    assert!(
        pending_bad
            .complete(&format!("{redirect}?code=the-code&state=forged"))
            .await
            .is_err()
    );

    // Complete with the browser's redirect.
    pending
        .complete(&format!("{redirect}?code=the-code&state={csrf}"))
        .await
        .unwrap();
    let reqs = state.token_requests.lock().unwrap().clone();
    let last = reqs.last().unwrap();
    assert!(last.contains_key("code_verifier"));
    assert_eq!(last.get("redirect_uri").map(String::as_str), Some(redirect));
    let stored = secrets.get("mcp.secured.oauth").unwrap().unwrap();
    assert!(stored.contains(ACCESS_TOKEN));
    assert!(oauth.has_credentials(&cfg).unwrap());

    // Now the client connects with the stored bearer token.
    let client = McpClient::new(cfg.clone(), secrets.clone())
        .unwrap()
        .with_auth(oauth.clone());
    let tools = client.list_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "echo"));
    assert_eq!(client.status(), ConnectionStatus::Connected);

    oauth.sign_out(&cfg).unwrap();
    assert!(!oauth.has_credentials(&cfg).unwrap());
}
