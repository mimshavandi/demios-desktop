//! Web search through the backend the user chose, with the user's own key (#369).
//!
//! One interface, [`SearchProvider`], and one type per backend: Brave (the suggested
//! default), Linkup, Exa and Tavily with the user's key, and SearXNG at a URL the user (or
//! an add-on) gives. Every call goes from the user's device or Node; there is no Demios
//! proxy.
//!
//! - **One backend, no fallback.** A call uses the backend it was given, or fails and says
//!   why (a bad key, used-up quota, a rate limit, the service unreachable). It never tries
//!   another backend instead.
//! - **Keys** come from the caller (the app reads them from the keychain) and are sent only
//!   to that backend's fixed address. Redirects aren't followed, so a key is never sent on
//!   to another address.
//! - **SearXNG** may be on the user's own network: its URL is the user's choice, so it
//!   doesn't go through the fetcher's private-network guard. http and https only.
//! - **Results are untrusted**: titles and snippets come from the web. Whoever puts them in
//!   a prompt wraps them with `dn_core::untrusted::block`.

use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// A boxed future, so providers can be used as `dyn SearchProvider`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The most results asked for.
pub const MAX_RESULTS: u8 = 20;
/// The longest snippet kept, in characters.
pub const MAX_SNIPPET_CHARS: usize = 600;

/// What to search for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    /// The search terms.
    pub q: String,
    /// How many results, 1 to [`MAX_RESULTS`].
    pub count: u8,
}

impl SearchQuery {
    /// A query for `count` results (clamped to 1..=[`MAX_RESULTS`]).
    pub fn new(q: impl Into<String>, count: u8) -> Self {
        Self {
            q: q.into(),
            count: count.clamp(1, MAX_RESULTS),
        }
    }
}

/// One result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    /// A short excerpt, at most [`MAX_SNIPPET_CHARS`].
    pub snippet: String,
    /// When it was published, as the backend says, if it does.
    pub published: Option<String>,
}

/// Why a search failed. Each says what to do.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SearchError {
    #[error("the search terms are empty")]
    EmptyQuery,
    #[error("{0} didn't accept the key; check it in Settings › Web search")]
    BadKey(&'static str),
    #[error("{0} says the key's quota or credits are used up")]
    Quota(&'static str),
    #[error("{0} is limiting requests; try again in a little while")]
    RateLimited(&'static str),
    #[error(
        "the SearXNG instance doesn't allow JSON results; turn on `json` under `search.formats` in its settings.yml"
    )]
    SearxngJsonOff,
    #[error("{0} answered {1}")]
    Status(&'static str, u16),
    #[error("{0} sent an answer that couldn't be read: {1}")]
    BadAnswer(&'static str, String),
    #[error("couldn't reach {0}: {1}")]
    Network(&'static str, String),
    #[error("`{0}` isn't an http or https address")]
    BadUrl(String),
}

/// A web search backend.
pub trait SearchProvider: Send + Sync {
    /// Its name, for messages (`Brave`, `SearXNG`).
    fn name(&self) -> &'static str;
    /// Runs one search.
    fn search<'a>(
        &'a self,
        q: &'a SearchQuery,
    ) -> BoxFuture<'a, Result<Vec<SearchResult>, SearchError>>;
}

/// The HTTP client the providers share: no redirects (a key is never sent on), a time
/// limit, and an honest user agent.
pub fn client() -> Result<reqwest::Client, SearchError> {
    reqwest::Client::builder()
        .user_agent(format!(
            "Demios/{} (+https://demios.io/bot)",
            env!("CARGO_PKG_VERSION")
        ))
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| SearchError::Network("the search service", e.to_string()))
}

fn snippet(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= MAX_SNIPPET_CHARS {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(MAX_SNIPPET_CHARS).collect();
        out.push('…');
        out
    }
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn opt_text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Reads the result list at `path` (`["web", "results"]`), mapping each with `f`, and drops
/// results without a URL.
fn results(
    name: &'static str,
    body: &Value,
    path: &[&str],
    f: impl Fn(&Value) -> SearchResult,
) -> Result<Vec<SearchResult>, SearchError> {
    let mut v = body;
    for k in path {
        v = match v.get(k) {
            Some(x) => x,
            // No results section at all: Brave leaves `web` out when nothing matched.
            None if path.len() > 1 && *k == path[0] => return Ok(Vec::new()),
            None => {
                return Err(SearchError::BadAnswer(
                    name,
                    format!("no `{}`", path.join(".")),
                ));
            }
        };
    }
    let list = v.as_array().ok_or_else(|| {
        SearchError::BadAnswer(name, format!("`{}` isn't a list", path.join(".")))
    })?;
    Ok(list
        .iter()
        .map(f)
        .filter(|r| r.url.starts_with("http://") || r.url.starts_with("https://"))
        .collect())
}

/// Sends `req` and reads a JSON answer, mapping the backends' error statuses.
async fn send(name: &'static str, req: reqwest::RequestBuilder) -> Result<Value, SearchError> {
    let resp = req
        .send()
        .await
        .map_err(|e| SearchError::Network(name, e.without_url().to_string()))?;
    let status = resp.status().as_u16();
    match status {
        200..=299 => {}
        401 | 403 => return Err(SearchError::BadKey(name)),
        402 | 432 | 433 => return Err(SearchError::Quota(name)),
        429 => return Err(SearchError::RateLimited(name)),
        _ => return Err(SearchError::Status(name, status)),
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| SearchError::Network(name, e.without_url().to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| SearchError::BadAnswer(name, e.to_string()))
}

fn body_of(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

/// `endpoint` with `pairs` as its query.
fn with_query(endpoint: &str, pairs: &[(&str, &str)]) -> String {
    match reqwest::Url::parse(endpoint) {
        Ok(mut u) => {
            u.query_pairs_mut().extend_pairs(pairs);
            u.to_string()
        }
        Err(_) => endpoint.to_owned(),
    }
}

fn check_query(q: &SearchQuery) -> Result<(), SearchError> {
    if q.q.trim().is_empty() {
        Err(SearchError::EmptyQuery)
    } else {
        Ok(())
    }
}

/// Brave Search: `GET /res/v1/web/search`, key in `X-Subscription-Token`.
pub struct Brave {
    http: reqwest::Client,
    key: String,
    endpoint: String,
}

impl Brave {
    pub fn new(http: reqwest::Client, key: impl Into<String>) -> Self {
        Self {
            http,
            key: key.into(),
            endpoint: "https://api.search.brave.com/res/v1/web/search".into(),
        }
    }
}

impl SearchProvider for Brave {
    fn name(&self) -> &'static str {
        "Brave"
    }

    fn search<'a>(
        &'a self,
        q: &'a SearchQuery,
    ) -> BoxFuture<'a, Result<Vec<SearchResult>, SearchError>> {
        Box::pin(async move {
            check_query(q)?;
            let req = self
                .http
                .get(with_query(
                    &self.endpoint,
                    &[("q", q.q.as_str()), ("count", &q.count.to_string())],
                ))
                .header("X-Subscription-Token", &self.key)
                .header("Accept", "application/json");
            let body = send(self.name(), req).await?;
            results(self.name(), &body, &["web", "results"], |r| SearchResult {
                title: text(r, "title"),
                url: text(r, "url"),
                snippet: snippet(&text(r, "description")),
                published: opt_text(r, "page_age").or_else(|| opt_text(r, "age")),
            })
        })
    }
}

/// Tavily: `POST /search`, key as a bearer token.
pub struct Tavily {
    http: reqwest::Client,
    key: String,
    endpoint: String,
}

impl Tavily {
    pub fn new(http: reqwest::Client, key: impl Into<String>) -> Self {
        Self {
            http,
            key: key.into(),
            endpoint: "https://api.tavily.com/search".into(),
        }
    }
}

impl SearchProvider for Tavily {
    fn name(&self) -> &'static str {
        "Tavily"
    }

    fn search<'a>(
        &'a self,
        q: &'a SearchQuery,
    ) -> BoxFuture<'a, Result<Vec<SearchResult>, SearchError>> {
        Box::pin(async move {
            check_query(q)?;
            let req = self
                .http
                .post(&self.endpoint)
                .bearer_auth(&self.key)
                .header("Content-Type", "application/json")
                .body(body_of(&json!({
                    "query": q.q,
                    "max_results": q.count,
                    "search_depth": "basic",
                    "include_published_date": true,
                })));
            let body = send(self.name(), req).await?;
            results(self.name(), &body, &["results"], |r| SearchResult {
                title: text(r, "title"),
                url: text(r, "url"),
                snippet: snippet(&text(r, "content")),
                published: opt_text(r, "published_date"),
            })
        })
    }
}

/// Exa: `POST /search`, key in `x-api-key`, highlights as the snippet.
pub struct Exa {
    http: reqwest::Client,
    key: String,
    endpoint: String,
}

impl Exa {
    pub fn new(http: reqwest::Client, key: impl Into<String>) -> Self {
        Self {
            http,
            key: key.into(),
            endpoint: "https://api.exa.ai/search".into(),
        }
    }
}

impl SearchProvider for Exa {
    fn name(&self) -> &'static str {
        "Exa"
    }

    fn search<'a>(
        &'a self,
        q: &'a SearchQuery,
    ) -> BoxFuture<'a, Result<Vec<SearchResult>, SearchError>> {
        Box::pin(async move {
            check_query(q)?;
            let req = self
                .http
                .post(&self.endpoint)
                .header("x-api-key", &self.key)
                .header("Content-Type", "application/json")
                .body(body_of(&json!({
                    "query": q.q,
                    "numResults": q.count,
                    "contents": { "highlights": { "maxCharacters": MAX_SNIPPET_CHARS } },
                })));
            let body = send(self.name(), req).await?;
            results(self.name(), &body, &["results"], |r| {
                let highlights = r
                    .get("highlights")
                    .and_then(Value::as_array)
                    .map(|h| {
                        h.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" … ")
                    })
                    .unwrap_or_default();
                SearchResult {
                    title: text(r, "title"),
                    url: text(r, "url"),
                    snippet: snippet(if highlights.is_empty() {
                        r.get("text").and_then(Value::as_str).unwrap_or_default()
                    } else {
                        &highlights
                    }),
                    published: opt_text(r, "publishedDate"),
                }
            })
        })
    }
}

/// Linkup: `POST /v1/search` for search results, key as a bearer token.
pub struct Linkup {
    http: reqwest::Client,
    key: String,
    endpoint: String,
}

impl Linkup {
    pub fn new(http: reqwest::Client, key: impl Into<String>) -> Self {
        Self {
            http,
            key: key.into(),
            endpoint: "https://api.linkup.so/v1/search".into(),
        }
    }
}

impl SearchProvider for Linkup {
    fn name(&self) -> &'static str {
        "Linkup"
    }

    fn search<'a>(
        &'a self,
        q: &'a SearchQuery,
    ) -> BoxFuture<'a, Result<Vec<SearchResult>, SearchError>> {
        Box::pin(async move {
            check_query(q)?;
            let req = self
                .http
                .post(&self.endpoint)
                .bearer_auth(&self.key)
                .header("Content-Type", "application/json")
                .body(body_of(&json!({
                    "q": q.q,
                    "depth": "standard",
                    "outputType": "searchResults",
                    "maxResults": q.count,
                })));
            let body = send(self.name(), req).await?;
            results(self.name(), &body, &["results"], |r| SearchResult {
                title: text(r, "name"),
                url: text(r, "url"),
                snippet: snippet(&text(r, "content")),
                published: opt_text(r, "publishedAt"),
            })
        })
    }
}

/// SearXNG at the user's URL: `GET <base>/search?format=json`. The instance must allow the
/// JSON format.
pub struct Searxng {
    http: reqwest::Client,
    endpoint: String,
}

impl Searxng {
    /// A SearXNG instance at `base` (`https://search.example.org`), http or https.
    pub fn new(http: reqwest::Client, base: &str) -> Result<Self, SearchError> {
        let url = reqwest::Url::parse(base).map_err(|_| SearchError::BadUrl(base.into()))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(SearchError::BadUrl(base.into()));
        }
        let endpoint = url
            .join("search")
            .map_err(|_| SearchError::BadUrl(base.into()))?;
        Ok(Self {
            http,
            endpoint: endpoint.to_string(),
        })
    }
}

impl SearchProvider for Searxng {
    fn name(&self) -> &'static str {
        "SearXNG"
    }

    fn search<'a>(
        &'a self,
        q: &'a SearchQuery,
    ) -> BoxFuture<'a, Result<Vec<SearchResult>, SearchError>> {
        Box::pin(async move {
            check_query(q)?;
            let req = self.http.get(with_query(
                &self.endpoint,
                &[("q", q.q.as_str()), ("format", "json")],
            ));
            let body = match send(self.name(), req).await {
                Err(SearchError::BadKey(_)) => return Err(SearchError::SearxngJsonOff),
                other => other?,
            };
            let mut out = results(self.name(), &body, &["results"], |r| SearchResult {
                title: text(r, "title"),
                url: text(r, "url"),
                snippet: snippet(&text(r, "content")),
                published: opt_text(r, "publishedDate"),
            })?;
            // SearXNG has no result count: keep the first `count`.
            out.truncate(usize::from(q.count));
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// One canned answer; records the request it got.
    async fn server(status: &str, body: &str) -> (String, Arc<Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(String::new()));
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let s = seen.clone();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16384];
            let mut req = Vec::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).to_string();
                let done = match text.split_once("\r\n\r\n") {
                    Some((head, rest)) => {
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        rest.len() >= len
                    }
                    None => false,
                };
                if n == 0 || done {
                    break;
                }
            }
            *s.lock().unwrap() = String::from_utf8_lossy(&req).to_string();
            let _ = sock.write_all(resp.as_bytes()).await;
        });
        (base, seen)
    }

    fn q() -> SearchQuery {
        SearchQuery::new("tide tables", 3)
    }

    #[tokio::test]
    async fn brave() {
        let (base, seen) = server(
            "200 OK",
            r#"{"query":{"original":"tide tables"},"web":{"results":[
                {"title":"Tides","url":"https://tides.example.org/","description":"Tide <strong>tables</strong>","page_age":"2026-11-20T10:00:00"},
                {"title":"No url"},
                {"title":"Bad scheme","url":"javascript:alert(1)","description":"x"}]}}"#,
        )
        .await;
        let mut b = Brave::new(client().unwrap(), "k-123");
        b.endpoint = format!("{base}/res/v1/web/search");
        let r = b.search(&q()).await.unwrap();
        assert_eq!(r.len(), 1, "results without an http(s) URL are dropped");
        assert_eq!(r[0].title, "Tides");
        assert_eq!(r[0].published.as_deref(), Some("2026-11-20T10:00:00"));
        let req = seen.lock().unwrap().to_ascii_lowercase();
        assert!(
            req.starts_with("get /res/v1/web/search?q=tide+tables&count=3 "),
            "{req}"
        );
        assert!(req.contains("x-subscription-token: k-123"), "{req}");
    }

    #[tokio::test]
    async fn brave_with_no_web_section_has_no_results() {
        let (base, _) = server("200 OK", r#"{"query":{"original":"zzz"}}"#).await;
        let mut b = Brave::new(client().unwrap(), "k");
        b.endpoint = format!("{base}/x");
        assert_eq!(b.search(&q()).await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn tavily() {
        let (base, seen) = server(
            "200 OK",
            r#"{"query":"tide tables","results":[{"title":"Tides","url":"https://t.example.org/","content":"Tables","score":0.9,"published_date":"Tue, 11 Mar 2025 17:00:00 GMT"}]}"#,
        )
        .await;
        let mut t = Tavily::new(client().unwrap(), "tvly-1");
        t.endpoint = format!("{base}/search");
        let r = t.search(&q()).await.unwrap();
        assert_eq!(r[0].snippet, "Tables");
        assert_eq!(
            r[0].published.as_deref(),
            Some("Tue, 11 Mar 2025 17:00:00 GMT")
        );
        let req = seen.lock().unwrap().clone();
        assert!(
            req.to_ascii_lowercase()
                .contains("authorization: bearer tvly-1"),
            "{req}"
        );
        assert!(
            req.contains(r#""query":"tide tables""#) && req.contains(r#""max_results":3"#),
            "{req}"
        );
    }

    #[tokio::test]
    async fn exa_uses_highlights() {
        let (base, seen) = server(
            "200 OK",
            r#"{"requestId":"r","results":[{"title":"Tides","url":"https://e.example.org/","publishedDate":"2026-01-02T00:00:00.000Z","highlights":["one","two"]}]}"#,
        )
        .await;
        let mut e = Exa::new(client().unwrap(), "exa-1");
        e.endpoint = format!("{base}/search");
        let r = e.search(&q()).await.unwrap();
        assert_eq!(r[0].snippet, "one … two");
        let req = seen.lock().unwrap().clone();
        assert!(
            req.to_ascii_lowercase().contains("x-api-key: exa-1"),
            "{req}"
        );
        assert!(req.contains(r#""numResults":3"#), "{req}");
    }

    #[tokio::test]
    async fn linkup() {
        let (base, seen) = server(
            "200 OK",
            r#"{"results":[{"type":"text","name":"Tides","url":"https://l.example.org/","content":"Tables","publishedAt":"2026-11-01"}]}"#,
        )
        .await;
        let mut l = Linkup::new(client().unwrap(), "lk-1");
        l.endpoint = format!("{base}/v1/search");
        let r = l.search(&q()).await.unwrap();
        assert_eq!(r[0].title, "Tides");
        assert_eq!(r[0].published.as_deref(), Some("2026-11-01"));
        let req = seen.lock().unwrap().clone();
        assert!(
            req.contains(r#""outputType":"searchResults""#) && req.contains(r#""maxResults":3"#),
            "{req}"
        );
    }

    #[tokio::test]
    async fn searxng_keeps_the_first_count_and_says_when_json_is_off() {
        let (base, seen) = server(
            "200 OK",
            r#"{"query":"tide tables","results":[
                {"title":"A","url":"https://a.example.org/","content":"a"},
                {"title":"B","url":"https://b.example.org/","content":"b"},
                {"title":"C","url":"https://c.example.org/","content":"c"},
                {"title":"D","url":"https://d.example.org/","content":"d"}]}"#,
        )
        .await;
        let s = Searxng::new(client().unwrap(), &base).unwrap();
        let r = s.search(&q()).await.unwrap();
        assert_eq!(
            r.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
        assert!(
            seen.lock()
                .unwrap()
                .starts_with("GET /search?q=tide+tables&format=json ")
        );

        let (base, _) = server("403 Forbidden", "{}").await;
        let s = Searxng::new(client().unwrap(), &base).unwrap();
        assert_eq!(s.search(&q()).await, Err(SearchError::SearxngJsonOff));

        for bad in ["ftp://x", "not a url", "http://user:pw@x/"] {
            assert!(Searxng::new(client().unwrap(), bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn errors_say_what_to_do_and_nothing_falls_back() {
        for (status, want) in [
            ("401 Unauthorized", SearchError::BadKey("Tavily")),
            ("432 Plan Limit", SearchError::Quota("Tavily")),
            ("429 Too Many Requests", SearchError::RateLimited("Tavily")),
            (
                "500 Internal Server Error",
                SearchError::Status("Tavily", 500),
            ),
        ] {
            let (base, _) = server(status, "{}").await;
            let mut t = Tavily::new(client().unwrap(), "k");
            t.endpoint = format!("{base}/search");
            assert_eq!(t.search(&q()).await, Err(want));
        }
        let (base, _) = server("200 OK", "not json").await;
        let mut t = Tavily::new(client().unwrap(), "k");
        t.endpoint = format!("{base}/search");
        assert!(matches!(
            t.search(&q()).await,
            Err(SearchError::BadAnswer(..))
        ));
        let t = Tavily::new(client().unwrap(), "k");
        assert_eq!(
            t.search(&SearchQuery::new("  ", 3)).await,
            Err(SearchError::EmptyQuery)
        );
    }

    #[tokio::test]
    async fn a_redirect_isnt_followed_so_the_key_isnt_sent_on() {
        let (base, _) = server("302 Found\r\nLocation: http://127.0.0.1:1/steal", "{}").await;
        let mut b = Brave::new(client().unwrap(), "secret");
        b.endpoint = format!("{base}/x");
        assert_eq!(b.search(&q()).await, Err(SearchError::Status("Brave", 302)));
    }

    #[test]
    fn snippets_are_capped_and_counts_clamped() {
        let long = "x".repeat(MAX_SNIPPET_CHARS + 10);
        assert_eq!(snippet(&long).chars().count(), MAX_SNIPPET_CHARS + 1);
        assert_eq!(SearchQuery::new("a", 0).count, 1);
        assert_eq!(SearchQuery::new("a", 99).count, MAX_RESULTS);
    }
}
