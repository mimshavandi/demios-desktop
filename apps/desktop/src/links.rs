//! Links in answers open in the system browser (T-SHELL-09, issue #50).
//!
//! The webview can only *ask*: `open_link` checks the URL here (http/https only, no
//! credentials, a host, a sane length), then shows a native confirmation from Rust with the
//! full normalised URL, and only on "Open" hands it to the system browser. Model output is
//! untrusted, so the user always sees exactly where a link goes, outside the page that
//! showed it. The webview has no opener permission of its own.

use url::Url;

/// Longest link we offer to open (longer ones are almost always tracking or data blobs).
pub const MAX_LINK_LEN: usize = 2048;

/// Why a link can't be opened, in the user's words.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused(pub &'static str);

/// Checks a link from an answer. Returns the normalised URL the confirmation shows (hosts
/// in punycode, so look-alike characters are visible).
pub fn check_link(raw: &str) -> Result<Url, Refused> {
    let raw = raw.trim();
    if raw.len() > MAX_LINK_LEN {
        return Err(Refused("This link is too long to open safely."));
    }
    // The URL parser silently drops tabs and newlines; direction marks can reorder what the
    // dialog shows. A link with either isn't what it looks like.
    if raw.chars().any(|c| c.is_control() || is_bidi_control(c)) {
        return Err(Refused(
            "This link has hidden characters in it, so Demios won't open it.",
        ));
    }
    let u = Url::parse(raw).map_err(|_| Refused("This isn't a web address Demios can open."))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(Refused(
            "Only web links (http and https) open from answers.",
        ));
    }
    if u.host_str().is_none_or(str::is_empty) {
        return Err(Refused("This isn't a web address Demios can open."));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(Refused(
            "This link has a user name or password in it, so Demios won't open it.",
        ));
    }
    Ok(u)
}

/// Characters that change the display order of text (RTL/LTR overrides, isolates, marks).
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Longest URL shown in full; longer ones keep their start and end with "…" between.
pub const SHOWN_URL_LEN: usize = 200;

/// The URL as the dialog shows it: long ones shortened in the middle (the site is shown on
/// its own line above, so a long path can't push it out of view or imitate it).
pub fn shown_url(u: &Url) -> String {
    let s = u.as_str();
    let n = s.chars().count();
    if n <= SHOWN_URL_LEN {
        return s.to_string();
    }
    let head: String = s.chars().take(120).collect();
    let tail: String = s.chars().skip(n - 60).collect();
    format!("{head}…{tail}")
}

fn is_loopback(u: &Url) -> bool {
    match u.host() {
        Some(url::Host::Domain(d)) => {
            let d = d.to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unspecified(),
        None => false,
    }
}

/// The confirmation's text: the site on its own line, then the URL (shortened when very
/// long), with notes for plain http, encoded host names, links to this computer, and that the
/// link comes from an AI answer.
pub fn confirm_text(u: &Url) -> String {
    let site = match u.port() {
        Some(p) => format!("{}:{p}", u.host_str().unwrap_or("")),
        None => u.host_str().unwrap_or("").to_string(),
    };
    let mut t = format!(
        "Open this link in your browser?\n\nSite: {site}\n\n{}",
        shown_url(u)
    );
    if is_loopback(u) {
        t.push_str("\n\nThis link points to your own computer.");
    }
    if u.scheme() == "http" {
        t.push_str("\n\nThis address isn't encrypted (http).");
    }
    if u.host_str()
        .is_some_and(|h| h.split('.').any(|l| l.starts_with("xn--")))
    {
        t.push_str(
            "\n\nThe address uses non-Latin characters; it's shown in its encoded form so look-alike letters can't hide where it goes.",
        );
    }
    t.push_str("\n\nThe link comes from an AI answer. Open it only if you trust it.");
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_web_links_pass() {
        for ok in [
            "https://example.com",
            "https://example.com/a?b=c#d",
            "http://localhost:8080/x",
            "  https://example.com/trimmed  ",
        ] {
            assert!(check_link(ok).is_ok(), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            "JAVASCRIPT:alert(1)",
            "file:///etc/passwd",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox",
            "mailto:a@example.com",
            "demios://models/add?repo=x/y",
            "ms-settings:privacy",
            "smb://server/share",
            "/relative/path",
            "#anchor",
            "https://",
            "https://user:pw@example.com/",
            "https://user@example.com/",
            "",
        ] {
            assert!(check_link(bad).is_err(), "{bad:?}");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_LINK_LEN));
        assert!(check_link(&long).is_err());
    }

    #[test]
    fn hostile_forms_are_refused() {
        let at_limit = format!("https://example.com/{}", "a".repeat(MAX_LINK_LEN - 20));
        assert_eq!(at_limit.len(), MAX_LINK_LEN);
        assert!(check_link(&at_limit).is_ok(), "exactly the limit is fine");
        assert!(
            check_link(&format!("{at_limit}a")).is_err(),
            "one over is refused"
        );
        for bad in [
            "intent://scan/#Intent;scheme=zxing;end",
            "http://[fe80::1%25eth0]/",
            "https://:pw@example.com/",
            "https://example.com/\nhttps://evil.com",
            "https://exa\tmple.com/",
            "https://example.com/\u{202E}moc.elppa",
            "https://\u{2066}example.com\u{2069}/",
        ] {
            assert!(check_link(bad).is_err(), "{bad:?}");
        }
        // A %-encoded host is decoded, so the dialog shows the real one.
        let u = check_link("https://%65vil.com/").unwrap();
        assert_eq!(u.host_str(), Some("evil.com"));
        assert!(confirm_text(&u).contains("Site: evil.com"));
    }

    #[test]
    fn the_site_is_on_its_own_line_and_long_paths_are_shortened() {
        let tricky = format!("https://evil.com/https://apple.com/{}", "x".repeat(400));
        let u = check_link(&tricky).unwrap();
        let t = confirm_text(&u);
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines.contains(&"Site: evil.com"), "{t}");
        let shown = shown_url(&u);
        assert!(
            shown.chars().count() <= SHOWN_URL_LEN + 1,
            "{}",
            shown.len()
        );
        assert!(shown.starts_with("https://evil.com/https://apple.com/"));
        assert!(shown.contains('…'));
        assert!(
            confirm_text(&check_link("https://example.com:8443/x").unwrap())
                .contains("Site: example.com:8443")
        );
    }

    #[test]
    fn links_to_this_computer_say_so() {
        for local in [
            "http://localhost:3000/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://app.localhost/",
        ] {
            let t = confirm_text(&check_link(local).unwrap());
            assert!(t.contains("points to your own computer"), "{local}");
        }
        assert!(
            !confirm_text(&check_link("https://example.com/").unwrap()).contains("own computer")
        );
    }

    #[test]
    fn the_confirmation_shows_the_real_destination() {
        // A look-alike host is shown in punycode, not as the friendly text.
        let u = check_link("https://аpple.com/login").unwrap(); // Cyrillic "а"
        let t = confirm_text(&u);
        assert!(t.contains("https://xn--pple-43d.com/login"), "{t}");
        assert!(t.contains("encoded form"));
        // Backslashes and odd spacing can't disguise the host.
        let u = check_link("https://example.com\\@evil.com/").unwrap();
        assert_eq!(u.host_str(), Some("example.com"));
        let t = confirm_text(&check_link("http://example.com/").unwrap());
        assert!(t.contains("isn't encrypted"));
    }
}
