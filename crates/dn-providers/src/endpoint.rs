//! Rules for OpenAI-compatible server addresses (T-PRV-05) and the redirect policy every
//! provider client uses.
//!
//! - `http` is allowed only for this computer and the local network: loopback, private and
//!   link-local IPs, CGNAT/Tailscale addresses (`100.64.0.0/10`), single-label host names
//!   and `.local` / `.lan` / `.home.arpa` / `.internal` names. Any other host needs `https`.
//! - No user name or password in the URL (keys go in the keychain), no query or fragment.
//! - Redirects are followed only within the same origin (scheme, host and port), so a
//!   server can't send a request, or its key, to another host or downgrade to `http`.

use reqwest::Url;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Why a server address was refused, in words for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlRejected(pub String);

impl std::fmt::Display for UrlRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UrlRejected {}

/// Checks and normalizes an OpenAI-compatible server's base URL. A URL without a path gets
/// `/v1` (Ollama, LM Studio, llama.cpp and vLLM all serve the API there); a trailing `/` is
/// dropped.
pub fn check_server_url(raw: &str) -> Result<Url, UrlRejected> {
    let raw = raw.trim();
    let mut url = Url::parse(raw).map_err(|_| {
        UrlRejected("Enter the server's full address, for example http://localhost:11434.".into())
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlRejected(
            "The address must start with http:// or https://.".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlRejected(
            "Don't put a user name or password in the address. Add the key in its own field."
                .into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(UrlRejected(
            "The address can't contain ? or #. Enter only the server address.".into(),
        ));
    }
    let Some(host) = url.host_str().map(str::to_string) else {
        return Err(UrlRejected("The address has no host name.".into()));
    };
    if url.scheme() == "http" && !is_local_host(&host) {
        return Err(UrlRejected(format!(
            "{host} isn't on this computer or your local network, so it needs https://. Plain http is only allowed for local servers."
        )));
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(if path.is_empty() { "/v1" } else { &path });
    Ok(url)
}

/// Whether `host` (as in a URL, IPv6 in brackets) is this computer or the local network.
pub fn is_local_host(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    let bare = h.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return is_local_ip(ip);
    }
    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    // A single-label name (`nas`, `homeserver`) only resolves on the local network.
    if !h.contains('.') {
        return true;
    }
    [".local", ".lan", ".home.arpa", ".internal"]
        .iter()
        .any(|s| h.ends_with(s))
}

fn is_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_local_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_local_v4(v4);
            }
            let seg = v6.segments()[0];
            v6 == Ipv6Addr::LOCALHOST
                || (seg & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (seg & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

fn is_local_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        // CGNAT range, used by Tailscale and similar private networks.
        || (a == 100 && (64..128).contains(&b))
}

/// Redirects within the same origin only (at most 5). A redirect to another host, port or
/// scheme fails the request instead of following it.
pub fn same_origin_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let first = attempt.previous().first().map(Url::origin);
        if attempt.previous().len() > 5 {
            attempt.error("too many redirects")
        } else if first.as_ref() == Some(&attempt.url().origin()) {
            attempt.follow()
        } else {
            let to = attempt.url().origin().ascii_serialization();
            attempt.error(format!(
                "the server redirected to another address ({to}); enter that address instead"
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_http_is_allowed_and_gets_v1() {
        for (input, want) in [
            ("http://localhost:11434", "http://localhost:11434/v1"),
            ("http://127.0.0.1:1234/v1/", "http://127.0.0.1:1234/v1"),
            ("http://192.168.1.20:11434", "http://192.168.1.20:11434/v1"),
            ("http://10.0.0.5:8000/v1", "http://10.0.0.5:8000/v1"),
            ("http://172.20.0.2:8080", "http://172.20.0.2:8080/v1"),
            ("http://169.254.10.1", "http://169.254.10.1/v1"),
            (
                "http://100.101.102.103:11434",
                "http://100.101.102.103:11434/v1",
            ),
            ("http://[::1]:11434", "http://[::1]:11434/v1"),
            (
                "http://[fd12:3456::1]:11434",
                "http://[fd12:3456::1]:11434/v1",
            ),
            ("http://nas:11434", "http://nas:11434/v1"),
            ("http://studio.local:1234", "http://studio.local:1234/v1"),
            ("http://box.home.arpa/api/v1", "http://box.home.arpa/api/v1"),
            (
                "  https://api.example.com/v1  ",
                "https://api.example.com/v1",
            ),
            ("https://8.8.8.8", "https://8.8.8.8/v1"),
        ] {
            assert_eq!(check_server_url(input).unwrap().as_str(), want, "{input}");
        }
    }

    #[test]
    fn remote_http_and_odd_urls_are_refused() {
        for bad in [
            "http://api.example.com/v1",
            "http://8.8.8.8:11434",
            "http://100.128.0.1",
            "http://[2001:db8::1]:11434",
            "http://localhost.example.com",
            "ftp://localhost",
            "file:///etc/passwd",
            "localhost:11434",
            "https://user:pass@api.example.com/v1",
            "https://api.example.com/v1?key=x",
            "https://api.example.com/v1#x",
            "",
        ] {
            assert!(check_server_url(bad).is_err(), "{bad} should be refused");
        }
        let e = check_server_url("http://api.example.com").unwrap_err();
        assert!(e.0.contains("needs https://"), "{e}");
    }

    #[test]
    fn mapped_ipv4_follows_the_v4_rule() {
        assert!(is_local_host("[::ffff:192.168.0.2]"));
        assert!(!is_local_host("[::ffff:8.8.8.8]"));
    }
}
