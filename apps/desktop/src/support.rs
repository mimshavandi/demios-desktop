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

/// The issue-form URL for a report. `model` is only used for [`ReportKind::Answer`]; the
/// caller reads it from the stored message, so the webview can't choose what goes into the
/// URL. Every value is percent-encoded.
pub fn report_url(
    kind: ReportKind,
    version: &str,
    os: &str,
    model: Option<&dn_core::model::ModelRef>,
) -> String {
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
        && let Some(m) = model
    {
        url.push_str("&model=");
        url.push_str(&encode(&m.to_string()));
    }
    url
}

/// The model ref of a stored assistant message, if it has a valid one.
pub fn answer_model(m: &dn_store::chat::StoredMessage) -> Option<dn_core::model::ModelRef> {
    if m.role != dn_core::chat::Role::Assistant {
        return None;
    }
    m.model.as_deref()?.parse().ok()
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
    use dn_core::model::ModelRef;

    fn model(s: &str) -> ModelRef {
        s.parse().unwrap()
    }

    #[test]
    fn problem_url_has_template_version_and_os_only() {
        let u = report_url(
            ReportKind::Problem,
            "0.1.0",
            "macOS (aarch64)",
            Some(&model("local:x")),
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
            Some(&model("byok:openai:gpt-5")),
        );
        assert!(u.contains("template=answer-report.yml"));
        assert!(u.ends_with("&model=byok%3Aopenai%3Agpt-5"));
    }

    #[test]
    fn odd_characters_in_a_model_id_stay_inside_the_value() {
        // A model id can hold anything; it must not end the value or start a fragment.
        let u = report_url(
            ReportKind::Answer,
            "0.1.0",
            "Linux",
            Some(&model("local:x%26body=leak#frag&title=y")),
        );
        let (_, value) = u.split_once("&model=").unwrap();
        assert_eq!(value, "local%3Ax%2526body%3Dleak%23frag%26title%3Dy");
        assert!(!u.contains('#'));
        assert_eq!(u.matches('&').count(), 3, "{u}");
    }

    #[test]
    fn only_assistant_messages_with_a_valid_model_count() {
        use dn_store::chat::{MessageStatus, StoredMessage};
        let msg = |role, model: Option<&str>| StoredMessage {
            id: dn_core::Id::new(),
            conversation_id: dn_core::Id::new(),
            role,
            content: vec![],
            tool_calls: vec![],
            tool_call_id: None,
            model: model.map(str::to_owned),
            status: MessageStatus::Complete,
            finish_reason: None,
            error: None,
            created_at: 0,
            updated_at: 0,
        };
        use dn_core::chat::Role;
        assert_eq!(
            answer_model(&msg(Role::Assistant, Some("local:a"))),
            Some(model("local:a"))
        );
        assert_eq!(answer_model(&msg(Role::User, Some("local:a"))), None);
        assert_eq!(answer_model(&msg(Role::Assistant, Some("free text"))), None);
        assert_eq!(answer_model(&msg(Role::Assistant, None)), None);
    }
}
