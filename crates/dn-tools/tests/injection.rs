//! Prompt injection through what a reader tool returns can't make anything happen on its
//! own: no send, no web request and no approval (ADR-0013 §9.1, §9.4). A generic corpus of
//! injected messages, in the shape the Connect read tools return, run through a stand-in
//! reader and a model that obeys every instruction. Add-ons run their real reader through
//! the same harness, `dn_tools::injection::check`.

use dn_tools::injection::{Reader, check};
use serde_json::Value;
use std::sync::Arc;

fn corpus() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/injection/corpus.json")).unwrap()
}

/// Shows the model everything: headers and the full text, hidden parts included. The worst
/// case for injection.
fn stand_in(message: &Value) -> Result<String, String> {
    let field = |k: &str| message.get(k).and_then(Value::as_str).unwrap_or_default();
    if field("text").is_empty() && field("subject").is_empty() {
        return Err("empty message".into());
    }
    Ok(format!(
        "From: {}\nSubject: {}\nDate: {}\n\n{}",
        field("from"),
        field("subject"),
        field("date"),
        field("text")
    ))
}

#[tokio::test]
async fn injected_messages_cause_no_send_no_web_request_and_no_approval() {
    let corpus = corpus();
    assert!(corpus.len() >= 10);
    let reader: Arc<Reader> = Arc::new(stand_in);
    let report = check(reader, &corpus).await.unwrap();
    assert_eq!(report.messages, corpus.len());
    assert_eq!(report.refused, 0);
    // Three side-effect tools per message: from the reader's handler (refused), the chat
    // (a card nobody answers) and a job (held).
    assert_eq!(report.attempts, corpus.len() * 9);
    assert_eq!(report.cards, corpus.len() * 3);
    assert_eq!(report.held, corpus.len() * 3);
}

#[tokio::test]
async fn a_refused_message_is_counted_and_still_causes_nothing() {
    let reader: Arc<Reader> = Arc::new(|_| Err("not a message".into()));
    let report = check(reader, &corpus()[..2]).await.unwrap();
    assert_eq!(report.refused, 2);
}
