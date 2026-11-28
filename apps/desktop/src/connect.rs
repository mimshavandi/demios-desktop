//! Connect (ADR-0018): a curated catalogue of tool servers, shipped with the app as signed
//! JSON. Each entry adds an ordinary MCP server ([`Entry::server_input`]); there is no special
//! runtime, and every tool keeps its risk level and approvals (ADR-0006).
//!
//! The catalogue is read only after its signature checks out ([`load`]). A catalogue that
//! doesn't check out is refused as a whole: no entries, no older copy, no unsigned fallback.

use crate::mcp::{ServerInput, TransportInput};
use dn_core::capability::RiskTier;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// The catalogue as shipped with the app.
const SHIPPED: &[u8] = include_bytes!("../connect/catalogue.json");
/// The owner's signature of it (hex Ed25519 over these exact bytes), made by hand with
/// `scripts/connect/sign-catalogue.mjs` and the key in `catalogue.pub`. Empty until then.
// Unused in `connect_test_key` builds, which trust only the test key.
#[cfg_attr(connect_test_key, allow(dead_code))]
const SHIPPED_SIG: &str = include_str!("../connect/catalogue.json.sig");
/// The owner's public key (64 hex digits), compiled into every build. Empty until the owner
/// makes one (`scripts/connect/new-key.mjs`); `build.rs` refuses anything else.
#[cfg_attr(connect_test_key, allow(dead_code))]
const RELEASE_PUBLIC_KEY: &str = include_str!("../connect/catalogue.pub");

/// The public half of the throwaway TEST key (64 hex digits) and its signature of the
/// catalogue (`catalogue.json.test-sig`). The private half was never kept:
/// `sign-catalogue.mjs --test` makes a new key pair, signs, writes both files and drops it.
/// Only debug builds with the `connect-test-key` feature (and tests) trust it.
#[cfg(any(test, connect_test_key))]
const TEST_PUBLIC_KEY: &str = include_str!("../connect/test-key.pub");
#[cfg(any(test, connect_test_key))]
const SHIPPED_TEST_SIG: &str = include_str!("../connect/catalogue.json.test-sig");

/// A public key file's contents: empty (no key yet) or 64 hex digits.
pub fn parse_public_key(s: &str) -> Result<Option<[u8; 32]>, CatalogueError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    hex(s)
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .map(Some)
        .ok_or_else(|| CatalogueError::Format("catalogue.pub isn't 64 hex digits".into()))
}

/// The test key; an empty or garbled `test-key.pub` is an error, not "no key".
#[cfg(any(test, connect_test_key))]
fn test_public_key() -> Result<[u8; 32], CatalogueError> {
    parse_public_key(TEST_PUBLIC_KEY)?
        .ok_or_else(|| CatalogueError::Format("test-key.pub is empty".into()))
}

/// The key this build trusts for the catalogue, and the signature to check with it. None
/// while the owner has no key yet: the Connect list then says it isn't available (no
/// unsigned fallback).
fn trusted() -> Result<Option<([u8; 32], &'static str)>, CatalogueError> {
    #[cfg(connect_test_key)]
    {
        Ok(Some((test_public_key()?, SHIPPED_TEST_SIG)))
    }
    #[cfg(not(connect_test_key))]
    {
        Ok(parse_public_key(RELEASE_PUBLIC_KEY)?.map(|k| (k, SHIPPED_SIG)))
    }
}

/// The shipped catalogue, if its signature checks out with the key this build trusts.
pub fn load() -> Result<Catalogue, CatalogueError> {
    let (key, sig) = trusted()?.ok_or(CatalogueError::NoKey)?;
    verify(SHIPPED, sig, &key)?;
    parse(SHIPPED)
}

/// Checks the hex Ed25519 signature `sig` of `bytes` with `key`.
pub fn verify(bytes: &[u8], sig: &str, key: &[u8; 32]) -> Result<(), CatalogueError> {
    let sig = hex(sig.trim()).ok_or(CatalogueError::Signature)?;
    if sig.len() != 64 {
        return Err(CatalogueError::Signature);
    }
    aws_lc_rs::signature::UnparsedPublicKey::new(&aws_lc_rs::signature::ED25519, key)
        .verify(bytes, &sig)
        .map_err(|_| CatalogueError::Signature)
}

fn hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The catalogue format this app reads. A newer format is refused, not half-read.
pub const FORMAT: u32 = 1;

/// The whole catalogue file.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Catalogue {
    pub format: u32,
    /// Increases with every published catalogue; a channel that delivers catalogues outside
    /// app updates must refuse one whose serial isn't higher than the one it has.
    pub serial: u64,
    /// When this catalogue was published (`YYYY-MM-DD`).
    pub published: String,
    pub entries: Vec<Entry>,
}

/// One reviewed service.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Also the tool server id it is added as (`[a-z0-9_-]`, like any server id).
    pub id: String,
    /// The service, as people know it ("Northwind Issues").
    pub service: String,
    /// Who runs the server and its sign-in.
    pub vendor: String,
    pub kind: Kind,
    /// One line on what its tools do.
    pub summary: String,
    pub server: Server,
    pub sign_in: SignIn,
    /// The OAuth scopes the app asks for; empty means the server's defaults.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Reviewed tools and their risk level. A level only ever makes a tool stricter than
    /// the app would make it on its own; it never lets a tool run without asking.
    #[serde(default)]
    pub tools: Vec<Tool>,
    /// The vendor's terms for the server, which the person agrees to with the vendor.
    pub terms_url: String,
    /// Where the person's data goes, in their words.
    pub data: String,
    pub review: Review,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The service runs the server and its own sign-in (ADR-0018 §2.1). The only kind this
    /// version adds.
    VendorRemote,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// The server's HTTPS address (Streamable HTTP).
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignIn {
    /// OAuth in the browser with the vendor; the client registers itself (dynamic client
    /// registration or a client id metadata document). Demios holds no client secret.
    Oauth,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    pub risk: RiskTier,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    /// `YYYY-MM-DD`.
    pub date: String,
    /// The vendor's own page that was checked: that it runs this server and how sign-in works.
    pub source: String,
    pub status: ReviewStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Reviewed,
    /// Kept so a server someone added can be marked and offered for removal (ADR-0018 §4);
    /// never offered for adding.
    NoLongerReviewed,
}

/// Why a catalogue was refused. The message is for logs; the UI says the list couldn't be
/// checked.
#[derive(Debug, PartialEq, Eq)]
pub enum CatalogueError {
    /// This build trusts no catalogue key yet.
    NoKey,
    Signature,
    Format(String),
    Invalid(String),
}

impl std::fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoKey => f.write_str("this build has no key to check the catalogue with"),
            Self::Signature => f.write_str("the catalogue's signature doesn't match"),
            Self::Format(m) => write!(f, "the catalogue can't be read: {m}"),
            Self::Invalid(m) => write!(f, "the catalogue is invalid: {m}"),
        }
    }
}

impl std::error::Error for CatalogueError {}

/// Parses and checks a catalogue whose signature was already verified.
pub fn parse(bytes: &[u8]) -> Result<Catalogue, CatalogueError> {
    let cat: Catalogue =
        serde_json::from_slice(bytes).map_err(|e| CatalogueError::Format(e.to_string()))?;
    if cat.format != FORMAT {
        return Err(CatalogueError::Format(format!(
            "format {} (this app reads {FORMAT})",
            cat.format
        )));
    }
    if cat.serial == 0 {
        return Err(CatalogueError::Invalid("serial must be 1 or more".into()));
    }
    date(&cat.published, "published")?;
    let mut ids = HashSet::new();
    let mut urls = HashSet::new();
    for e in &cat.entries {
        e.check()?;
        if !ids.insert(e.id.as_str()) {
            return Err(CatalogueError::Invalid(format!("entry {} twice", e.id)));
        }
        if !urls.insert(e.server.url.as_str()) {
            return Err(CatalogueError::Invalid(format!(
                "{}: another entry has the same server",
                e.id
            )));
        }
    }
    Ok(cat)
}

impl Entry {
    fn check(&self) -> Result<(), CatalogueError> {
        let bad = |m: &str| Err(CatalogueError::Invalid(format!("{}: {m}", self.id)));
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        {
            return bad("id must be 1–64 of a-z, 0-9, - and _");
        }
        for (field, v) in [
            ("service", &self.service),
            ("vendor", &self.vendor),
            ("summary", &self.summary),
            ("data", &self.data),
        ] {
            if v.trim().is_empty() || v.chars().any(char::is_control) {
                return bad(&format!("{field} must be one line of text"));
            }
        }
        if !https(&self.server.url) {
            return bad("the server must be an https address");
        }
        if !https(&self.terms_url) || !https(&self.review.source) {
            return bad("terms_url and review.source must be https addresses");
        }
        date(&self.review.date, &format!("{}: review.date", self.id))?;
        if self
            .scopes
            .iter()
            .any(|s| s.is_empty() || s.chars().any(char::is_whitespace))
        {
            return bad("a scope is empty or has spaces");
        }
        let mut names = HashSet::new();
        for t in &self.tools {
            if t.name.is_empty() || !names.insert(t.name.as_str()) {
                return bad("tool names must be unique and not empty");
            }
            // A remote server's tools never run without asking: a level below "Changes local
            // data" would read as if they could.
            if t.risk == RiskTier::Read {
                return bad(&format!("tool {} can't be below low_write", t.name));
            }
        }
        Ok(())
    }

    /// The ordinary tool server this entry adds: the server's address with sign-in in the
    /// browser, no headers, no client id, no secrets.
    pub fn server_input(&self) -> ServerInput {
        ServerInput {
            id: self.id.clone(),
            name: Some(self.service.clone()),
            transport: TransportInput::StreamableHttp {
                url: self.server.url.clone(),
                headers: Vec::new(),
                oauth: matches!(self.sign_in, SignIn::Oauth),
                scopes: self.scopes.clone(),
                client_id: None,
            },
        }
    }

    pub fn offered(&self) -> bool {
        self.review.status == ReviewStatus::Reviewed
    }
}

fn https(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str().is_some_and(|h| !h.is_empty())
            && u.username().is_empty()
            && u.password().is_none()
    })
}

fn date(s: &str, what: &str) -> Result<(), CatalogueError> {
    let b = s.as_bytes();
    let ok = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(CatalogueError::Invalid(format!(
            "{what} must be YYYY-MM-DD"
        )))
    }
}

/// What Settings › Tools › Connect shows: each entry that is offered, or that the person
/// already added (an entry no longer reviewed is shown only then, marked).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ConnectView {
    pub entries: Vec<EntryView>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct EntryView {
    pub id: String,
    pub service: String,
    pub vendor: String,
    pub summary: String,
    pub url: String,
    pub scopes: Vec<String>,
    pub terms_url: String,
    pub data: String,
    pub review_date: String,
    pub review_source: String,
    pub reviewed: bool,
    /// The tool server it is added as, if it is: same id and address, or any server with this
    /// address (added by hand).
    pub server_id: Option<String>,
}

fn server_url(cfg: &dn_tools::McpServerConfig) -> Option<&str> {
    match &cfg.transport {
        dn_tools::McpTransport::StreamableHttp { url, .. } => Some(url.as_str()),
        _ => None,
    }
}

pub fn view(cat: &Catalogue, servers: &[dn_tools::McpServerConfig]) -> ConnectView {
    let entries = cat
        .entries
        .iter()
        .filter_map(|e| {
            let server_id = servers
                .iter()
                .find(|s| server_url(s) == Some(e.server.url.as_str()))
                .map(|s| s.id.clone());
            (e.offered() || server_id.is_some()).then(|| EntryView {
                id: e.id.clone(),
                service: e.service.clone(),
                vendor: e.vendor.clone(),
                summary: e.summary.clone(),
                url: e.server.url.clone(),
                scopes: e.scopes.clone(),
                terms_url: e.terms_url.clone(),
                data: e.data.clone(),
                review_date: e.review.date.clone(),
                review_source: e.review.source.clone(),
                reviewed: e.offered(),
                server_id,
            })
        })
        .collect();
    ConnectView { entries }
}

/// The user-facing error for a catalogue that can't be used. No list, no older copy.
pub fn unavailable(e: &CatalogueError) -> crate::error::IpcError {
    tracing::warn!("connect catalogue: {e}");
    match e {
        CatalogueError::NoKey => crate::error::IpcError::new(
            "catalogue_unavailable",
            "Connect isn't available in this version of Demios yet. Add tool servers below.",
        ),
        _ => crate::error::IpcError::new(
            "catalogue_unchecked",
            "The list of services couldn't be checked, so it isn't shown. Reinstall Demios to get a good copy.",
        ),
    }
}

impl crate::state::AppState {
    pub fn connect_view(&self) -> crate::error::IpcResult<ConnectView> {
        let cat = load().map_err(|e| unavailable(&e))?;
        Ok(view(&cat, &self.settings()?.mcp_servers))
    }

    /// Adds an entry as an ordinary tool server, through the same path as the Add form
    /// ([`Self::save_mcp_server`]): same checks, same risk levels and approvals. Blocking.
    pub fn connect_entry(
        self: &std::sync::Arc<Self>,
        cat: &Catalogue,
        entry_id: &str,
    ) -> crate::error::IpcResult<dn_tools::McpServerConfig> {
        let entry = cat
            .entries
            .iter()
            .find(|e| e.id == entry_id && e.offered())
            .ok_or_else(|| {
                crate::error::IpcError::new("not_found", "That service isn't in the list any more.")
            })?;
        let servers = self.settings()?.mcp_servers;
        if servers
            .iter()
            .any(|s| server_url(s) == Some(entry.server.url.as_str()))
        {
            return Err(crate::error::IpcError::invalid(format!(
                "{} is already one of your tool servers.",
                entry.service
            )));
        }
        if servers.iter().any(|s| s.id == entry.id) {
            return Err(crate::error::IpcError::invalid(format!(
                "You already have a tool server with the id {}. Remove it, or add {} by hand under Tool servers.",
                entry.id, entry.service
            )));
        }
        self.save_mcp_server(&entry.server_input(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> serde_json::Value {
        serde_json::json!({
            "format": 1,
            "serial": 3,
            "published": "2026-11-26",
            "entries": [{
                "id": "northwind",
                "service": "Northwind Issues",
                "vendor": "Northwind",
                "kind": "vendor_remote",
                "summary": "Read and create issues and comments",
                "server": { "url": "https://mcp.northwind.example/mcp" },
                "sign_in": { "type": "oauth" },
                "scopes": ["read", "write"],
                "tools": [{ "name": "create_issue", "risk": "external_write" }],
                "terms_url": "https://northwind.example/terms",
                "data": "Goes between Northwind and this computer.",
                "review": {
                    "date": "2026-11-26",
                    "source": "https://northwind.example/docs/mcp",
                    "status": "reviewed"
                }
            }]
        })
    }

    type Change = Box<dyn Fn(&mut serde_json::Value)>;

    fn parse_json(v: &serde_json::Value) -> Result<Catalogue, CatalogueError> {
        parse(&serde_json::to_vec(v).unwrap())
    }

    #[test]
    fn a_good_catalogue_parses_and_adds_an_ordinary_oauth_server() {
        let cat = parse_json(&sample()).unwrap();
        let e = &cat.entries[0];
        assert!(e.offered());
        let input = e.server_input();
        assert_eq!(input.id, "northwind");
        assert_eq!(input.name.as_deref(), Some("Northwind Issues"));
        match input.transport {
            TransportInput::StreamableHttp {
                url,
                headers,
                oauth,
                scopes,
                client_id,
            } => {
                assert_eq!(url, "https://mcp.northwind.example/mcp");
                assert!(headers.is_empty());
                assert!(oauth);
                assert_eq!(scopes, ["read", "write"]);
                assert_eq!(client_id, None);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn bad_catalogues_are_refused_whole() {
        let cases: Vec<(&str, Change)> = vec![
            ("newer format", Box::new(|v| v["format"] = 2.into())),
            ("serial 0", Box::new(|v| v["serial"] = 0.into())),
            (
                "unknown field",
                Box::new(|v| v["entries"][0]["extra"] = 1.into()),
            ),
            (
                "unknown kind",
                Box::new(|v| v["entries"][0]["kind"] = "pinned".into()),
            ),
            (
                "http server",
                Box::new(|v| {
                    v["entries"][0]["server"]["url"] = "http://mcp.northwind.example/mcp".into()
                }),
            ),
            (
                "userinfo",
                Box::new(|v| {
                    v["entries"][0]["server"]["url"] =
                        "https://u:p@mcp.northwind.example/mcp".into()
                }),
            ),
            (
                "bad id",
                Box::new(|v| v["entries"][0]["id"] = "North Wind".into()),
            ),
            (
                "read tool",
                Box::new(|v| v["entries"][0]["tools"][0]["risk"] = "read".into()),
            ),
            (
                "bad date",
                Box::new(|v| v["entries"][0]["review"]["date"] = "26.11.2026".into()),
            ),
            (
                "newline",
                Box::new(|v| v["entries"][0]["summary"] = "a\nb".into()),
            ),
            (
                "terms not https",
                Box::new(|v| v["entries"][0]["terms_url"] = "ftp://x".into()),
            ),
            (
                "duplicate id",
                Box::new(|v| {
                    let e = v["entries"][0].clone();
                    v["entries"].as_array_mut().unwrap().push(e);
                }),
            ),
        ];
        for (what, change) in cases {
            let mut v = sample();
            change(&mut v);
            assert!(parse_json(&v).is_err(), "{what} was accepted");
        }
    }

    /// A key pair made for this test run only; no private key is stored anywhere.
    fn fresh_pair() -> aws_lc_rs::signature::Ed25519KeyPair {
        aws_lc_rs::signature::Ed25519KeyPair::generate().unwrap()
    }

    fn sign_hex(pair: &aws_lc_rs::signature::Ed25519KeyPair, bytes: &[u8]) -> String {
        pair.sign(bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn public(pair: &aws_lc_rs::signature::Ed25519KeyPair) -> [u8; 32] {
        use aws_lc_rs::signature::KeyPair;
        pair.public_key().as_ref().try_into().unwrap()
    }

    #[test]
    fn the_shipped_catalogue_is_signed_with_the_test_key() {
        verify(SHIPPED, SHIPPED_TEST_SIG, &test_public_key().unwrap()).unwrap();
    }

    #[test]
    fn a_tampered_catalogue_is_refused() {
        let key = test_public_key().unwrap();
        // One changed byte: another server address.
        let tampered = String::from_utf8(SHIPPED.to_vec()).unwrap().replacen(
            "https://mcp.linear.app/mcp",
            "https://mcp.linear.app/mcq",
            1,
        );
        assert_ne!(tampered.as_bytes(), SHIPPED);
        assert_eq!(
            verify(tampered.as_bytes(), SHIPPED_TEST_SIG, &key),
            Err(CatalogueError::Signature)
        );
        // Signed by another key, it checks out with that key only: the check is about the
        // bytes and the key.
        let other = fresh_pair();
        let sig = sign_hex(&other, tampered.as_bytes());
        verify(tampered.as_bytes(), &sig, &public(&other)).unwrap();
        assert_eq!(
            verify(tampered.as_bytes(), &sig, &key),
            Err(CatalogueError::Signature)
        );
        // Another key, a short or garbled signature: refused.
        assert!(verify(SHIPPED, SHIPPED_TEST_SIG, &public(&other)).is_err());
        assert!(verify(SHIPPED, &SHIPPED_TEST_SIG.trim()[..126], &key).is_err());
        assert!(verify(SHIPPED, "zz", &key).is_err());
        assert!(verify(SHIPPED, "", &key).is_err());
    }

    #[test]
    fn the_owner_public_key_file_is_empty_or_64_hex_digits() {
        assert_eq!(parse_public_key(""), Ok(None));
        assert_eq!(parse_public_key("  \n"), Ok(None));
        assert_eq!(parse_public_key(&"ab".repeat(32)), Ok(Some([0xab; 32])));
        let hex = "ab".repeat(32);
        assert!(parse_public_key(&"ab".repeat(33)).is_err());
        assert!(parse_public_key(&"zz".repeat(32)).is_err());
        assert!(parse_public_key(&hex[..62]).is_err());
        assert!(parse_public_key("not a key").is_err());
        // The committed file is one of those (build.rs refuses anything else).
        assert!(parse_public_key(RELEASE_PUBLIC_KEY).is_ok());
    }

    #[test]
    fn the_test_key_never_signs_the_release_signature() {
        // A release signature made with the test key would be trusted by no release build, but
        // it also mustn't pass the release check by mistake.
        if !SHIPPED_SIG.trim().is_empty() {
            assert!(verify(SHIPPED, SHIPPED_SIG, &test_public_key().unwrap()).is_err());
        }
    }

    #[test]
    fn without_a_trusted_key_nothing_is_offered() {
        #[cfg(not(connect_test_key))]
        if RELEASE_PUBLIC_KEY.trim().is_empty() {
            assert_eq!(load(), Err(CatalogueError::NoKey));
        }
        #[cfg(connect_test_key)]
        assert!(load().is_ok());
    }

    #[test]
    fn the_shipped_catalogue_is_valid_and_lists_vendor_servers_only() {
        let cat = parse(SHIPPED).unwrap();
        assert!(!cat.entries.is_empty());
        for e in &cat.entries {
            assert_eq!(e.kind, Kind::VendorRemote, "{}", e.id);
            assert!(e.offered(), "{}", e.id);
        }
    }

    fn app() -> std::sync::Arc<crate::state::AppState> {
        let conn = dn_store::open_in_memory(&crate::state::migration_sets()).unwrap();
        std::sync::Arc::new(crate::state::AppState::new(
            conn,
            crate::models::ModelRegistry::default(),
            crate::secrets::Keys::memory(),
        ))
    }

    /// A server added through Connect is the same ordinary server the Add form makes, and
    /// its tools ask as any remote server's do: nothing runs without asking.
    #[test]
    fn a_connected_service_is_an_ordinary_server_that_keeps_its_approvals() {
        use dn_core::capability::ApprovalPolicy;
        let cat = parse_json(&sample()).unwrap();
        let st = app();
        let cfg = st.connect_entry(&cat, "northwind").unwrap();

        // The same config as adding the same address by hand.
        let by_hand = app()
            .save_mcp_server(&cat.entries[0].server_input(), None)
            .unwrap();
        assert_eq!(cfg, by_hand);
        assert!(
            !cfg.trusts_annotations(),
            "a remote server's hints aren't trusted"
        );
        let settings = st.settings().unwrap();
        assert!(
            settings.tool_overrides.is_empty(),
            "Connect sets no tool choices"
        );
        assert!(
            !settings.reviewed_tools.contains_key("northwind"),
            "nothing is pre-reviewed"
        );

        let tool = |name: &str, read_only: bool| dn_tools::McpTool {
            name: name.into(),
            title: None,
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            hints: dn_tools::client::ToolHints {
                read_only: Some(read_only),
                ..Default::default()
            },
        };
        let cap = |t: &dn_tools::McpTool| {
            dn_tools::McpCapability::from_tool(&cfg.id, t, None, cfg.trusts_annotations())
        };
        // Even a tool that says it only reads asks once per chat; others ask every time.
        assert_eq!(
            cap(&tool("search", true)).approval,
            ApprovalPolicy::AskOncePerConversation
        );
        assert_eq!(
            cap(&tool("create_issue", false)).approval,
            ApprovalPolicy::AlwaysAsk
        );

        // Shown as added; adding again is refused.
        let v = view(&cat, &settings.mcp_servers);
        assert_eq!(v.entries[0].server_id.as_deref(), Some("northwind"));
        let e = st.connect_entry(&cat, "northwind").unwrap_err();
        assert!(
            e.message.contains("already one of your tool servers"),
            "{}",
            e.message
        );
    }

    #[test]
    fn connect_finds_a_server_added_by_hand_and_refuses_a_taken_id() {
        let cat = parse_json(&sample()).unwrap();
        let st = app();
        // The same address under another id: shown as added, not added twice.
        let mut by_hand = cat.entries[0].server_input();
        by_hand.id = "my-tracker".into();
        st.save_mcp_server(&by_hand, None).unwrap();
        let v = view(&cat, &st.settings().unwrap().mcp_servers);
        assert_eq!(v.entries[0].server_id.as_deref(), Some("my-tracker"));
        assert!(st.connect_entry(&cat, "northwind").is_err());

        // The id taken by another server: refused, with a way out.
        let st = app();
        let mut other = cat.entries[0].server_input();
        if let TransportInput::StreamableHttp { url, .. } = &mut other.transport {
            *url = "https://elsewhere.example/mcp".into();
        }
        st.save_mcp_server(&other, None).unwrap();
        let e = st.connect_entry(&cat, "northwind").unwrap_err();
        assert!(
            e.message.contains("add Northwind Issues by hand"),
            "{}",
            e.message
        );
        assert_eq!(st.settings().unwrap().mcp_servers.len(), 1);
    }

    #[test]
    fn an_entry_no_longer_reviewed_shows_only_when_added_and_cant_be_connected() {
        let mut v = sample();
        v["entries"][0]["review"]["status"] = "no_longer_reviewed".into();
        let cat = parse_json(&v).unwrap();
        assert!(view(&cat, &[]).entries.is_empty());
        let st = app();
        assert_eq!(
            st.connect_entry(&cat, "northwind").unwrap_err().code,
            "not_found"
        );
        st.save_mcp_server(&cat.entries[0].server_input(), None)
            .unwrap();
        let shown = view(&cat, &st.settings().unwrap().mcp_servers);
        assert_eq!(shown.entries.len(), 1);
        assert!(!shown.entries[0].reviewed);
    }

    #[test]
    fn an_entry_no_longer_reviewed_is_not_offered() {
        let mut v = sample();
        v["entries"][0]["review"]["status"] = "no_longer_reviewed".into();
        assert!(!parse_json(&v).unwrap().entries[0].offered());
    }
}
