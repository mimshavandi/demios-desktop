//! The page fetcher (#369): one GET, with the guard, caps and etiquette the issue sets.
//!
//! - **Where it may go:** http and https only, no user name or password in the address, the
//!   default ports only, and never a non-public address ([`crate::guard`]): names are
//!   checked when they resolve, IP literals before the request. The system proxy is not
//!   used, since a proxy would make the connection somewhere the guard never checked.
//! - **Redirects:** at most [`FetcherConfig::max_redirects`], each hop checked like the
//!   first address, and only within the same host: a redirect to another host stops with
//!   [`FetchError::RedirectedElsewhere`], naming the address, for the caller to decide on.
//! - **Caps:** a total time limit, and a size limit enforced while the body streams in (a
//!   larger page is refused, never cut silently). Only text types the extractor reads are
//!   accepted: HTML, XHTML, plain text and markdown.
//! - **Etiquette:** an honest user agent naming Demios, and per host at most
//!   [`FetcherConfig::per_host_concurrent`] requests at once and one start per
//!   [`FetcherConfig::per_host_interval`].
//!
//! - **robots.txt:** read per site and kept for a day (RFC 9309): a missing one allows
//!   everything, one the site can't serve allows nothing. Only the requested site's rules
//!   are read; a redirect to another site follows that site's address checks, not its
//!   robots.txt.

use crate::guard::{self, BlockedHost, GuardResolver};
use crate::rate::RateLimit;
use reqwest::Url;
use std::sync::Arc;
use std::time::Duration;

/// The fetcher's limits.
#[derive(Clone, Debug)]
pub struct FetcherConfig {
    /// Sent with every request: names Demios and where to read about it.
    pub user_agent: String,
    /// The most time one fetch may take, redirects and body included.
    pub timeout: Duration,
    /// The most time to open a connection.
    pub connect_timeout: Duration,
    /// The largest body accepted, in bytes.
    pub max_bytes: usize,
    /// The most redirects followed.
    pub max_redirects: usize,
    /// The least time between the starts of two requests to one host.
    pub per_host_interval: Duration,
    /// The most requests to one host at a time.
    pub per_host_concurrent: usize,
}

impl Default for FetcherConfig {
    fn default() -> Self {
        Self {
            user_agent: format!(
                "Demios/{} (+https://demios.io/bot)",
                env!("CARGO_PKG_VERSION")
            ),
            timeout: Duration::from_secs(15),
            connect_timeout: Duration::from_secs(5),
            max_bytes: 5 * 1024 * 1024,
            max_redirects: 5,
            per_host_interval: Duration::from_secs(1),
            per_host_concurrent: 2,
        }
    }
}

/// A fetched page, before extraction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// The address asked for.
    pub url: String,
    /// Where the page came from after redirects.
    pub final_url: String,
    /// The media type, lowercased, without parameters (`text/html`).
    pub content_type: String,
    /// The body as text.
    pub text: String,
}

/// Why a fetch didn't return a page. Each says what happened in words a person can read.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("`{0}` isn't a web address")]
    InvalidUrl(String),
    #[error("{0}")]
    NotAllowed(String),
    #[error(transparent)]
    Blocked(BlockedHost),
    #[error("the page took longer than {0} seconds")]
    Timeout(u64),
    #[error("the page is larger than {} MB", .0 / (1024 * 1024))]
    TooLarge(usize),
    #[error("the page is {0}, which can't be read as text")]
    UnsupportedType(String),
    #[error("the page uses the {0} character set, which isn't supported")]
    UnsupportedCharset(String),
    #[error("the site answered {0}")]
    Status(u16),
    #[error("more than {0} redirects")]
    TooManyRedirects(usize),
    #[error("couldn't reach the site: {0}")]
    Network(String),
    #[error("the site's robots.txt doesn't allow fetching this page")]
    Robots,
    /// The page redirects to another site; that address wasn't fetched.
    #[error("the page moved to another site, {0}, which wasn't fetched")]
    RedirectedElsewhere(String),
    #[error("{0}")]
    Unreadable(String),
}

/// How long a site's robots.txt is kept.
pub const ROBOTS_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// The most of a robots.txt downloaded; [`crate::robots`] parses the first 500 KiB.
const ROBOTS_MAX_BYTES: usize = 512 * 1024;
/// The product token robots.txt groups are matched against.
pub const ROBOTS_AGENT: &str = "Demios";
const ROBOTS_TYPES: &[&str] = &["text/plain"];

/// A site's robots.txt, as it applies to Demios.
#[derive(Clone)]
enum Robots {
    AllowAll,
    DisallowAll,
    Rules(Arc<crate::robots::Robots>),
}

impl Robots {
    fn parse(text: &str) -> Self {
        Self::Rules(Arc::new(crate::robots::Robots::parse(text, ROBOTS_AGENT)))
    }

    fn allows(&self, url: &Url) -> bool {
        match self {
            Self::AllowAll => true,
            Self::DisallowAll => false,
            Self::Rules(r) => {
                let mut path = url.path().to_owned();
                if let Some(q) = url.query() {
                    path.push('?');
                    path.push_str(q);
                }
                r.allows(&path)
            }
        }
    }
}

/// Media types the fetcher accepts.
const TEXT_TYPES: &[&str] = &[
    "text/html",
    "application/xhtml+xml",
    "text/plain",
    "text/markdown",
];

/// Checks an address before it is requested or followed.
fn check_url(url: &Url, allow: &Allow) -> Result<(), FetchError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::NotAllowed(format!(
            "only http and https pages can be fetched, not {}:",
            url.scheme()
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::NotAllowed(
            "an address with a user name or password can't be fetched".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| FetchError::InvalidUrl(url.to_string()))?;
    if url.port().is_some() && !allow.port(url.port()) {
        return Err(FetchError::NotAllowed(
            "only the standard web ports can be fetched".into(),
        ));
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let literal = bare.parse::<std::net::IpAddr>().ok();
    if literal.is_none() {
        let d = host.trim_end_matches('.').to_ascii_lowercase();
        if d == "localhost" || d.ends_with(".localhost") {
            return Err(FetchError::Blocked(BlockedHost {
                host: d,
                reason: guard::Blocked("this computer (loopback)"),
            }));
        }
    }
    if let Some(ip) = literal
        && !allow.ip(ip)
        && let Err(reason) = guard::check(ip)
    {
        return Err(FetchError::Blocked(BlockedHost {
            host: ip.to_string(),
            reason,
        }));
    }
    Ok(())
}

/// What tests may reach that a real fetch may not: one loopback port, for a local server.
#[derive(Clone, Debug, Default)]
struct Allow {
    #[cfg(test)]
    loopback_port: Option<u16>,
}

impl Allow {
    #[cfg(test)]
    fn port(&self, port: Option<u16>) -> bool {
        self.loopback_port.is_some() && self.loopback_port == port
    }

    #[cfg(not(test))]
    fn port(&self, _port: Option<u16>) -> bool {
        false
    }

    #[cfg(test)]
    fn ip(&self, ip: std::net::IpAddr) -> bool {
        self.loopback_port.is_some() && ip.is_loopback()
    }

    #[cfg(not(test))]
    fn ip(&self, _ip: std::net::IpAddr) -> bool {
        false
    }
}

/// Fetches pages. One per process is enough: it holds the connection pool and the per-host
/// rate limits.
pub struct Fetcher {
    /// For pages: follows redirects within the same host only. A redirect to another host
    /// stops with [`FetchError::RedirectedElsewhere`], so the caller decides about that
    /// address on its own (a page's address can carry data out; see `dn_tools::web`).
    client: reqwest::Client,
    /// For robots.txt: follows redirects to other hosts too (RFC 9309 §2.3.1.2), still
    /// through the guard. Its address carries nothing.
    robots_client: reqwest::Client,
    config: FetcherConfig,
    rate: RateLimit,
    robots: tokio::sync::Mutex<std::collections::HashMap<String, (tokio::time::Instant, Robots)>>,
    allow: Allow,
}

impl Fetcher {
    /// A fetcher that resolves names with the system's resolver.
    pub fn new(config: FetcherConfig) -> Result<Self, FetchError> {
        Self::build(config, GuardResolver::system(), Allow::default())
    }

    fn build(
        config: FetcherConfig,
        resolver: GuardResolver,
        allow: Allow,
    ) -> Result<Self, FetchError> {
        let resolver = Arc::new(resolver);
        let client = |same_site: bool| {
            let max = config.max_redirects;
            let hop_allow = allow.clone();
            let policy = reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() > max {
                    return attempt.error(Redirect(FetchError::TooManyRedirects(max)));
                }
                if let Err(e) = check_url(attempt.url(), &hop_allow) {
                    return attempt.error(Redirect(e));
                }
                let first = attempt.previous().first().and_then(|u| u.host_str());
                if same_site && first != attempt.url().host_str() {
                    // Stopped, not followed: `get` reports where it pointed.
                    return attempt.stop();
                }
                attempt.follow()
            });
            reqwest::Client::builder()
                .user_agent(config.user_agent.clone())
                .timeout(config.timeout)
                .connect_timeout(config.connect_timeout)
                .redirect(policy)
                .dns_resolver(resolver.clone())
                .no_proxy()
                .build()
                .map_err(|e| FetchError::Network(e.to_string()))
        };
        Ok(Self {
            rate: RateLimit::new(config.per_host_interval, config.per_host_concurrent),
            robots: tokio::sync::Mutex::default(),
            client: client(true)?,
            robots_client: client(false)?,
            config,
            allow,
        })
    }

    /// Fetches `url` and returns its text, if the site's robots.txt allows it.
    pub async fn fetch(&self, url: &str) -> Result<Fetched, FetchError> {
        let parsed = Url::parse(url).map_err(|_| FetchError::InvalidUrl(url.to_owned()))?;
        check_url(&parsed, &self.allow)?;
        if !self.robots_allow(&parsed).await? {
            return Err(FetchError::Robots);
        }
        self.get(
            &self.client,
            url,
            &parsed,
            TEXT_TYPES,
            self.config.max_bytes,
        )
        .await
    }

    /// Fetches `url` and returns its text, then extracts its main text as markdown.
    pub async fn read(&self, url: &str) -> Result<crate::extract::Page, FetchError> {
        let page = self.fetch(url).await?;
        crate::extract::extract(&page).map_err(|e| FetchError::Unreadable(e.to_string()))
    }

    /// Whether the site's robots.txt lets Demios fetch `url` (RFC 9309). Kept for
    /// [`ROBOTS_TTL`] per site. A missing robots.txt (4xx) allows everything; one the site
    /// can't serve (5xx, unreachable, too slow) allows nothing until it can be read.
    async fn robots_allow(&self, url: &Url) -> Result<bool, FetchError> {
        let site = url.origin().ascii_serialization();
        if let Some(rules) = self
            .robots
            .lock()
            .await
            .get(&site)
            .and_then(|(at, r)| (at.elapsed() < ROBOTS_TTL).then(|| r.clone()))
        {
            return Ok(rules.allows(url));
        }
        let robots_url = format!("{site}/robots.txt");
        let parsed =
            Url::parse(&robots_url).map_err(|_| FetchError::InvalidUrl(robots_url.clone()))?;
        let rules = match self
            .get(
                &self.robots_client,
                &robots_url,
                &parsed,
                ROBOTS_TYPES,
                ROBOTS_MAX_BYTES,
            )
            .await
        {
            Ok(page) => Robots::parse(&page.text),
            Err(FetchError::Status(code)) if (400..500).contains(&code) => Robots::AllowAll,
            // Another type than text: no rules for crawlers in it.
            Err(FetchError::UnsupportedType(_)) => Robots::AllowAll,
            Err(FetchError::Blocked(b)) => return Err(FetchError::Blocked(b)),
            Err(_) => Robots::DisallowAll,
        };
        let allowed = rules.allows(url);
        self.robots
            .lock()
            .await
            .insert(site, (tokio::time::Instant::now(), rules));
        Ok(allowed)
    }

    /// One GET of `url` (already checked), for `types`, at most `max` bytes.
    async fn get(
        &self,
        client: &reqwest::Client,
        url: &str,
        parsed: &Url,
        types: &[&str],
        max: usize,
    ) -> Result<Fetched, FetchError> {
        let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
        let _turn = self.rate.turn(&host).await;
        let started = tokio::time::Instant::now();
        let mut response = client
            .get(parsed.clone())
            .send()
            .await
            .map_err(|e| self.error(e))?;
        let status = response.status();
        if status.is_redirection() {
            // The client stops on a redirect it won't follow (another host, another
            // scheme): say where it pointed, checked like any address.
            let to = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|l| response.url().join(l).ok());
            return Err(match to {
                Some(u) => match check_url(&u, &self.allow) {
                    Err(e) => e,
                    Ok(()) => FetchError::RedirectedElsewhere(u.to_string()),
                },
                None => FetchError::Status(status.as_u16()),
            });
        }
        if !status.is_success() {
            return Err(FetchError::Status(status.as_u16()));
        }
        let (media, charset) = media_type(response.headers());
        if !types.contains(&media.as_str()) {
            return Err(FetchError::UnsupportedType(if media.is_empty() {
                "of no stated type".into()
            } else {
                media
            }));
        }
        if response.content_length().is_some_and(|n| n > max as u64) {
            return Err(FetchError::TooLarge(max));
        }
        let final_url = response.url().to_string();
        let mut body = Vec::new();
        loop {
            let left = self.config.timeout.saturating_sub(started.elapsed());
            let chunk = tokio::time::timeout(left, response.chunk())
                .await
                .map_err(|_| FetchError::Timeout(self.config.timeout.as_secs()))?
                .map_err(|e| self.error(e))?;
            let Some(chunk) = chunk else { break };
            if body.len() + chunk.len() > max {
                return Err(FetchError::TooLarge(max));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Fetched {
            url: url.to_owned(),
            final_url,
            content_type: media,
            text: decode(&body, charset.as_deref())?,
        })
    }

    /// The reason behind a client error: a guard or redirect refusal first.
    fn error(&self, e: reqwest::Error) -> FetchError {
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&e);
        while let Some(s) = source {
            if let Some(b) = s.downcast_ref::<BlockedHost>() {
                return FetchError::Blocked(BlockedHost {
                    host: b.host.clone(),
                    reason: b.reason,
                });
            }
            if let Some(Redirect(r)) = s.downcast_ref::<Redirect>() {
                return match r {
                    FetchError::Blocked(b) => FetchError::Blocked(BlockedHost {
                        host: b.host.clone(),
                        reason: b.reason,
                    }),
                    FetchError::NotAllowed(m) => FetchError::NotAllowed(m.clone()),
                    FetchError::TooManyRedirects(n) => FetchError::TooManyRedirects(*n),
                    other => FetchError::Network(other.to_string()),
                };
            }
            source = s.source();
        }
        if e.is_timeout() {
            FetchError::Timeout(self.config.timeout.as_secs())
        } else {
            FetchError::Network(e.to_string())
        }
    }
}

/// A redirect the policy refused, carried through the client's error.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Redirect(FetchError);

/// The media type and charset from `Content-Type`, lowercased.
fn media_type(headers: &reqwest::header::HeaderMap) -> (String, Option<String>) {
    let Some(value) = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return (String::new(), None);
    };
    let mut parts = value.split(';');
    let media = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    let charset = parts.find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k.trim().eq_ignore_ascii_case("charset"))
            .then(|| v.trim().trim_matches('"').to_ascii_lowercase())
    });
    (media, charset)
}

/// Windows-1252's characters for 0x80–0x9F (the rest matches Latin-1).
const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
];

/// Decodes the body. UTF-8 (the default), US-ASCII, Latin-1 and Windows-1252 are read;
/// another declared character set is refused rather than read as the wrong text.
fn decode(body: &[u8], charset: Option<&str>) -> Result<String, FetchError> {
    match charset.unwrap_or("utf-8") {
        "utf-8" | "utf8" | "us-ascii" | "ascii" => Ok(String::from_utf8_lossy(body).into_owned()),
        "iso-8859-1" | "latin1" | "latin-1" | "windows-1252" | "cp1252" => Ok(body
            .iter()
            .map(|&b| match b {
                0x80..=0x9f => CP1252_HIGH[usize::from(b - 0x80)],
                _ => char::from(b),
            })
            .collect()),
        other => Err(FetchError::UnsupportedCharset(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A one-route-per-path HTTP server on 127.0.0.1 answering with canned responses.
    async fn server(routes: Vec<(&'static str, String)>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    let resp = routes
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map(|(_, r)| r.clone())
                        .unwrap_or_else(|| {
                            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into()
                        });
                    let resp = resp.replace("{UA}", &req);
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        port
    }

    fn ok(ctype: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn redirect(to: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    fn test_fetcher(port: u16, config: FetcherConfig) -> Fetcher {
        Fetcher::build(
            config,
            GuardResolver::system(),
            Allow {
                loopback_port: Some(port),
            },
        )
        .unwrap()
    }

    fn quick() -> FetcherConfig {
        FetcherConfig {
            per_host_interval: Duration::ZERO,
            timeout: Duration::from_secs(5),
            ..FetcherConfig::default()
        }
    }

    #[tokio::test]
    async fn a_page_is_fetched_with_an_honest_user_agent() {
        let port = server(vec![("/", ok("text/html; charset=utf-8", "<p>héllo</p>"))]).await;
        let f = test_fetcher(port, quick());
        let page = f.fetch(&format!("http://127.0.0.1:{port}/")).await.unwrap();
        assert_eq!(page.text, "<p>héllo</p>");
        assert_eq!(page.content_type, "text/html");
        assert!(FetcherConfig::default().user_agent.starts_with("Demios/"));
    }

    #[tokio::test]
    async fn non_public_addresses_and_odd_urls_are_refused_before_any_request() {
        let f = Fetcher::new(quick()).unwrap();
        for (url, want) in [
            ("http://127.0.0.1/", "loopback"),
            ("http://169.254.169.254/latest/meta-data/", "cloud metadata"),
            ("http://10.0.0.1/", "private-network"),
            ("http://[::1]/", "loopback"),
            ("http://[::ffff:192.168.1.1]/", "private-network"),
            ("http://localhost/", "loopback"),
            ("http://api.localhost./", "loopback"),
            ("http://100.100.100.200/", "carrier-grade"),
            // Other spellings of 127.0.0.1 and 169.254.169.254; the URL parser
            // normalises them to dotted form before the check.
            ("http://2130706433/", "loopback"),
            ("http://0x7f.1/", "loopback"),
            ("http://0177.0.0.1/", "loopback"),
            ("http://127.1/", "loopback"),
            ("http://0xa9fea9fe/", "cloud metadata"),
            ("http://[0:0:0:0:0:ffff:7f00:1]/", "loopback"),
            ("http://[fd00:ec2::254]/", "unique-local"),
            ("http://0.0.0.0/", "this network"),
            ("ftp://example.com/", "only http and https"),
            ("file:///etc/passwd", "only http and https"),
            ("http://user:pw@example.com/", "user name or password"),
            ("http://example.com:8080/", "standard web ports"),
        ] {
            let e = f.fetch(url).await.unwrap_err().to_string();
            assert!(e.contains(want), "{url}: {e}");
        }
        assert!(matches!(
            f.fetch("not a url").await,
            Err(FetchError::InvalidUrl(_))
        ));
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_a_private_address_is_refused() {
        let resolver = GuardResolver::with_lookup(Arc::new(|_| {
            Box::pin(async {
                let a: reqwest::dns::Addrs =
                    Box::new(vec!["192.168.1.1:0".parse().unwrap()].into_iter());
                Ok(a)
            })
        }));
        let f = Fetcher::build(quick(), resolver, Allow::default()).unwrap();
        let e = f.fetch("http://router.example/").await.unwrap_err();
        assert!(matches!(e, FetchError::Blocked(_)), "{e}");
        assert!(
            e.to_string()
                .contains("router.example is a private-network address")
        );
    }

    #[tokio::test]
    async fn a_redirect_to_a_private_address_or_another_scheme_is_refused() {
        let port = server(vec![
            (
                "/to-metadata",
                redirect("http://169.254.169.254/latest/meta-data/"),
            ),
            ("/to-local-name", redirect("http://localhost/admin")),
            ("/to-file", redirect("file:///etc/passwd")),
        ])
        .await;
        let f = test_fetcher(port, quick());
        let base = format!("http://127.0.0.1:{port}");
        let e = f.fetch(&format!("{base}/to-metadata")).await.unwrap_err();
        assert!(matches!(e, FetchError::Blocked(_)), "{e}");
        let e = f.fetch(&format!("{base}/to-local-name")).await.unwrap_err();
        assert!(matches!(e, FetchError::Blocked(_)), "{e}");
        let e = f.fetch(&format!("{base}/to-file")).await.unwrap_err();
        assert!(e.to_string().contains("only http and https"), "{e}");
    }

    #[tokio::test]
    async fn a_redirect_to_another_host_stops_and_names_the_address() {
        // A granted site with an open redirector mustn't carry data to another site
        // without that site's own check (security review of the web tools).
        let port = server(vec![(
            "/out",
            redirect("https://attacker.example/?d=secret"),
        )])
        .await;
        let f = test_fetcher(port, quick());
        let e = f
            .fetch(&format!("http://127.0.0.1:{port}/out"))
            .await
            .unwrap_err();
        match e {
            FetchError::RedirectedElsewhere(to) => {
                assert_eq!(to, "https://attacker.example/?d=secret")
            }
            other => panic!("{other}"),
        }
    }

    #[tokio::test]
    async fn redirects_are_followed_up_to_the_limit() {
        let port = server(vec![
            ("/a", redirect("/b")),
            ("/b", redirect("/c")),
            ("/c", ok("text/plain", "end")),
            ("/loop", redirect("/loop")),
        ])
        .await;
        let f = test_fetcher(port, quick());
        let base = format!("http://127.0.0.1:{port}");
        let page = f.fetch(&format!("{base}/a")).await.unwrap();
        assert_eq!(page.text, "end");
        assert_eq!(page.final_url, format!("{base}/c"));
        let e = f.fetch(&format!("{base}/loop")).await.unwrap_err();
        assert!(matches!(e, FetchError::TooManyRedirects(5)), "{e}");
    }

    #[tokio::test]
    async fn a_large_page_or_a_binary_type_is_refused() {
        let big = "x".repeat(2048);
        let port = server(vec![
            ("/big", ok("text/html", &big)),
            (
                "/big-no-length",
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{big}"
                ),
            ),
            ("/pdf", ok("application/pdf", "%PDF")),
            (
                "/none",
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi".into(),
            ),
            (
                "/gone",
                "HTTP/1.1 410 Gone\r\nContent-Length: 0\r\n\r\n".into(),
            ),
        ])
        .await;
        let f = test_fetcher(
            port,
            FetcherConfig {
                max_bytes: 1024,
                ..quick()
            },
        );
        let base = format!("http://127.0.0.1:{port}");
        for path in ["/big", "/big-no-length"] {
            let e = f.fetch(&format!("{base}{path}")).await.unwrap_err();
            assert!(matches!(e, FetchError::TooLarge(1024)), "{path}: {e}");
        }
        let e = f.fetch(&format!("{base}/pdf")).await.unwrap_err();
        assert_eq!(
            e.to_string(),
            "the page is application/pdf, which can't be read as text"
        );
        assert!(matches!(
            f.fetch(&format!("{base}/none")).await,
            Err(FetchError::UnsupportedType(_))
        ));
        assert!(matches!(
            f.fetch(&format!("{base}/gone")).await,
            Err(FetchError::Status(410))
        ));
    }

    #[tokio::test]
    async fn a_slow_page_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if String::from_utf8_lossy(&buf[..n]).contains("/robots.txt") {
                        let _ = sock
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                            .await;
                        return;
                    }
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 100\r\n\r\nslow",
                        )
                        .await;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                });
            }
        });
        let f = test_fetcher(
            port,
            FetcherConfig {
                timeout: Duration::from_millis(300),
                ..quick()
            },
        );
        let e = f
            .fetch(&format!("http://127.0.0.1:{port}/"))
            .await
            .unwrap_err();
        assert!(matches!(e, FetchError::Timeout(_)), "{e}");
    }

    #[tokio::test]
    async fn robots_txt_is_honoured_and_kept() {
        let port = server(vec![
            (
                "/robots.txt",
                ok(
                    "text/plain",
                    "User-agent: *\nDisallow: /private\n\nUser-agent: Demios\nDisallow: /no-demios\n",
                ),
            ),
            ("/ok", ok("text/plain", "fine")),
            ("/no-demios/x", ok("text/plain", "secret")),
            ("/private/x", ok("text/plain", "everyone else")),
        ])
        .await;
        let f = test_fetcher(port, quick());
        let base = format!("http://127.0.0.1:{port}");
        assert_eq!(f.fetch(&format!("{base}/ok")).await.unwrap().text, "fine");
        let e = f.fetch(&format!("{base}/no-demios/x")).await.unwrap_err();
        assert!(matches!(e, FetchError::Robots), "{e}");
        // The Demios group applies, not `*`.
        assert_eq!(
            f.fetch(&format!("{base}/private/x")).await.unwrap().text,
            "everyone else"
        );
    }

    #[tokio::test]
    async fn a_missing_robots_txt_allows_and_a_broken_one_disallows() {
        let missing = server(vec![("/x", ok("text/plain", "hi"))]).await;
        let f = test_fetcher(missing, quick());
        assert!(
            f.fetch(&format!("http://127.0.0.1:{missing}/x"))
                .await
                .is_ok()
        );

        let broken = server(vec![
            (
                "/robots.txt",
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n".into(),
            ),
            ("/x", ok("text/plain", "hi")),
        ])
        .await;
        let f = test_fetcher(broken, quick());
        let e = f
            .fetch(&format!("http://127.0.0.1:{broken}/x"))
            .await
            .unwrap_err();
        assert!(matches!(e, FetchError::Robots), "{e}");
    }

    #[tokio::test]
    async fn read_returns_the_main_text_as_markdown() {
        let html = "<html><head><title>T</title></head><body><article><p>A long enough \
                    paragraph about tides and moorings, with enough words in it that the \
                    readability pass keeps it as the article's text and not as boilerplate.</p>\
                    </article></body></html>";
        let port = server(vec![("/a", ok("text/html", html))]).await;
        let f = test_fetcher(port, quick());
        let page = f.read(&format!("http://127.0.0.1:{port}/a")).await.unwrap();
        assert!(
            page.markdown.contains("tides and moorings"),
            "{}",
            page.markdown
        );
    }

    #[test]
    fn character_sets() {
        assert_eq!(decode(b"caf\xe9", Some("iso-8859-1")).unwrap(), "café");
        assert_eq!(
            decode(b"\x93hi\x94 \x80", Some("windows-1252")).unwrap(),
            "“hi” €"
        );
        assert_eq!(decode("ü".as_bytes(), None).unwrap(), "ü");
        assert!(matches!(
            decode(b"x", Some("shift_jis")),
            Err(FetchError::UnsupportedCharset(_))
        ));
    }
}
