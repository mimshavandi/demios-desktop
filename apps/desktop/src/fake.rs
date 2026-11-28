//! T-SHELL-03: a deterministic [`ChatModel`] so the shell is usable before
//! `dn-inference` / `dn-providers` merge. Registered only under `cfg(fake_model)` (feature `dev-fake-model` + debug build, see
//! build.rs); always compiled for tests.

use dn_core::chat::{
    ChatModel, ChatRequest, ContentPart, EventStream, FinishReason, Role, StreamEvent, ToolCall,
    Usage,
};
use std::collections::VecDeque;
use std::future::Future as _;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// Model ref the fake answers to.
pub const FAKE_MODEL_REF: &str = "local:dev-fake";

/// Streams a canned markdown reply that quotes the last user message, word by word.
#[derive(Clone, Debug)]
pub struct FakeChatModel {
    /// Pause between chunks; zero in tests.
    pub delay: Duration,
}

impl Default for FakeChatModel {
    /// 18 ms between chunks; `DEMIOS_FAKE_DELAY_MS` overrides it (the e2e suite slows the
    /// stream down so it can press Stop mid-answer).
    fn default() -> Self {
        let ms = std::env::var("DEMIOS_FAKE_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(18)
            .min(2_000);
        Self {
            delay: Duration::from_millis(ms),
        }
    }
}

/// First line of the HTML artifact the fake makes ("make an HTML artifact"), then the
/// update it applies ("update the artifact"). The e2e suite checks for these strings.
pub const FAKE_ARTIFACT_V1: &str = "Version one";
pub const FAKE_ARTIFACT_V2: &str = "Version two";

/// "make a probe artifact": artifact code that tries to reach the app from inside the frame
/// and reports what happened as one console error line (`dn-probe {json}`), which the pane shows
/// in its issue list. The e2e-windows isolation spec reads it there (the frame is in the
/// artifact webview, out of WebDriver's reach). Evidence only: the spec also checks from outside.
pub const FAKE_PROBE_ARTIFACT: &str = r##"<!doctype html>
<html><head><title>Probe</title></head><body><h1>Probe</h1>
<script>
(async () => {
  const out = {};
  const T = window.__TAURI_INTERNALS__;
  out.tauri = typeof T;
  out.label = T && T.metadata && T.metadata.currentWebview ? T.metadata.currentWebview.label : null;
  out.origin = self.origin;
  const call = async (cmd, args) => {
    if (!T || typeof T.invoke !== "function") return "no-invoke";
    const timeout = new Promise((r) => setTimeout(() => r("timeout"), 5000));
    try { return await Promise.race([T.invoke(cmd, args).then((v) => "ok:" + JSON.stringify(v).slice(0, 80)), timeout]); }
    catch (e) { return "err:" + String(e && e.message ? e.message : e).slice(0, 120); }
  };
  const attempt = async (f) => { try { return "ok:" + String(await f()).slice(0, 80); } catch (e) { return "blocked:" + (e && e.name ? e.name : String(e)); } };
  out.listConversations = await call("list_conversations", {});
  out.createConversation = await call("create_conversation", {});
  out.parentDocument = await attempt(() => parent.document.title);
  out.topDocument = await attempt(() => top.document.title);
  out.hostDoc = await attempt(() => fetch("http://artifact.localhost/host/doc?seq=1").then((r) => r.status));
  out.hostEvent = await attempt(() => fetch("http://artifact.localhost/host/event", { method: "POST", body: "{}" }).then((r) => r.status));
  out.ipcHttp = await attempt(() => fetch("http://ipc.localhost/create_conversation", { method: "POST", body: "{}" }).then((r) => r.status));
  console.error("dn-probe " + JSON.stringify(out));
})();
</script></body></html>
"##;

fn text_of(m: &dn_core::chat::ChatMessage) -> String {
    m.content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } => None,
        })
        .collect()
}

impl FakeChatModel {
    /// What the fake does for `req`: a tool call (artifacts, when the tool is offered) or
    /// the canned text reply.
    pub fn plan(req: &ChatRequest) -> (String, Option<ToolCall>) {
        let offered = |name: &str| req.tools.iter().any(|t| t.name == name);
        let last = req.messages.last();
        if let Some(m) = last.filter(|m| m.role == Role::Tool) {
            let ok = text_of(m).contains("\"ok\":true");
            return (
                if ok {
                    "Done. The artifact is open next to the chat.".into()
                } else {
                    "That didn't work; the tool reported an error.".into()
                },
                None,
            );
        }
        let user = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(text_of)
            .unwrap_or_default()
            .to_lowercase();
        if user.contains("update the artifact") && offered("artifact_update") {
            // The id comes from the latest successful artifact tool result in the history.
            let id = req.messages.iter().rev().find_map(|m| {
                (m.role == Role::Tool)
                    .then(|| serde_json::from_str::<serde_json::Value>(&text_of(m)).ok())
                    .flatten()
                    .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(String::from))
            });
            if let Some(id) = id {
                return (
                    "Updating the artifact.".into(),
                    Some(ToolCall {
                        id: "fake_update".into(),
                        name: "artifact_update".into(),
                        arguments: serde_json::json!({
                            "id": id,
                            "old_str": FAKE_ARTIFACT_V1,
                            "new_str": FAKE_ARTIFACT_V2,
                        }),
                    }),
                );
            }
        }
        if user.contains("probe artifact") && offered("artifact_create") {
            return (
                "Making a probe artifact.".into(),
                Some(ToolCall {
                    id: "fake_probe".into(),
                    name: "artifact_create".into(),
                    arguments: serde_json::json!({
                        "type": "html",
                        "title": "Probe artifact",
                        "content": FAKE_PROBE_ARTIFACT,
                    }),
                }),
            );
        }
        if user.contains("html artifact") && offered("artifact_create") {
            let content = format!(
                "<!doctype html>\n<html><head><title>Fake artifact</title></head>\n<body>\n<h1 data-testid=\"fake-artifact-heading\">{FAKE_ARTIFACT_V1}</h1>\n<p>Made by the dev fake model.</p>\n</body></html>\n"
            );
            return (
                "Making an HTML artifact.".into(),
                Some(ToolCall {
                    id: "fake_create".into(),
                    name: "artifact_create".into(),
                    arguments: serde_json::json!({
                        "type": "html",
                        "title": "Fake artifact",
                        "content": content,
                    }),
                }),
            );
        }
        (Self::reply_for(req), None)
    }

    pub fn reply_for(req: &ChatRequest) -> String {
        let last_user = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(text_of)
            .unwrap_or_default();
        let quoted = last_user
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        let turn = req.messages.iter().filter(|m| m.role == Role::User).count();
        format!(
            "**Dev fake model** (turn {turn}). No real model is connected yet, so this reply is canned.\n\n\
             You said:\n\n{quoted}\n\n\
             A code block, to check highlighting:\n\n\
             ```rust\nfn main() {{\n    let answer = 6 * 7;\n    println!(\"the answer is {{answer}}\");\n}}\n```\n\n\
             - Pick a real model in **Settings → Models** once local inference or a provider key is available.\n\
             - *Stop* and *Regenerate* work on this reply too."
        )
    }
}

impl ChatModel for FakeChatModel {
    fn chat(&self, req: ChatRequest) -> EventStream {
        let (reply, tool_call) = Self::plan(&req);
        let mut chunks: VecDeque<String> = VecDeque::new();
        let mut cur = String::new();
        for ch in reply.chars() {
            cur.push(ch);
            if ch == ' ' || ch == '\n' {
                chunks.push_back(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            chunks.push_back(cur);
        }
        let input_tokens = req
            .messages
            .iter()
            .map(|m| m.content.len() as u32)
            .sum::<u32>();
        Box::pin(FakeStream {
            output_tokens: chunks.len() as u32,
            input_tokens,
            chunks,
            tool_call,
            delay: self.delay,
            sleep: None,
            done_reason: FinishReason::Stop,
            done: false,
        })
    }
}

struct FakeStream {
    chunks: VecDeque<String>,
    tool_call: Option<ToolCall>,
    delay: Duration,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    input_tokens: u32,
    output_tokens: u32,
    done_reason: FinishReason,
    done: bool,
}

impl futures_core::Stream for FakeStream {
    type Item = Result<StreamEvent, dn_core::CoreError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        if !self.delay.is_zero() {
            let delay = self.delay;
            let sleep = self
                .sleep
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(delay)));
            if sleep.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.sleep = None;
        }
        if self.chunks.is_empty()
            && let Some(call) = self.tool_call.take()
        {
            self.done_reason = FinishReason::ToolCalls;
            return Poll::Ready(Some(Ok(StreamEvent::ToolCall { call })));
        }
        Poll::Ready(Some(Ok(match self.chunks.pop_front() {
            Some(text) => StreamEvent::TextDelta { text },
            None => {
                self.done = true;
                StreamEvent::Done {
                    finish_reason: self.done_reason,
                    usage: Usage {
                        input_tokens: self.input_tokens,
                        output_tokens: self.output_tokens,
                        ..Default::default()
                    },
                }
            }
        })))
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use dn_core::chat::{ChatMessage, Reasoning};

    pub(crate) fn request(text: &str) -> ChatRequest {
        ChatRequest {
            model: FAKE_MODEL_REF.parse().unwrap(),
            messages: vec![ChatMessage::text(Role::User, text)],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            response_schema: None,
            reasoning: Reasoning::Off,
            caller: "test".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::request;
    use super::*;

    #[tokio::test]
    async fn streams_reply_then_done() {
        let model = FakeChatModel {
            delay: Duration::from_millis(1),
        };
        let mut s = model.chat(request("hello\nworld"));
        let mut text = String::new();
        let mut done = None;
        while let Some(ev) = std::future::poll_fn(|cx| s.as_mut().poll_next(cx)).await {
            match ev.unwrap() {
                StreamEvent::TextDelta { text: t } => text.push_str(&t),
                StreamEvent::Done { finish_reason, .. } => done = Some(finish_reason),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(done, Some(FinishReason::Stop));
        assert!(text.contains("> hello\n> world"), "{text}");
        assert!(text.contains("```rust"));
        assert_eq!(text, FakeChatModel::reply_for(&request("hello\nworld")));
    }
}
