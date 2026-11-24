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

/// The confirmation's text: the full URL on its own, plus a note for plain http and for
/// internationalised host names (shown in their punycode form).
pub fn confirm_text(u: &Url) -> String {
    let mut t = format!("Open this link in your browser?\n\n{}", u.as_str());
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
