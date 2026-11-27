//! robots.txt, as RFC 9309 says (#369).
//!
//! - **Groups:** one or more `user-agent` lines, then their rules. A crawler follows the
//!   groups whose user-agent matches its product token, case-insensitively; with none,
//!   the `*` groups; with neither, nothing is disallowed. Groups for the same agent are
//!   combined.
//! - **Rules:** `allow` and `disallow` with a path pattern. The rule with the longest
//!   matching pattern wins; between an `allow` and a `disallow` of the same length, `allow`
//!   wins. An empty `disallow` disallows nothing.
//! - **Patterns:** `*` matches any run of characters and a trailing `$` anchors the end;
//!   everything else matches itself, case-sensitively, against the path and query.
//! - **Limits:** only the first [`MAX_BYTES`] are read. `/robots.txt` itself is always
//!   allowed. Other lines (`sitemap`, `crawl-delay`) are ignored.
//!
//! What to do when robots.txt can't be fetched (4xx: allow everything; 5xx or unreachable:
//! allow nothing) is the fetcher's part.

/// The most of a robots.txt parsed (RFC 9309 §2.5: at least 500 KiB).
pub const MAX_BYTES: usize = 500 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Rule {
    allow: bool,
    pattern: String,
}

/// The rules of one robots.txt that apply to one crawler.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Robots {
    rules: Vec<Rule>,
}

/// The token part of a `user-agent` value: up to the first character that can't be in a
/// product token.
fn agent_token(value: &str) -> &str {
    let end = value
        .find(|c: char| !(c.is_ascii_alphabetic() || c == '_' || c == '-' || c == '*'))
        .unwrap_or(value.len());
    &value[..end]
}

impl Robots {
    /// Parses `text` for the crawler with product token `agent`.
    pub fn parse(text: &str, agent: &str) -> Self {
        let text = if text.len() > MAX_BYTES {
            let mut end = MAX_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            &text[..end]
        } else {
            text
        };
        let mut mine = Vec::new();
        let mut any = Vec::new();
        let mut found_mine = false;
        // The agents of the group being read, and whether its rules have started.
        let mut agents: Vec<String> = Vec::new();
        let mut in_rules = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "user-agent" => {
                    if in_rules {
                        agents.clear();
                        in_rules = false;
                    }
                    let token = agent_token(value).to_ascii_lowercase();
                    if token == agent.to_ascii_lowercase() {
                        found_mine = true;
                    }
                    agents.push(token);
                }
                "allow" | "disallow" => {
                    in_rules = true;
                    if agents.is_empty() || value.is_empty() {
                        // A rule outside a group, or an empty disallow: no rule.
                        continue;
                    }
                    let rule = Rule {
                        allow: key == "allow",
                        pattern: encode(value),
                    };
                    if agents.iter().any(|a| a == &agent.to_ascii_lowercase()) {
                        mine.push(rule.clone());
                    }
                    if agents.iter().any(|a| a == "*") {
                        any.push(rule);
                    }
                }
                _ => {}
            }
        }
        Self {
            rules: if found_mine { mine } else { any },
        }
    }

    /// Whether the crawler may fetch `path` (path and query, starting with `/`).
    pub fn allows(&self, path: &str) -> bool {
        if path == "/robots.txt" {
            return true;
        }
        let path = encode(path);
        let mut best: Option<(usize, bool)> = None;
        for r in &self.rules {
            if matches(&r.pattern, &path) {
                let len = r.pattern.len();
                best = match best {
                    Some((l, a)) if l > len || (l == len && a) => Some((l, a)),
                    _ => Some((len, r.allow)),
                };
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }
}

/// Percent-encodes characters outside printable ASCII, so a rule written with raw UTF-8
/// matches a path the URL parser encoded, and the other way round (RFC 9309 §2.2.2).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_graphic() {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

/// Whether `pattern` (`*` for any run, a trailing `$` for the end) matches the start of
/// `path`.
fn matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let p = pattern.as_bytes();
    let s = path.as_bytes();
    // Positions in `s` the pattern so far can end at; grows by at most |s| each step.
    let mut at = vec![false; s.len() + 1];
    at[0] = true;
    for &c in p {
        let mut next = vec![false; s.len() + 1];
        if c == b'*' {
            let mut reach = false;
            for i in 0..=s.len() {
                reach |= at[i];
                next[i] = reach;
            }
        } else {
            for i in 0..s.len() {
                if at[i] && s[i] == c {
                    next[i + 1] = true;
                }
            }
        }
        at = next;
        if !at.contains(&true) {
            return false;
        }
    }
    if anchored {
        at[s.len()]
    } else {
        at.contains(&true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example in RFC 9309 §5.1.
    const RFC_EXAMPLE: &str = "\
User-Agent: *
Disallow: *.gif$
Disallow: /example/
Allow: /publications/

User-Agent: foobot
Disallow:/
Allow:/example/page.html
Allow:/example/allowed.gif

User-Agent: barbot
User-Agent: bazbot
Disallow: /example/page.html

User-Agent: quxbot

EOF
";

    #[test]
    fn the_rfc_example() {
        let any = Robots::parse(RFC_EXAMPLE, "otherbot");
        assert!(!any.allows("/image.gif"));
        assert!(!any.allows("/example/x"));
        assert!(any.allows("/publications/x"));
        assert!(any.allows("/image.gif?size=2"), "`$` anchors the end");

        let foo = Robots::parse(RFC_EXAMPLE, "FooBot");
        assert!(!foo.allows("/"));
        assert!(!foo.allows("/x"));
        assert!(foo.allows("/example/page.html"));
        assert!(foo.allows("/example/allowed.gif"));

        for bot in ["barbot", "bazbot"] {
            let r = Robots::parse(RFC_EXAMPLE, bot);
            assert!(!r.allows("/example/page.html"));
            assert!(
                r.allows("/example/other.html"),
                "only its own group applies"
            );
            assert!(r.allows("/image.gif"), "the `*` group doesn't apply");
        }

        // A group with no rules allows everything; it doesn't fall back to `*`.
        let qux = Robots::parse(RFC_EXAMPLE, "quxbot");
        assert!(qux.allows("/example/x"));
    }

    /// RFC 9309 §2.2.2: the longest match wins; an equal allow wins.
    #[test]
    fn the_longest_match_wins_and_allow_wins_a_tie() {
        let r = Robots::parse("User-agent: *\nAllow: /p\nDisallow: /\n", "x");
        assert!(r.allows("/page"));
        assert!(!r.allows("/other"));
        let r = Robots::parse("User-agent: *\nAllow: /folder\nDisallow: /folder\n", "x");
        assert!(r.allows("/folder/page"));
        let r = Robots::parse("User-agent: *\nAllow: /page\nDisallow: /*.htm\n", "x");
        assert!(!r.allows("/page.htm"));
        let r = Robots::parse("User-agent: *\nAllow: /$\nDisallow: /\n", "x");
        assert!(r.allows("/"));
        assert!(!r.allows("/page.htm"));
    }

    /// RFC 9309 §2.2.3: `*` and `$`; matching is case-sensitive.
    #[test]
    fn wildcards() {
        let cases: &[(&str, &[&str], &[&str])] = &[
            (
                "/fish",
                &[
                    "/fish",
                    "/fish.html",
                    "/fish/salmon.html",
                    "/fishheads",
                    "/fish.php?id=x",
                ],
                &["/Fish.asp", "/catfish", "/?id=fish", "/desert/fish"],
            ),
            (
                "/fish*",
                &["/fish", "/fishheads/yummy.html"],
                &["/Fish.asp", "/catfish"],
            ),
            (
                "/fish/",
                &["/fish/", "/fish/?id=x", "/fish/salmon.htm"],
                &["/fish", "/fish.html", "/animals/fish/", "/Fish/Salmon.asp"],
            ),
            (
                "/*.php",
                &[
                    "/index.php",
                    "/folder/filename.php?parameters",
                    "/folder/any.php.file.html",
                    "/filename.php/",
                ],
                &["/", "/windows.PHP"],
            ),
            (
                "/*.php$",
                &["/filename.php", "/folder/filename.php"],
                &[
                    "/filename.php?parameters",
                    "/filename.php/",
                    "/filename.php5",
                    "/windows.PHP",
                ],
            ),
            (
                "/fish*.php",
                &["/fish.php", "/fishheads/catfish.php?parameters"],
                &["/Fish.PHP"],
            ),
        ];
        for (pattern, yes, no) in cases {
            let r = Robots::parse(&format!("User-agent: *\nDisallow: {pattern}\n"), "x");
            for p in *yes {
                assert!(!r.allows(p), "{pattern} should match {p}");
            }
            for p in *no {
                assert!(r.allows(p), "{pattern} shouldn't match {p}");
            }
        }
    }

    #[test]
    fn agents_comments_and_odd_lines() {
        let text = "# hello\nUSER-AGENT: Demios/1.0 (+https://x)\nDisallow: /a # not /b\n\
                    sitemap: https://x/s.xml\ncrawl-delay: 5\nnonsense line\n\
                    User-agent: *\nDisallow: /\n";
        let r = Robots::parse(text, "Demios");
        assert!(!r.allows("/a"));
        assert!(r.allows("/b"), "the comment isn't part of the rule");
        assert!(r.allows("/"), "Demios' own group applies, not `*`");
        // Two groups for the same agent are combined.
        let r = Robots::parse(
            "User-agent: demios\nDisallow: /a\n\nUser-agent: x\nDisallow: /\n\nUser-agent: Demios\nDisallow: /b\n",
            "Demios",
        );
        assert!(!r.allows("/a") && !r.allows("/b") && r.allows("/c"));
        // Rules before any user-agent line, and an empty disallow, are no rules.
        let r = Robots::parse("Disallow: /\nUser-agent: *\nDisallow:\n", "x");
        assert!(r.allows("/anything"));
        // No rules at all.
        assert!(Robots::parse("", "x").allows("/"));
        // robots.txt itself is always allowed.
        assert!(Robots::parse("User-agent: *\nDisallow: /\n", "x").allows("/robots.txt"));
    }

    #[test]
    fn non_ascii_rules_match_the_encoded_path() {
        let r = Robots::parse("User-agent: *\nDisallow: /fööd\n", "x");
        assert!(!r.allows("/f%C3%B6%C3%B6d/x"));
        assert!(r.allows("/food"));
    }

    #[test]
    fn only_the_first_500_kib_are_read() {
        let mut text = String::from("User-agent: *\n");
        while text.len() < MAX_BYTES + 10 {
            text.push_str("# padding padding padding padding padding padding padding\n");
        }
        text.push_str("Disallow: /late\n");
        assert!(Robots::parse(&text, "x").allows("/late"));
    }
}
