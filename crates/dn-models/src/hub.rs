//! Hugging Face Hub client (ADR-0011): find GGUF repos, list a repo's quantisations, pin a
//! commit, and download with the SHA-256 from the Hub's LFS metadata.
//!
//! Flow for a UI or `dn`:
//! 1. [`HubClient::search`] → pages of [`RepoSummary`] (follow [`SearchPage::next`]).
//! 2. [`HubClient::repo`] → [`RepoInfo`]: the commit SHA a revision points to (every later
//!    call uses that SHA, so a download is reproducible), gating, licence + card link.
//! 3. [`HubClient::files`] → [`RepoFile`]s at that commit; [`gguf_variants`] groups them into
//!    one [`GgufVariant`] per quantisation (split GGUFs `-00001-of-0000N` become one variant).
//! 4. Before download, [`HubClient::footprint`] reads only the GGUF header(s) of a variant
//!    with HTTP range requests (a few MB, growing until the header parses) and returns the
//!    [`ModelFootprint`] for [`crate::fit`]. To rate every quant of a repo, read one header and
//!    scale it to the other variants' file sizes with [`estimate_variant`] (an
//!    [`Estimated`] value, so the "estimated" label can't get lost).
//! 5. [`HubClient::pull`] downloads a variant (resumable, every part checked against its LFS
//!    sha256; a file without one is refused) and writes a [`SourceManifest`] next to it.
//!
//! Configuration: `HF_ENDPOINT` selects a mirror ([`HubConfig::from_env`]). Gated repos use
//! the user's own token, stored in the OS keychain as `provider/huggingface`
//! ([`HF_TOKEN_SECRET`]). It is sent only to the official Hub (`https://huggingface.co`):
//! a mirror never sees it unless the user opts in ([`HubConfig::send_token_to_endpoint`],
//! env `DN_HF_SEND_TOKEN_TO_ENDPOINT=1`). It is dropped on redirects to other hosts and
//! never logged.
//! HTTPS only, except a loopback endpoint (tests, local mirrors).
//!
//! No Demios server and no telemetry: every request goes straight to the Hub.
//!
//! Every call is async and cancels when its future is dropped.

use crate::download::{self, DownloadError, Expected};
use crate::fit::ModelFootprint;
use crate::gguf::Gguf;
use dn_core::{CoreError, RecoveryAction};
use dn_secrets::{AsyncSecrets, Secret, SecretId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_ENDPOINT: &str = "https://huggingface.co";

/// Keychain id of the user's Hugging Face token.
pub fn hf_token_secret() -> SecretId {
    SecretId::provider(HF_TOKEN_SECRET)
}

/// Id passed to [`SecretId::provider`] for the Hugging Face token.
pub const HF_TOKEN_SECRET: &str = "huggingface";

#[derive(Debug, thiserror::Error)]
pub enum HubError {
    #[error("invalid repo id `{0}` (expected owner/name)")]
    InvalidRepo(String),
    #[error("invalid file path `{0}`")]
    InvalidPath(String),
    #[error("repo or revision not found: {0}")]
    NotFound(String),
    /// 403, or 401 without a token: gated or private. `has_token` says whether a token was
    /// sent (with one, the account hasn't accepted the model's terms).
    #[error("{repo} needs access (gated or private); token sent: {has_token}")]
    Gated { repo: String, has_token: bool },
    /// 401 although a token was sent: the token is invalid or expired.
    #[error("the Hugging Face token was rejected")]
    Unauthorized,
    /// The repo has no complete GGUF model file at this revision.
    #[error("{0} has no GGUF files")]
    NoGguf(String),
    #[error("rate limited by the Hub")]
    RateLimited { retry_after: Option<u32> },
    #[error("hub returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("network: {0}")]
    Http(String),
    #[error("unexpected hub response: {0}")]
    Parse(String),
    /// The Hub has no LFS sha256 for this file, so it can't be verified: refused.
    #[error("{0} has no LFS sha256 on the Hub; refusing to download an unverifiable file")]
    NoChecksum(String),
    /// The file isn't a GGUF the reader understands (or its header is absurdly large).
    #[error("GGUF header: {0}")]
    Gguf(String),
    /// The server ignored a `Range` request for a non-zero offset.
    #[error("server does not support range requests")]
    RangeUnsupported,
    #[error("endpoint `{0}` must be https (or loopback)")]
    InsecureEndpoint(String),
    #[error(transparent)]
    Download(#[from] DownloadError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<HubError> for CoreError {
    fn from(e: HubError) -> Self {
        let developer_message = e.to_string();
        let (user_message, recovery) = match &e {
            HubError::Gated { has_token, .. } => (
                if *has_token {
                    "Your Hugging Face account doesn't have access to this model yet. Accept its terms on the model page, then try again."
                } else {
                    "This model needs a Hugging Face token. Add yours in settings, accept the model's terms on its page, then try again."
                }
                .to_string(),
                vec![RecoveryAction::AddProviderKey],
            ),
            HubError::Unauthorized => (
                "Your Hugging Face token was rejected. Replace it in settings.".to_string(),
                vec![RecoveryAction::AddProviderKey],
            ),
            HubError::NoGguf(_) => (
                "This repo has no GGUF files Demios can run.".to_string(),
                vec![],
            ),
            HubError::RateLimited { retry_after } => (
                "Hugging Face is limiting requests. Try again in a moment.".to_string(),
                vec![RecoveryAction::Retry {
                    after_seconds: retry_after.or(Some(30)),
                }],
            ),
            HubError::Http(_) | HubError::Status { .. } => (
                "Couldn't reach Hugging Face. Check your connection.".to_string(),
                vec![RecoveryAction::Retry {
                    after_seconds: Some(5),
                }],
            ),
            HubError::NotFound(_) => (
                "That model or version doesn't exist on Hugging Face.".to_string(),
                vec![],
            ),
            HubError::NoChecksum(_) => (
                "This file can't be verified, so it won't be downloaded.".to_string(),
                vec![],
            ),
            HubError::Download(DownloadError::DiskFull { .. }) => (
                "Not enough disk space for this model.".to_string(),
                vec![],
            ),
            _ => ("Couldn't get the model from Hugging Face.".to_string(), vec![]),
        };
        CoreError::ModelUnavailable {
            user_message,
            developer_message,
            recovery,
        }
    }
}

pub type Result<T, E = HubError> = std::result::Result<T, E>;

#[derive(Clone, Default)]
pub struct HubConfig {
    /// Hub base URL; `None` = [`DEFAULT_ENDPOINT`].
    pub endpoint: Option<String>,
    pub token: Option<Secret>,
    /// Also send `token` to a non-default `endpoint` (a mirror the user trusts with it).
    /// Off by default: the token is for huggingface.co.
    pub send_token_to_endpoint: bool,
}

impl std::fmt::Debug for HubConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubConfig")
            .field("endpoint", &self.endpoint)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("send_token_to_endpoint", &self.send_token_to_endpoint)
            .finish()
    }
}

impl HubConfig {
    /// Endpoint from `HF_ENDPOINT`, no token; `DN_HF_SEND_TOKEN_TO_ENDPOINT=1` opts in to
    /// sending the token to that endpoint.
    pub fn from_env() -> Self {
        Self {
            endpoint: std::env::var("HF_ENDPOINT").ok().filter(|s| !s.is_empty()),
            token: None,
            send_token_to_endpoint: std::env::var("DN_HF_SEND_TOKEN_TO_ENDPOINT")
                .is_ok_and(|v| v == "1"),
        }
    }

    pub fn with_token(mut self, token: Secret) -> Self {
        self.token = Some(token);
        self
    }

    /// Adds the token stored in the keychain, if there is one.
    pub async fn with_token_from(mut self, store: &AsyncSecrets) -> Self {
        if let Ok(Some(t)) = store.get(hf_token_secret()).await {
            self.token = Some(t);
        }
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSort {
    #[default]
    Downloads,
    Likes,
    Trending,
    Updated,
}

impl SearchSort {
    fn param(self) -> &'static str {
        match self {
            Self::Downloads => "downloads",
            Self::Likes => "likes",
            Self::Trending => "trendingScore",
            Self::Updated => "lastModified",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    pub sort: SearchSort,
    /// Page size (the Hub caps it; default 20).
    pub limit: Option<u32>,
}

/// One search hit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepoSummary {
    pub id: String,
    /// Owner part of `id`.
    pub author: String,
    pub downloads: u64,
    pub likes: u64,
    pub last_modified: Option<String>,
    pub gated: bool,
    pub pipeline_tag: Option<String>,
    pub license: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchPage {
    pub items: Vec<RepoSummary>,
    /// Opaque cursor for [`HubClient::search_next`]; `None` on the last page.
    pub next: Option<String>,
}

/// Why a licence deserves a second look before download (any licence is allowed; the
/// user sees the licence and the model card first).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseWarning {
    /// No licence in the model card.
    Missing,
    /// Research / non-commercial terms.
    NonCommercial,
    /// `other` or a model-specific licence: read the card.
    Custom,
    /// Commercial use allowed under an acceptable-use policy (Llama, Gemma): allowed like
    /// any licence, but worth a note next to it.
    UsePolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseInfo {
    /// SPDX-like id from the card (`apache-2.0`, `llama3.1`, `cc-by-nc-4.0`, `other`, ...).
    pub id: Option<String>,
    /// `license_name` from the card when `id` is `other`.
    pub name: Option<String>,
    /// `license_link` from the card, if any.
    pub link: Option<String>,
    pub warning: Option<LicenseWarning>,
}

impl LicenseInfo {
    pub fn classify(id: Option<&str>, name: Option<&str>, link: Option<&str>) -> Self {
        let (id, name) = (id.map(sanitize_display), name.map(sanitize_display));
        let (id, name) = (id.as_deref(), name.as_deref());
        // Only an http(s) URL is kept as the licence link.
        let link = link
            .map(sanitize_display)
            .filter(|l| l.starts_with("https://") || l.starts_with("http://"));
        let link = link.as_deref();
        let id = id
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty());
        let label = name.map(|n| n.to_ascii_lowercase()).or_else(|| id.clone());
        let warning = match (&id, &label) {
            (None, None) => Some(LicenseWarning::Missing),
            (_, Some(l)) if is_non_commercial(l) => Some(LicenseWarning::NonCommercial),
            (Some(i), _) if is_non_commercial(i) => Some(LicenseWarning::NonCommercial),
            (Some(i), _) if i == "other" || i == "unknown" => Some(LicenseWarning::Custom),
            (Some(i), _) if has_use_policy(i) => Some(LicenseWarning::UsePolicy),
            (Some(i), _) if is_open(i) => None,
            _ => Some(LicenseWarning::Custom),
        };
        Self {
            id,
            name: name.map(str::to_string),
            link: link.map(str::to_string),
            warning,
        }
    }
}

fn has_use_policy(l: &str) -> bool {
    l.starts_with("llama") || l == "gemma"
}

fn is_non_commercial(l: &str) -> bool {
    l.contains("-nc")
        || l.contains("noncommercial")
        || l.contains("non-commercial")
        || l.contains("research")
        || l == "mnpl"
        || l.starts_with("mnpl-")
}

/// Licences that need no warning: OSI-style open licences and the common open-weight model
/// licences with commercial use.
fn is_open(l: &str) -> bool {
    const OPEN: &[&str] = &[
        "apache-2.0",
        "mit",
        "bsd-2-clause",
        "bsd-3-clause",
        "cc0-1.0",
        "cc-by-4.0",
        "cc-by-sa-4.0",
        "openrail",
        "openrail++",
        "bigscience-openrail-m",
        "creativeml-openrail-m",
        "llama2",
        "llama3",
        "llama3.1",
        "llama3.2",
        "llama3.3",
        "llama4",
        "gemma",
        "deepseek",
        "qwen",
        "mpl-2.0",
        "gpl-3.0",
        "lgpl-3.0",
        "agpl-3.0",
        "artistic-2.0",
        "unlicense",
        "wtfpl",
    ];
    OPEN.contains(&l)
}

/// A repo pinned to a commit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub id: String,
    /// Full commit SHA the requested revision resolved to.
    pub sha: String,
    pub gated: bool,
    pub private: bool,
    pub license: LicenseInfo,
    /// Model card page (`{endpoint}/{repo}`), to show before download.
    pub card_url: String,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoFile {
    pub path: String,
    pub size: u64,
    /// LFS sha256 (hex). `None` for files stored in git (never model weights).
    pub sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantKind {
    /// Language model weights.
    Model,
    /// Vision projector (`mmproj`), loaded next to a model.
    Projector,
}

/// One downloadable quantisation: a single GGUF or all parts of a split GGUF.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GgufVariant {
    /// Display name: file name without `.gguf` and without the split suffix.
    pub name: String,
    /// Quantisation label parsed from the name (`Q4_K_M`, `IQ3_XXS`, `BF16`, ...).
    pub quant: Option<String>,
    pub kind: VariantKind,
    /// Parts in load order (one entry unless split). Load the first.
    pub files: Vec<RepoFile>,
    pub total_size: u64,
}

/// Progress of a [`HubClient::pull`], summed over all parts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullProgress {
    /// 0-based index of the part being downloaded.
    pub part: usize,
    pub parts: usize,
    pub done_bytes: u64,
    pub total_bytes: u64,
}

/// Written next to downloaded weights as `<first part>.source.json`: where they came from.
/// It also marks files Demios downloaded (cache discovery never touches other files).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceManifest {
    pub source: String,
    pub endpoint: String,
    pub repo: String,
    pub revision: String,
    pub files: Vec<RepoFile>,
    pub license: LicenseInfo,
    pub card_url: String,
    /// Seconds since the Unix epoch.
    pub downloaded_at: u64,
}

impl SourceManifest {
    pub fn path_for(first_part: &Path) -> PathBuf {
        let mut p = first_part.as_os_str().to_owned();
        p.push(".source.json");
        PathBuf::from(p)
    }

    pub fn read(first_part: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(Self::path_for(first_part)).ok()?).ok()
    }
}

/// `owner/name`, each part `[A-Za-z0-9._-]`, not `.`/`..`.
pub fn validate_repo_id(id: &str) -> Result<()> {
    let ok = id.split('/').count() == 2
        && id.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.len() <= 96
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        });
    if ok {
        Ok(())
    } else {
        Err(HubError::InvalidRepo(id.into()))
    }
}

/// Relative path inside a repo, safe as a local path on every OS: `/`-separated segments,
/// none empty, `.`, `..`, ending in `.` or space, a Windows device name (`CON`, `NUL`,
/// `COM1`, `LPT1.txt`, ...), or containing `\`, `:`, `*?"<>|` or control characters.
pub fn validate_repo_path(path: &str) -> Result<()> {
    let ok = !path.is_empty()
        && path.len() <= 1024
        && !path
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        && path.split('/').all(segment_ok);
    if ok {
        Ok(())
    } else {
        Err(HubError::InvalidPath(path.into()))
    }
}

fn segment_ok(s: &str) -> bool {
    if s.is_empty() || s == "." || s == ".." || s.ends_with(['.', ' ']) || s.len() > 255 {
        return false;
    }
    let stem = s
        .split('.')
        .next()
        .unwrap_or(s)
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    !reserved
}

/// A full 40-hex commit SHA (what `repo()` resolves revisions to).
fn validate_commit(sha: &str) -> Result<()> {
    if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(HubError::NotFound(format!("`{sha}` is not a commit SHA")))
    }
}

/// Text from a model card or search result, safe to print: control characters removed,
/// length capped.
pub fn sanitize_display(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}

fn validate_revision(rev: &str) -> Result<()> {
    let ok = !rev.is_empty()
        && rev.len() <= 255
        && !rev.contains("..")
        && rev
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    if ok {
        Ok(())
    } else {
        Err(HubError::NotFound(format!("invalid revision `{rev}`")))
    }
}

pub struct HubClient {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    token: Option<Secret>,
}

impl HubClient {
    pub fn new(cfg: HubConfig) -> Result<Self> {
        let raw = cfg.endpoint.as_deref().unwrap_or(DEFAULT_ENDPOINT);
        let mut endpoint = reqwest::Url::parse(raw.trim_end_matches('/'))
            .map_err(|_| HubError::InsecureEndpoint(raw.into()))?;
        let host = endpoint.host_str().unwrap_or("");
        let loopback = host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && loopback) {
            return Err(HubError::InsecureEndpoint(raw.into()));
        }
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let official = endpoint.scheme() == "https"
            && endpoint.host_str() == Some("huggingface.co")
            && endpoint.port().is_none();
        let token = cfg.token.filter(|_| official || cfg.send_token_to_endpoint);
        let http = reqwest::Client::builder()
            .https_only(!loopback)
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .user_agent(concat!("demios/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| HubError::Http(e.to_string()))?;
        Ok(Self {
            http,
            endpoint,
            token,
        })
    }

    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str().trim_end_matches('/')
    }

    fn url(&self, path: &str) -> reqwest::Url {
        let mut u = self.endpoint.clone();
        let base = u.path().trim_end_matches('/').to_string();
        u.set_path(&format!("{base}{path}"));
        u
    }

    /// Only our own endpoint ever gets the token or is followed as a cursor.
    fn same_origin(&self, u: &reqwest::Url) -> bool {
        u.scheme() == self.endpoint.scheme()
            && u.host_str() == self.endpoint.host_str()
            && u.port_or_known_default() == self.endpoint.port_or_known_default()
    }

    async fn get(&self, url: reqwest::Url, repo: Option<&str>) -> Result<reqwest::Response> {
        let mut rb = self.http.get(url.clone());
        if let Some(t) = self.token.as_ref().filter(|_| self.same_origin(&url)) {
            rb = rb.bearer_auth(t.expose());
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| HubError::Http(e.without_url().to_string()))?;
        let status = resp.status().as_u16();
        match status {
            200..=299 => Ok(resp),
            401 if self.token.is_some() => Err(HubError::Unauthorized),
            401 | 403 => Err(HubError::Gated {
                repo: repo.unwrap_or("").into(),
                has_token: self.token.is_some(),
            }),
            404 => Err(HubError::NotFound(
                repo.map(str::to_string)
                    .unwrap_or_else(|| url.path().into()),
            )),
            429 => Err(HubError::RateLimited {
                retry_after: resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse().ok()),
            }),
            _ => {
                let mut body = resp.text().await.unwrap_or_default();
                body.truncate(300);
                Err(HubError::Status { status, body })
            }
        }
    }

    async fn json(&self, url: reqwest::Url, repo: Option<&str>) -> Result<(Value, Option<String>)> {
        /// API responses are small; a bigger body is refused, not buffered.
        const MAX_JSON: usize = 16 << 20;
        let mut resp = self.get(url, repo).await?;
        let next = next_link(&resp);
        if resp.content_length().is_some_and(|n| n > MAX_JSON as u64) {
            return Err(HubError::Parse("response too large".into()));
        }
        let mut body = Vec::new();
        while let Some(c) = resp
            .chunk()
            .await
            .map_err(|e| HubError::Http(e.without_url().to_string()))?
        {
            if body.len() + c.len() > MAX_JSON {
                return Err(HubError::Parse("response too large".into()));
            }
            body.extend_from_slice(&c);
        }
        let v = serde_json::from_slice(&body).map_err(|e| HubError::Parse(e.to_string()))?;
        Ok((v, next))
    }

    /// First page of GGUF repos matching `q`.
    pub async fn search(&self, q: &SearchQuery) -> Result<SearchPage> {
        let mut u = self.url("/api/models");
        u.query_pairs_mut()
            .append_pair("filter", "gguf")
            .append_pair("search", q.text.trim())
            .append_pair("sort", q.sort.param())
            .append_pair("direction", "-1")
            .append_pair("limit", &q.limit.unwrap_or(20).clamp(1, 100).to_string())
            .append_pair("full", "false")
            .append_pair("cardData", "true");
        self.search_url(u).await
    }

    /// The page after one returned by [`search`](Self::search).
    pub async fn search_next(&self, cursor: &str) -> Result<SearchPage> {
        let u = reqwest::Url::parse(cursor).map_err(|e| HubError::Parse(e.to_string()))?;
        if !self.same_origin(&u) || !u.path().ends_with("/api/models") {
            return Err(HubError::Parse("cursor is not a Hub search URL".into()));
        }
        self.search_url(u).await
    }

    async fn search_url(&self, u: reqwest::Url) -> Result<SearchPage> {
        let (v, next) = self.json(u, None).await?;
        let items = v
            .as_array()
            .ok_or_else(|| HubError::Parse("search: expected an array".into()))?
            .iter()
            .filter_map(parse_summary)
            .collect();
        let next = next.filter(|n| {
            reqwest::Url::parse(n)
                .map(|u| self.same_origin(&u))
                .unwrap_or(false)
        });
        Ok(SearchPage { items, next })
    }

    /// Resolve `revision` (default `main`) to a commit and read gating and licence.
    pub async fn repo(&self, repo: &str, revision: Option<&str>) -> Result<RepoInfo> {
        validate_repo_id(repo)?;
        let rev = revision.unwrap_or("main");
        validate_revision(rev)?;
        let u = self.url(&format!("/api/models/{repo}/revision/{}", encode_rev(rev)));
        let (v, _) = self.json(u, Some(repo)).await?;
        let sha = v["sha"]
            .as_str()
            .filter(|s| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(|| HubError::Parse("revision: no commit sha".into()))?
            .to_string();
        let tags: Vec<String> = v["tags"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Ok(RepoInfo {
            id: v["id"].as_str().unwrap_or(repo).to_string(),
            sha,
            gated: is_gated(&v["gated"]),
            private: v["private"].as_bool().unwrap_or(false),
            license: license_of(&v, &tags),
            card_url: format!("{}/{repo}", self.endpoint()),
            tags,
        })
    }

    /// Every file of `repo` at commit `sha` (follows the tree API's pages).
    pub async fn files(&self, repo: &str, sha: &str) -> Result<Vec<RepoFile>> {
        validate_repo_id(repo)?;
        validate_commit(sha)?;
        let mut u = self.url(&format!("/api/models/{repo}/tree/{sha}"));
        u.query_pairs_mut().append_pair("recursive", "true");
        let mut out = vec![];
        let mut next = Some(u.to_string());
        let mut pages = 0;
        while let Some(n) = next.take() {
            pages += 1;
            if pages > 50 {
                return Err(HubError::Parse("tree: too many pages".into()));
            }
            let u = reqwest::Url::parse(&n).map_err(|e| HubError::Parse(e.to_string()))?;
            if !self.same_origin(&u) {
                return Err(HubError::Parse("tree: next page on another host".into()));
            }
            let (v, link) = self.json(u, Some(repo)).await?;
            let arr = v
                .as_array()
                .ok_or_else(|| HubError::Parse("tree: expected an array".into()))?;
            for e in arr {
                if e["type"].as_str() != Some("file") {
                    continue;
                }
                let Some(path) = e["path"].as_str() else {
                    continue;
                };
                if validate_repo_path(path).is_err() {
                    continue;
                }
                let sha256 = e["lfs"]["oid"]
                    .as_str()
                    .map(|s| s.trim_start_matches("sha256:").to_ascii_lowercase())
                    .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()));
                let size = e["lfs"]["size"]
                    .as_u64()
                    .or_else(|| e["size"].as_u64())
                    .unwrap_or(0);
                // Unsized entries can't be verified or range-read.
                if size == 0 {
                    continue;
                }
                out.push(RepoFile {
                    path: path.into(),
                    size,
                    sha256,
                });
            }
            next = link;
        }
        Ok(out)
    }

    /// [`files`](Self::files) grouped by [`gguf_variants`]; `NoGguf` when there is no model.
    pub async fn variants(&self, repo: &str, sha: &str) -> Result<Vec<GgufVariant>> {
        let v = gguf_variants(&self.files(repo, sha).await?);
        if v.iter().all(|v| v.kind != VariantKind::Model) {
            return Err(HubError::NoGguf(repo.into()));
        }
        Ok(v)
    }

    /// Bytes `[start, end)` of a repo file, via `Range`. A `200` answer is accepted only for
    /// `start == 0` and is cut at `end` (the rest of the body is never read).
    async fn fetch_range(&self, url: &str, start: u64, end: u64) -> Result<Vec<u8>> {
        if end <= start {
            return Ok(vec![]);
        }
        let u = reqwest::Url::parse(url).map_err(|e| HubError::Parse(e.to_string()))?;
        let mut rb = self
            .http
            .get(u.clone())
            .header(reqwest::header::RANGE, format!("bytes={start}-{}", end - 1));
        if let Some(t) = self.token.as_ref().filter(|_| self.same_origin(&u)) {
            rb = rb.bearer_auth(t.expose());
        }
        let mut resp = rb
            .send()
            .await
            .map_err(|e| HubError::Http(e.without_url().to_string()))?;
        match resp.status().as_u16() {
            206 => {
                let got = resp
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("bytes "))
                    .and_then(|v| v.split('-').next())
                    .and_then(|v| v.trim().parse::<u64>().ok());
                if got != Some(start) {
                    return Err(HubError::RangeUnsupported);
                }
            }
            200 if start == 0 => {}
            200 => return Err(HubError::RangeUnsupported),
            401 if self.token.is_some() => return Err(HubError::Unauthorized),
            401 | 403 => {
                return Err(HubError::Gated {
                    repo: String::new(),
                    has_token: self.token.is_some(),
                });
            }
            404 => return Err(HubError::NotFound(u.path().into())),
            429 => return Err(HubError::RateLimited { retry_after: None }),
            s => {
                return Err(HubError::Status {
                    status: s,
                    body: String::new(),
                });
            }
        }
        let want = (end - start) as usize;
        let mut out = Vec::with_capacity(want.min(16 << 20));
        while out.len() < want {
            match resp
                .chunk()
                .await
                .map_err(|e| HubError::Http(e.without_url().to_string()))?
            {
                Some(c) => out.extend_from_slice(&c[..c.len().min(want - out.len())]),
                None => break,
            }
        }
        Ok(out)
    }

    /// Parse the GGUF header of one repo file from its first bytes only: starts with 1 MiB and
    /// fetches the next, doubling chunk until the header parses (at most
    /// [`crate::gguf::MAX_HEADER_BYTES`], 64 MiB).
    pub async fn gguf_header(&self, repo: &str, sha: &str, file: &RepoFile) -> Result<Gguf> {
        const FIRST: u64 = 1 << 20;
        const MAX: u64 = crate::gguf::MAX_HEADER_BYTES;
        let url = self.file_url(repo, sha, &file.path)?;
        let limit = file.size.min(MAX);
        let mut buf = self.fetch_range(&url, 0, FIRST.min(limit)).await?;
        loop {
            match Gguf::from_prefix(&buf, file.size) {
                Ok(g) => return Ok(g),
                Err(e) if e.is_truncated() && (buf.len() as u64) < limit => {
                    let start = buf.len() as u64;
                    let end = (start * 2).min(limit);
                    let more = self.fetch_range(&url, start, end).await?;
                    if more.is_empty() {
                        return Err(HubError::Gguf("file ended inside the header".into()));
                    }
                    buf.extend_from_slice(&more);
                }
                Err(e) if e.is_truncated() => {
                    return Err(HubError::Gguf(format!(
                        "header larger than {} MiB",
                        limit >> 20
                    )));
                }
                Err(e) => return Err(HubError::Gguf(e.to_string())),
            }
        }
    }

    /// Memory footprint of a variant from its header(s) alone, for [`crate::fit`] before
    /// download. Split variants read every part's header (tensors are spread over parts).
    pub async fn footprint(
        &self,
        info: &RepoInfo,
        variant: &GgufVariant,
    ) -> Result<ModelFootprint> {
        let mut parts = Vec::with_capacity(variant.files.len());
        for f in &variant.files {
            parts.push(self.gguf_header(&info.id, &info.sha, f).await?);
        }
        let g = Gguf::merge_split(parts).ok_or_else(|| HubError::Gguf("empty variant".into()))?;
        Ok(ModelFootprint::from_gguf(&g))
    }

    /// `{endpoint}/{repo}/resolve/{sha}/{path}`.
    pub fn file_url(&self, repo: &str, sha: &str, path: &str) -> Result<String> {
        validate_repo_id(repo)?;
        validate_commit(sha)?;
        validate_repo_path(path)?;
        Ok(self
            .url(&format!("/{repo}/resolve/{sha}/{path}"))
            .to_string())
    }

    /// Where [`pull`](Self::pull) stores `path` of `repo`: `models_dir/hf/<owner>--<name>/<path>`.
    pub fn local_path(models_dir: &Path, repo: &str, path: &str) -> Result<PathBuf> {
        validate_repo_id(repo)?;
        validate_repo_path(path)?;
        let mut p = models_dir.join("hf").join(repo.replace('/', "--"));
        for seg in path.split('/') {
            p.push(seg);
        }
        Ok(p)
    }

    /// Download every part of `variant` from `info` (a repo pinned by [`repo`](Self::repo)),
    /// then write the [`SourceManifest`]. Returns the path to load (the first part).
    pub async fn pull(
        &self,
        info: &RepoInfo,
        variant: &GgufVariant,
        models_dir: &Path,
        progress: &mut (dyn FnMut(PullProgress) + Send),
    ) -> Result<PathBuf> {
        // Refuse up front if any part can't be verified.
        for f in &variant.files {
            if f.sha256.is_none() {
                return Err(HubError::NoChecksum(f.path.clone()));
            }
        }
        let total: u64 = variant.files.iter().map(|f| f.size).sum();
        let parts = variant.files.len();
        let mut before = 0u64;
        let mut first = None;
        for (i, f) in variant.files.iter().enumerate() {
            let url = self.file_url(&info.id, &info.sha, &f.path)?;
            let dest = Self::local_path(models_dir, &info.id, &f.path)?;
            let same_origin = reqwest::Url::parse(&url)
                .map(|u| self.same_origin(&u))
                .unwrap_or(false);
            let bearer = self
                .token
                .as_ref()
                .filter(|_| same_origin)
                .map(|t| t.expose());
            let sha = f.sha256.as_deref().unwrap_or_default();
            let mut cb = |done: u64, _: u64| {
                progress(PullProgress {
                    part: i,
                    parts,
                    done_bytes: before + done,
                    total_bytes: total,
                })
            };
            let p = download::download_to(
                &self.http,
                &Expected {
                    url: &url,
                    size: f.size,
                    sha256: sha,
                    bearer,
                },
                &dest,
                &mut cb,
            )
            .await?;
            first.get_or_insert(p);
            before += f.size;
        }
        let first = first.ok_or_else(|| HubError::InvalidPath("empty variant".into()))?;
        let manifest = SourceManifest {
            source: "huggingface".into(),
            endpoint: self.endpoint().into(),
            repo: info.id.clone(),
            revision: info.sha.clone(),
            files: variant.files.clone(),
            license: info.license.clone(),
            card_url: info.card_url.clone(),
            downloaded_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        let json =
            serde_json::to_vec_pretty(&manifest).map_err(|e| HubError::Parse(e.to_string()))?;
        write_atomic(&SourceManifest::path_for(&first), &json).await?;
        Ok(first)
    }
}

/// Write via a temporary file, fsync, then rename, so a crash never leaves a torn manifest.
async fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    write_atomic_chunks(path, [Ok(data)]).await
}

/// [`write_atomic`] over chunks; a failing chunk (or any I/O error) removes the temporary
/// file and leaves `path` exactly as it was. Chunked so tests can fail it mid-write.
async fn write_atomic_chunks<'a>(
    path: &Path,
    chunks: impl IntoIterator<Item = std::io::Result<&'a [u8]>>,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let r = async {
        let mut f = tokio::fs::File::create(&tmp).await?;
        for c in chunks {
            f.write_all(c?).await?;
        }
        f.sync_all().await?;
        drop(f);
        tokio::fs::rename(&tmp, path).await
    }
    .await;
    if r.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    r
}

fn encode_rev(rev: &str) -> String {
    // Branch names may contain `/`; the revision API wants it percent-encoded.
    rev.replace('/', "%2F")
}

fn is_gated(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty() && s != "false",
        _ => false,
    }
}

fn license_of(v: &Value, tags: &[String]) -> LicenseInfo {
    let card = &v["cardData"];
    let id = card["license"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            card["license"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|x| x.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            tags.iter()
                .find_map(|t| t.strip_prefix("license:").map(str::to_string))
        });
    LicenseInfo::classify(
        id.as_deref(),
        card["license_name"].as_str(),
        card["license_link"].as_str(),
    )
}

fn parse_summary(v: &Value) -> Option<RepoSummary> {
    let id = v["id"].as_str().or_else(|| v["modelId"].as_str())?;
    validate_repo_id(id).ok()?;
    let tags: Vec<String> = v["tags"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Some(RepoSummary {
        author: id.split('/').next().unwrap_or_default().into(),
        id: id.into(),
        downloads: v["downloads"].as_u64().unwrap_or(0),
        likes: v["likes"].as_u64().unwrap_or(0),
        last_modified: v["lastModified"].as_str().map(String::from),
        gated: is_gated(&v["gated"]),
        pipeline_tag: v["pipeline_tag"].as_str().map(String::from),
        license: license_of(v, &tags).id,
        tags,
    })
}

/// `Link: <url>; rel="next"`.
fn next_link(resp: &reqwest::Response) -> Option<String> {
    let h = resp.headers().get(reqwest::header::LINK)?.to_str().ok()?;
    h.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        params
            .split(';')
            .any(|p| p.trim().replace(' ', "") == "rel=\"next\"")
            .then(|| {
                url.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string()
            })
    })
}

/// A value that may be an estimate. The flag travels with the value so a UI can't show an
/// estimated fit as if it had been read from that file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Estimated<T> {
    pub value: T,
    /// `true`: derived from another variant's header; `false`: read from this variant.
    pub estimated: bool,
}

impl Estimated<ModelFootprint> {
    /// [`crate::fit`] on the footprint, keeping the flag.
    pub fn fit(&self, machine: &crate::fit::Machine) -> Estimated<crate::fit::FitReport> {
        Estimated {
            value: crate::fit::fit(&self.value, machine),
            estimated: self.estimated,
        }
    }
}

/// Footprint of `other` from `base_fp`, the footprint read from `base`'s header (same repo,
/// same model, another quant): architecture and KV shape are shared, and weights scale with
/// file size. `estimated` is false only for `base` itself.
pub fn estimate_variant(
    base_fp: &ModelFootprint,
    base: &GgufVariant,
    other: &GgufVariant,
) -> Estimated<ModelFootprint> {
    if other == base {
        return Estimated {
            value: base_fp.clone(),
            estimated: false,
        };
    }
    let r = other.total_size as f64 / base.total_size.max(1) as f64;
    Estimated {
        value: ModelFootprint {
            weight_bytes: (base_fp.weight_bytes as f64 * r) as u64,
            expert_bytes: (base_fp.expert_bytes as f64 * r) as u64,
            quant: other.quant.clone().or_else(|| base_fp.quant.clone()),
            ..base_fp.clone()
        },
        estimated: true,
    }
}

/// Group a repo's files into GGUF variants (one per quantisation; split parts together).
/// Incomplete split sets are dropped. Sorted: models first, then by size.
pub fn gguf_variants(files: &[RepoFile]) -> Vec<GgufVariant> {
    use std::collections::BTreeMap;
    // Keyed by (base, part count) so `m.gguf` and `m-0000k-of-0000n.gguf` stay apart.
    type Parts<'a> = Vec<(u32, u32, &'a RepoFile)>;
    let mut groups: BTreeMap<(String, u32), Parts<'_>> = BTreeMap::new();
    for f in files {
        let lower = f.path.to_ascii_lowercase();
        if !lower.ends_with(".gguf") {
            continue;
        }
        let stem = &f.path[..f.path.len() - 5];
        let (base, idx, of) = match split_suffix(stem) {
            Some((b, i, n)) => (b.to_string(), i, n),
            None => (stem.to_string(), 1, 1),
        };
        groups.entry((base, of)).or_default().push((idx, of, f));
    }
    let mut out = vec![];
    for ((base, _), mut parts) in groups {
        parts.sort_by_key(|(i, _, _)| *i);
        let of = parts[0].1;
        let complete = parts.len() as u32 == of
            && parts
                .iter()
                .enumerate()
                .all(|(k, (i, n, _))| *i == k as u32 + 1 && *n == of);
        if !complete {
            continue;
        }
        let name = base.rsplit('/').next().unwrap_or(&base).to_string();
        let kind = if name.to_ascii_lowercase().contains("mmproj") {
            VariantKind::Projector
        } else {
            VariantKind::Model
        };
        let files: Vec<RepoFile> = parts.into_iter().map(|(_, _, f)| f.clone()).collect();
        out.push(GgufVariant {
            quant: quant_label(&base),
            total_size: files.iter().map(|f| f.size).sum(),
            name,
            kind,
            files,
        });
    }
    out.sort_by_key(|v| (v.kind == VariantKind::Projector, v.total_size));
    out
}

/// `name-00001-of-00003` → (`name`, 1, 3).
fn split_suffix(stem: &str) -> Option<(&str, u32, u32)> {
    let (rest, of) = stem.rsplit_once("-of-")?;
    let (base, idx) = rest.rsplit_once('-')?;
    let digits = |s: &str| s.len() == 5 && s.chars().all(|c| c.is_ascii_digit());
    if !digits(idx) || !digits(of) {
        return None;
    }
    let (i, n): (u32, u32) = (idx.parse().ok()?, of.parse().ok()?);
    (i >= 1 && i <= n).then_some((base, i, n))
}

/// Quantisation label in a file name, e.g. `Qwen3-8B-Q4_K_M` → `Q4_K_M`.
pub fn quant_label(name: &str) -> Option<String> {
    const QUANTS: &[&str] = &[
        "IQ1_S",
        "IQ1_M",
        "IQ2_XXS",
        "IQ2_XS",
        "IQ2_S",
        "IQ2_M",
        "IQ3_XXS",
        "IQ3_XS",
        "IQ3_S",
        "IQ3_M",
        "IQ4_XS",
        "IQ4_NL",
        "Q2_K_S",
        "Q2_K_L",
        "Q2_K_XL",
        "Q2_K",
        "Q3_K_S",
        "Q3_K_M",
        "Q3_K_L",
        "Q3_K_XL",
        "Q4_0_4_4",
        "Q4_0_4_8",
        "Q4_0_8_8",
        "Q4_0",
        "Q4_1",
        "Q4_K_S",
        "Q4_K_M",
        "Q4_K_L",
        "Q4_K_XL",
        "Q5_0",
        "Q5_1",
        "Q5_K_S",
        "Q5_K_M",
        "Q5_K_L",
        "Q5_K_XL",
        "Q6_K_L",
        "Q6_K_XL",
        "Q6_K",
        "Q8_0",
        "Q8_K_XL",
        "TQ1_0",
        "TQ2_0",
        "MXFP4_MOE",
        "MXFP4",
        "BF16",
        "F16",
        "F32",
    ];
    let up = name.rsplit('/').next().unwrap_or(name).to_ascii_uppercase();
    let b = up.as_bytes();
    let boundary = |i: usize| i >= b.len() || !b[i].is_ascii_alphanumeric();
    let mut best: Option<(usize, &str)> = None;
    for q in QUANTS {
        let mut from = 0;
        while let Some(off) = up[from..].find(q) {
            let s = from + off;
            let e = s + q.len();
            // Quant labels are preceded by `-`, `.` or `_` (or start the name).
            let before_ok = s == 0 || !b[s - 1].is_ascii_alphanumeric();
            if before_ok && boundary(e) && best.is_none_or(|(_, bq)| q.len() > bq.len()) {
                best = Some((s, q));
            }
            from = s + 1;
        }
    }
    best.map(|(_, q)| q.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{Reply, serve};
    use std::collections::HashMap;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn f(path: &str, size: u64, sha: Option<&str>) -> RepoFile {
        RepoFile {
            path: path.into(),
            size,
            sha256: sha.map(String::from),
        }
    }

    #[test]
    fn quant_labels() {
        assert_eq!(quant_label("Qwen3-8B-Q4_K_M").as_deref(), Some("Q4_K_M"));
        assert_eq!(
            quant_label("Llama-3.2-3B-Instruct-IQ3_XXS").as_deref(),
            Some("IQ3_XXS")
        );
        assert_eq!(quant_label("gpt-oss-20b-MXFP4").as_deref(), Some("MXFP4"));
        assert_eq!(quant_label("model.BF16").as_deref(), Some("BF16"));
        assert_eq!(
            quant_label("Qwen3-30B-A3B-UD-Q4_K_XL").as_deref(),
            Some("Q4_K_XL")
        );
        assert_eq!(quant_label("q8_0/model-q8_0").as_deref(), Some("Q8_0"));
        assert_eq!(quant_label("readme"), None);
        // Not inside another word.
        assert_eq!(quant_label("XF16Y"), None);
    }

    #[test]
    fn split_files_grouped_and_incomplete_sets_dropped() {
        let files = vec![
            f("Q8_0/m-Q8_0-00002-of-00002.gguf", 20, Some(&"b".repeat(64))),
            f("Q8_0/m-Q8_0-00001-of-00002.gguf", 30, Some(&"a".repeat(64))),
            f("m-Q4_K_M.gguf", 25, Some(&"c".repeat(64))),
            f("m-Q6_K-00001-of-00003.gguf", 10, Some(&"d".repeat(64))),
            f("mmproj-m-f16.gguf", 5, Some(&"e".repeat(64))),
            f("README.md", 1, None),
        ];
        let v = gguf_variants(&files);
        let names: Vec<_> = v.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["m-Q4_K_M", "m-Q8_0", "mmproj-m-f16"]);
        let q8 = &v[1];
        assert_eq!(q8.quant.as_deref(), Some("Q8_0"));
        assert_eq!(q8.total_size, 50);
        assert_eq!(q8.files[0].path, "Q8_0/m-Q8_0-00001-of-00002.gguf");
        assert_eq!(v[2].kind, VariantKind::Projector);
    }

    #[test]
    fn split_set_and_same_named_single_file_stay_apart() {
        let h = Some("a".repeat(64));
        let files = vec![
            f("m.gguf", 5, h.as_deref()),
            f("m-00001-of-00002.gguf", 10, h.as_deref()),
            f("m-00002-of-00002.gguf", 10, h.as_deref()),
        ];
        let v = gguf_variants(&files);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].files.len(), v[1].files.len()), (1, 2));
    }

    #[test]
    fn repo_ids_and_paths_validated() {
        assert!(validate_repo_id("Qwen/Qwen3-8B-GGUF").is_ok());
        for bad in ["Qwen", "a/b/c", "../x", "a/..", "a/b c", "a/b?x", ""] {
            assert!(validate_repo_id(bad).is_err(), "{bad}");
        }
        assert!(validate_repo_path("Q8_0/m.gguf").is_ok());
        for bad in ["../m.gguf", "/m.gguf", "a//b", "a\\b", "C:m", "a/./b", ""] {
            assert!(validate_repo_path(bad).is_err(), "{bad}");
        }
        let p = HubClient::local_path(Path::new("/m"), "o/n", "Q8_0/x.gguf").unwrap();
        assert_eq!(p, Path::new("/m/hf/o--n/Q8_0/x.gguf"));
    }

    #[test]
    fn licences_classified() {
        let c =
            |id: Option<&str>, name: Option<&str>| LicenseInfo::classify(id, name, None).warning;
        assert_eq!(c(Some("apache-2.0"), None), None);
        assert_eq!(c(Some("llama3.1"), None), Some(LicenseWarning::UsePolicy));
        assert_eq!(c(Some("gemma"), None), Some(LicenseWarning::UsePolicy));
        assert_eq!(c(Some("mit"), None), None);
        assert_eq!(c(None, None), Some(LicenseWarning::Missing));
        assert_eq!(
            c(Some("cc-by-nc-4.0"), None),
            Some(LicenseWarning::NonCommercial)
        );
        assert_eq!(
            c(Some("other"), Some("qwen-research")),
            Some(LicenseWarning::NonCommercial)
        );
        assert_eq!(
            c(Some("other"), Some("exaone")),
            Some(LicenseWarning::Custom)
        );
        assert_eq!(c(Some("mnpl"), None), Some(LicenseWarning::NonCommercial));
        assert_eq!(
            c(Some("some-new-licence"), None),
            Some(LicenseWarning::Custom)
        );
    }

    #[test]
    fn endpoint_must_be_https_unless_loopback() {
        let cfg = |e: &str| HubConfig {
            endpoint: Some(e.into()),
            ..Default::default()
        };
        assert!(HubClient::new(cfg("https://hf-mirror.com")).is_ok());
        assert!(HubClient::new(cfg("http://127.0.0.1:8080")).is_ok());
        assert!(matches!(
            HubClient::new(cfg("http://hf-mirror.com")),
            Err(HubError::InsecureEndpoint(_))
        ));
        assert_eq!(
            HubClient::new(HubConfig::default()).unwrap().endpoint(),
            DEFAULT_ENDPOINT
        );
    }

    #[test]
    fn token_redacted_in_debug() {
        let c = HubConfig::default().with_token(Secret::new("hf_secret"));
        assert!(!format!("{c:?}").contains("hf_secret"));
    }

    fn client(url: &str, token: Option<&str>) -> HubClient {
        // Loopback test servers stand in for huggingface.co: opt in to sending the token.
        HubClient::new(HubConfig {
            endpoint: Some(url.into()),
            token: token.map(Secret::new),
            send_token_to_endpoint: true,
        })
        .unwrap()
    }

    fn revision_json(gated: &str) -> String {
        format!(
            r#"{{"id":"o/n","sha":"{SHA}","gated":{gated},"private":false,
                "tags":["gguf","license:apache-2.0"],"cardData":{{"license":"apache-2.0"}}}}"#
        )
    }

    #[tokio::test]
    async fn search_pages_and_cursor_checked() {
        let mut routes = HashMap::new();
        routes.insert(
            "/api/models?filter=gguf&search=qwen&sort=downloads&direction=-1&limit=2&full=false&cardData=true".into(),
            Reply::json(r#"[{"id":"Qwen/Qwen3-8B-GGUF","downloads":10,"likes":2,"tags":["license:apache-2.0"]},{"id":"bad id"}]"#),
        );
        let s = serve(routes).await;
        let c = client(&s.url, None);
        let page = c
            .search(&SearchQuery {
                text: "qwen".into(),
                sort: SearchSort::Downloads,
                limit: Some(2),
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "Qwen/Qwen3-8B-GGUF");
        assert_eq!(page.items[0].license.as_deref(), Some("apache-2.0"));
        assert!(page.next.is_none());
        // A cursor pointing elsewhere is refused.
        assert!(
            c.search_next("https://evil.example/api/models")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn next_link_followed_only_on_same_origin() {
        let mut routes = HashMap::new();
        routes.insert(
            "/api/models?filter=gguf&search=&sort=likes&direction=-1&limit=20&full=false&cardData=true".into(),
            Reply::json(r#"[{"id":"a/b"}]"#).header(
                "link",
                r#"<https://evil.example/api/models?cursor=x>; rel="next""#,
            ),
        );
        let s = serve(routes).await;
        let page = client(&s.url, None)
            .search(&SearchQuery {
                text: String::new(),
                sort: SearchSort::Likes,
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.next.is_none(), "foreign next link must be dropped");
    }

    #[tokio::test]
    async fn repo_resolves_sha_and_gating() {
        let mut routes = HashMap::new();
        routes.insert(
            "/api/models/o/n/revision/main".into(),
            Reply::json(&revision_json("\"manual\"")),
        );
        routes.insert(
            "/api/models/o/locked/revision/main".into(),
            Reply::status(401),
        );
        routes.insert(
            "/api/models/o/busy/revision/main".into(),
            Reply::status(429).header("retry-after", "7"),
        );
        let s = serve(routes).await;
        let c = client(&s.url, None);
        let info = c.repo("o/n", None).await.unwrap();
        assert_eq!(info.sha, SHA);
        assert!(info.gated);
        assert_eq!(info.license.id.as_deref(), Some("apache-2.0"));
        assert_eq!(info.card_url, format!("{}/o/n", s.url));
        assert!(matches!(
            c.repo("o/locked", None).await,
            Err(HubError::Gated {
                has_token: false,
                ..
            })
        ));
        assert!(matches!(
            c.repo("o/busy", None).await,
            Err(HubError::RateLimited {
                retry_after: Some(7)
            })
        ));
        assert!(matches!(
            c.repo("o/missing", None).await,
            Err(HubError::NotFound(_))
        ));
        assert!(matches!(
            client(&s.url, Some("bad")).repo("o/locked", None).await,
            Err(HubError::Unauthorized)
        ));
        let core: CoreError = HubError::Gated {
            repo: "o/n".into(),
            has_token: false,
        }
        .into();
        assert!(
            matches!(core, CoreError::ModelUnavailable { recovery, .. } if recovery == [RecoveryAction::AddProviderKey])
        );
    }

    #[tokio::test]
    async fn mirror_gets_no_token_without_opt_in() {
        let mut routes = HashMap::new();
        routes.insert(
            "/api/models/o/n/revision/main".into(),
            Reply::json(&revision_json("false")),
        );
        let s = serve(routes).await;
        let c = HubClient::new(HubConfig {
            endpoint: Some(s.url.clone()),
            token: Some(Secret::new("hf_tok")),
            send_token_to_endpoint: false,
        })
        .unwrap();
        c.repo("o/n", None).await.unwrap();
        let reqs = s.requests.lock().unwrap();
        assert!(!reqs[0].contains("authorization"), "{}", reqs[0]);
        // The official endpoint keeps it (checked without a request).
        let official = HubClient::new(HubConfig::default().with_token(Secret::new("t"))).unwrap();
        assert!(official.token.is_some());
    }

    #[tokio::test]
    async fn token_dropped_on_redirect_to_another_port() {
        let mut b = HashMap::new();
        b.insert(
            "/api/models/o/n/revision/main".into(),
            Reply::json(&revision_json("false")),
        );
        let target = serve(b).await;
        let mut a = HashMap::new();
        a.insert(
            "/api/models/o/n/revision/main".into(),
            Reply::status(302).header(
                "location",
                &format!("{}/api/models/o/n/revision/main", target.url),
            ),
        );
        let hub = serve(a).await;
        let info = client(&hub.url, Some("hf_tok"))
            .repo("o/n", None)
            .await
            .unwrap();
        assert_eq!(info.sha, SHA);
        assert!(hub.requests.lock().unwrap()[0].contains("authorization: bearer hf_tok"));
        let at_target = target.requests.lock().unwrap();
        assert_eq!(at_target.len(), 1);
        assert!(!at_target[0].contains("authorization"), "{}", at_target[0]);
    }

    #[test]
    fn unsafe_paths_and_commits_rejected() {
        for bad in [
            "CON",
            "nul.gguf",
            "a/COM1.gguf",
            "lpt9",
            "m.gguf.",
            "m.gguf ",
            "a\u{7}b.gguf",
            "a*b.gguf",
            "a?.gguf",
            "a|b",
        ] {
            assert!(validate_repo_path(bad).is_err(), "{bad:?}");
        }
        for ok in ["COM10.gguf", "console.gguf", "Q8_0/m-00001-of-00002.gguf"] {
            assert!(validate_repo_path(ok).is_ok(), "{ok}");
        }
        let c = client("http://127.0.0.1:9", None);
        assert!(c.file_url("o/n", "main", "m.gguf").is_err());
        assert!(c.file_url("o/n", SHA, "m.gguf").is_ok());
        assert_eq!(sanitize_display("ok\u{1b}[31mred"), "ok[31mred");
        let l = LicenseInfo::classify(Some("mit"), None, Some("javascript:alert(1)"));
        assert_eq!(l.link, None);
    }

    #[tokio::test]
    async fn token_sent_to_hub_only() {
        let mut routes = HashMap::new();
        routes.insert(
            "/api/models/o/n/revision/main".into(),
            Reply::json(&revision_json("false")),
        );
        let s = serve(routes).await;
        client(&s.url, Some("hf_tok"))
            .repo("o/n", None)
            .await
            .unwrap();
        let reqs = s.requests.lock().unwrap();
        assert!(reqs[0].contains("authorization: bearer hf_tok"));
    }

    #[tokio::test]
    async fn tree_reads_lfs_sha_and_pull_verifies() {
        let data = b"GGUF-fake-weights-0123456789".to_vec();
        let sha = {
            use sha2::{Digest, Sha256};
            Sha256::digest(&data)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let tree = format!(
            r#"[{{"type":"file","path":"m-Q4_K_M.gguf","size":134,"oid":"x",
                 "lfs":{{"oid":"{sha}","size":{},"pointerSize":134}}}},
                {{"type":"file","path":"README.md","size":10,"oid":"y"}},
                {{"type":"file","path":"empty.gguf","size":0,"oid":"e"}},
                {{"type":"directory","path":"sub"}},
                {{"type":"file","path":"../evil.gguf","size":1,"oid":"z"}}]"#,
            data.len()
        );
        let mut routes = HashMap::new();
        routes.insert(
            "/api/models/o/n/revision/main".into(),
            Reply::json(&revision_json("false")),
        );
        routes.insert(
            format!("/api/models/o/n/tree/{SHA}?recursive=true"),
            Reply::json(&tree),
        );
        routes.insert(
            format!("/o/n/resolve/{SHA}/m-Q4_K_M.gguf"),
            Reply::bytes(data.clone()),
        );
        let s = serve(routes).await;
        let c = client(&s.url, Some("hf_tok"));
        let info = c.repo("o/n", None).await.unwrap();
        let files = c.files("o/n", &info.sha).await.unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].sha256.as_deref(), Some(sha.as_str()));
        assert_eq!(files[0].size, data.len() as u64);
        assert_eq!(files[1].sha256, None);

        let variants = gguf_variants(&files);
        assert_eq!(variants.len(), 1);
        let dir = std::env::temp_dir().join(format!("dn-hub-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut last = None;
        let p = c
            .pull(&info, &variants[0], &dir, &mut |pr| last = Some(pr))
            .await
            .unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), data);
        assert_eq!(p, dir.join("hf/o--n/m-Q4_K_M.gguf"));
        let last = last.unwrap();
        assert_eq!(
            (last.done_bytes, last.total_bytes, last.parts),
            (data.len() as u64, data.len() as u64, 1)
        );
        let m = SourceManifest::read(&p).unwrap();
        assert_eq!((m.repo.as_str(), m.revision.as_str()), ("o/n", SHA));
        assert_eq!(m.license.id.as_deref(), Some("apache-2.0"));
        // The file download carried the token (same origin as the Hub endpoint).
        let reqs = s.requests.lock().unwrap();
        assert!(
            reqs.iter()
                .any(|r| r.contains("/resolve/") && r.contains("bearer hf_tok"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fake_gguf(vocab: usize, tensors: &[(&str, &[u64], u32, u64)]) -> Vec<u8> {
        use crate::gguf::write::{V, gguf};
        let toks: Vec<String> = (0..vocab).map(|i| format!("tok{i}")).collect();
        let toks: Vec<&str> = toks.iter().map(String::as_str).collect();
        gguf(
            &[
                ("general.architecture", V::Str("llama")),
                ("llama.block_count", V::U32(2)),
                ("llama.embedding_length", V::U32(64)),
                ("llama.attention.head_count", V::U32(4)),
                ("llama.context_length", V::U32(4096)),
                ("tokenizer.ggml.tokens", V::StrArr(&toks)),
            ],
            tensors,
        )
    }

    #[tokio::test]
    async fn footprint_from_range_reads_only() {
        // ~2.6 MB header (forces a second range read), 8 MB of tensor data never fetched.
        let file = fake_gguf(
            200_000,
            &[
                ("blk.0.attn_q.weight", &[64, 64], 8, 4 << 20),
                ("blk.1.attn_q.weight", &[64, 64], 8, 4 << 20),
            ],
        );
        let full = ModelFootprint::from_gguf(&Gguf::from_prefix(&file, file.len() as u64).unwrap());
        let mut routes = HashMap::new();
        routes.insert(
            format!("/o/n/resolve/{SHA}/m.gguf"),
            Reply::bytes(file.clone()).with_ranges(),
        );
        let s = serve(routes).await;
        let c = client(&s.url, None);
        let info = RepoInfo {
            id: "o/n".into(),
            sha: SHA.into(),
            gated: false,
            private: false,
            license: LicenseInfo::classify(Some("mit"), None, None),
            card_url: String::new(),
            tags: vec![],
        };
        let v = GgufVariant {
            name: "m".into(),
            quant: None,
            kind: VariantKind::Model,
            files: vec![f("m.gguf", file.len() as u64, None)],
            total_size: file.len() as u64,
        };
        let fp = c.footprint(&info, &v).await.unwrap();
        assert_eq!(fp, full);
        assert_eq!(fp.weight_bytes, 8 << 20);
        assert_eq!(fp.n_vocab, 200_000);
        let reqs = s.requests.lock().unwrap();
        assert!(reqs.len() >= 2, "header spans more than the first MiB");
        assert!(reqs.iter().all(|r| r.contains("range: bytes=")));
    }

    #[tokio::test]
    async fn split_footprint_sums_parts() {
        let p1 = fake_gguf(10, &[("blk.0.attn_q.weight", &[64, 64], 8, 1000)]);
        let p2 = {
            use crate::gguf::write::{V, gguf};
            gguf(
                &[("split.no", V::U32(1))],
                &[("blk.1.attn_q.weight", &[64, 64], 8, 3000)],
            )
        };
        let mut routes = HashMap::new();
        routes.insert(
            format!("/o/n/resolve/{SHA}/m-00001-of-00002.gguf"),
            Reply::bytes(p1.clone()).with_ranges(),
        );
        routes.insert(
            format!("/o/n/resolve/{SHA}/m-00002-of-00002.gguf"),
            Reply::bytes(p2.clone()).with_ranges(),
        );
        let s = serve(routes).await;
        let files = vec![
            f("m-00001-of-00002.gguf", p1.len() as u64, None),
            f("m-00002-of-00002.gguf", p2.len() as u64, None),
        ];
        let v = gguf_variants(&files).remove(0);
        let info = RepoInfo {
            id: "o/n".into(),
            sha: SHA.into(),
            gated: false,
            private: false,
            license: LicenseInfo::classify(Some("mit"), None, None),
            card_url: String::new(),
            tags: vec![],
        };
        let fp = client(&s.url, None).footprint(&info, &v).await.unwrap();
        // Both parts' tensors count (sizes include each part's alignment padding).
        assert!(
            (4000..4100).contains(&fp.weight_bytes),
            "{}",
            fp.weight_bytes
        );
        assert_eq!(fp.n_layer, 2);
    }

    #[test]
    fn estimate_scales_weights_by_file_size() {
        let base = ModelFootprint {
            weight_bytes: 4_000,
            expert_bytes: 2_000,
            n_params: 8,
            n_layer: 2,
            n_ctx_train: 4096,
            n_vocab: 10,
            n_embd: 64,
            kv_elems_per_token: 128,
            swa_kv_elems_per_token: 0,
            sliding_window: 0,
            quant: Some("Q4_K_M".into()),
        };
        let other = GgufVariant {
            name: "m-Q8_0".into(),
            quant: Some("Q8_0".into()),
            kind: VariantKind::Model,
            files: vec![],
            total_size: 8_200,
        };
        let base_v = GgufVariant {
            name: "m-Q4_K_M".into(),
            quant: Some("Q4_K_M".into()),
            kind: VariantKind::Model,
            files: vec![],
            total_size: 4_100,
        };
        let e = estimate_variant(&base, &base_v, &other);
        assert!(e.estimated);
        let e = e.value;
        assert_eq!((e.weight_bytes, e.expert_bytes), (8_000, 4_000));
        assert_eq!(e.quant.as_deref(), Some("Q8_0"));
        assert_eq!(e.kv_elems_per_token, 128);
        // The variant whose header was read is exact.
        let own = estimate_variant(&base, &base_v, &base_v);
        assert!(!own.estimated);
        assert_eq!(own.value, base);
    }

    #[tokio::test]
    async fn non_gguf_and_ignored_range_rejected() {
        let mut routes = HashMap::new();
        routes.insert(
            format!("/o/n/resolve/{SHA}/bad.gguf"),
            Reply::bytes(b"NOTAGGUF".repeat(10)).with_ranges(),
        );
        // No range support and a header longer than the first read.
        routes.insert(
            format!("/o/n/resolve/{SHA}/big.gguf"),
            Reply::bytes(fake_gguf(200_000, &[])),
        );
        let s = serve(routes).await;
        let c = client(&s.url, None);
        assert!(matches!(
            c.gguf_header("o/n", SHA, &f("bad.gguf", 80, None)).await,
            Err(HubError::Gguf(_))
        ));
        let big = fake_gguf(200_000, &[]).len() as u64;
        assert!(matches!(
            c.gguf_header("o/n", SHA, &f("big.gguf", big, None)).await,
            Err(HubError::RangeUnsupported)
        ));
    }

    #[tokio::test]
    async fn hub_json_body_capped_at_16_mib() {
        // A 17 MiB array, once with Content-Length and once streamed without it.
        let big = format!("[{}\"x\"]", "\"x\",".repeat((17 << 20) / 4));
        let mut routes = HashMap::new();
        routes.insert("/api/models/o/n/revision/main".into(), Reply::json(&big));
        routes.insert(
            "/api/models/o/m/revision/main".into(),
            Reply::json(&big).without_length(),
        );
        let s = serve(routes).await;
        let c = client(&s.url, None);
        for repo in ["o/n", "o/m"] {
            match c.repo(repo, None).await {
                Err(HubError::Parse(m)) => assert!(m.contains("too large"), "{repo}: {m}"),
                other => panic!("{repo}: expected the size cap, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn manifest_write_is_atomic() {
        let dir = std::env::temp_dir().join(format!("dn-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("m.gguf.source.json");
        std::fs::write(&p, b"{\"old\":true}").unwrap();
        // The write fails after its first chunk: the old manifest must survive intact and
        // no temporary file may be left behind.
        let r = write_atomic_chunks(
            &p,
            [
                Ok(&b"{\"new\":"[..]),
                Err(std::io::Error::other("disk full")),
            ],
        )
        .await;
        assert!(r.is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"{\"old\":true}");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "tmp file left behind"
        );
        write_atomic(&p, b"{\"new\":true}").await.unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"{\"new\":true}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn empty_range_makes_no_request() {
        let s = serve(HashMap::new()).await;
        let c = client(&s.url, None);
        let url = c.file_url("o/n", SHA, "m.gguf").unwrap();
        assert!(c.fetch_range(&url, 5, 5).await.unwrap().is_empty());
        assert!(c.fetch_range(&url, 9, 3).await.unwrap().is_empty());
        assert!(s.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn file_without_lfs_sha_refused() {
        let c = client("http://127.0.0.1:9", None);
        let info = RepoInfo {
            id: "o/n".into(),
            sha: SHA.into(),
            gated: false,
            private: false,
            license: LicenseInfo::classify(None, None, None),
            card_url: String::new(),
            tags: vec![],
        };
        let v = GgufVariant {
            name: "m".into(),
            quant: None,
            kind: VariantKind::Model,
            files: vec![f("m.gguf", 1, None)],
            total_size: 1,
        };
        let e = c
            .pull(&info, &v, &std::env::temp_dir(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(matches!(e, HubError::NoChecksum(_)));
    }
}
