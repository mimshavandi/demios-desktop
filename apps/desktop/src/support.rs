//! "Report a problem" and "Report this answer" (T-STORE-14, readiness audit S-21).
//!
//! The app opens a prefilled issue form in the system browser. The URL is built here from a
//! fixed base, never from webview input, and carries only the app version, the OS and (for
//! an answer) the model ref. No chat content is included: the user adds what they choose
//! in the form. The webview gets no opener permission; only this command opens it.

use serde::Deserialize;

/// Where reports go. The owner may switch this to a support address (T-STORE-14 notes);
/// keep it a single constant.
pub const ISSUES_NEW_URL: &str = "https://github.com/mimshavandi/demios-desktop/issues/new";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    /// Something in the app went wrong.
    Problem,
    /// A model answer was harmful, offensive or otherwise worth reporting.
    Answer,
}

/// Percent-encodes a query value (RFC 3986 unreserved characters stay).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The issue-form URL for a report. `model` is only used for [`ReportKind::Answer`] and is
/// dropped unless it looks like a model ref (`local:…`, `byok:…`, `dev:…`), so free text
/// can't be smuggled into the form.
pub fn report_url(kind: ReportKind, version: &str, os: &str, model: Option<&str>) -> String {
    let template = match kind {
        ReportKind::Problem => "problem.yml",
        ReportKind::Answer => "answer-report.yml",
    };
    let mut url = format!(
        "{ISSUES_NEW_URL}?template={template}&version={}&os={}",
        encode(version),
        encode(os)
    );
    if kind == ReportKind::Answer
        && let Some(m) = model.filter(|m| is_model_ref(m))
    {
        url.push_str("&model=");
        url.push_str(&encode(m));
    }
    url
}

fn is_model_ref(m: &str) -> bool {
    m.len() <= 200
        && ["local:", "byok:", "dev:"].iter().any(|p| m.starts_with(p))
        && m.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/@+".contains(c))
}

/// "macOS 15.4 (aarch64)" style text from what the standard library knows.
pub fn os_label() -> String {
    let name = match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    format!("{name} ({})", std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_url_has_template_version_and_os_only() {
        let u = report_url(
            ReportKind::Problem,
            "0.1.0",
            "macOS (aarch64)",
            Some("local:x"),
        );
        assert_eq!(
            u,
            "https://github.com/mimshavandi/demios-desktop/issues/new?template=problem.yml&version=0.1.0&os=macOS%20%28aarch64%29"
        );
    }

    #[test]
    fn answer_url_carries_the_model_ref() {
        let u = report_url(
            ReportKind::Answer,
            "0.1.0",
            "Linux (x86_64)",
            Some("byok:openai:gpt-5"),
        );
        assert!(u.contains("template=answer-report.yml"));
        assert!(u.ends_with("&model=byok%3Aopenai%3Agpt-5"));
    }

    #[test]
    fn free_text_never_reaches_the_url() {
        for bad in [
            "hello there, my secret chat",
            "local:x&body=leak",
            "javascript:alert(1)",
        ] {
            let u = report_url(ReportKind::Answer, "0.1.0", "Linux", Some(bad));
            assert!(!u.contains("model="), "{bad}: {u}");
            assert!(!u.contains("leak") && !u.contains("secret"));
        }
        assert!(report_url(ReportKind::Answer, "1", "x", None).starts_with(ISSUES_NEW_URL));
    }
}
