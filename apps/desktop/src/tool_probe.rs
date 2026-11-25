//! The one-time tool-call check for local chat models whose chat template doesn't say
//! whether they make tool calls (T-INF-26, tool support "unknown"). The user starts it; it
//! sends one tiny request with one tool to the local model, never to a provider, and the
//! answer is stored as "checked" in the app's data folder (`tool-probes.json`).

use dn_core::CoreError;
use dn_core::chat::{ChatMessage, ChatModel, ChatRequest, Reasoning, Role, StreamEvent, ToolSpec};
use dn_core::model::ModelRef;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const TOOL: &str = "get_current_time";
/// Long enough for a model to open and finish one tool call, even after a short preamble.
const MAX_TOKENS: u32 = 256;
/// A local model that hasn't answered by then counts as a failed check, not a "no".
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The request: one tool that needs no arguments and a question only it answers.
pub fn request(model: ModelRef) -> ChatRequest {
    ChatRequest {
        model,
        messages: vec![ChatMessage::text(
            Role::User,
            "What time is it right now? Use the get_current_time tool to find out.",
        )],
        tools: vec![ToolSpec {
            name: TOOL.into(),
            description: "Returns the current local time.".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        }],
        max_tokens: Some(MAX_TOKENS),
        temperature: Some(0.0),
        response_schema: None,
        reasoning: Reasoning::Off,
        caller: "tool_probe".into(),
    }
}

/// Whether `model` calls the tool when asked to. An error (or a timeout) is not an answer.
pub async fn probe(model: &dyn ChatModel, model_ref: ModelRef) -> Result<bool, CoreError> {
    let run = async {
        let mut s = model.chat(request(model_ref));
        let mut called = false;
        while let Some(ev) = dn_inference::next_event(&mut s).await {
            if let StreamEvent::ToolCall { call } = ev? {
                called |= call.name == TOOL;
            }
        }
        Ok(called)
    };
    tokio::time::timeout(TIMEOUT, run)
        .await
        .unwrap_or_else(|_| {
            Err(CoreError::ModelUnavailable {
                user_message: "The model didn't answer the tool check in time. Try again.".into(),
                developer_message: format!("tool probe timed out after {TIMEOUT:?}"),
                recovery: vec![],
            })
        })
}

/// Checked results by model id, kept in `tool-probes.json`.
#[derive(Debug, Default)]
pub struct Store {
    path: Option<PathBuf>,
    results: BTreeMap<String, bool>,
}

impl Store {
    /// The results stored at `path`; empty when it doesn't exist or can't be read.
    pub fn open(path: Option<PathBuf>) -> Self {
        let results = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, results }
    }

    pub fn get(&self, id: &str) -> Option<bool> {
        self.results.get(id).copied()
    }

    /// Records a result and writes the file (through a temporary file, so it's never torn).
    pub fn set(&mut self, id: &str, makes_tool_calls: bool) -> std::io::Result<()> {
        self.results.insert(id.to_string(), makes_tool_calls);
        self.write()
    }

    pub fn remove(&mut self, id: &str) -> std::io::Result<()> {
        if self.results.remove(id).is_some() {
            self.write()?;
        }
        Ok(())
    }

    pub fn clear(&mut self) -> std::io::Result<()> {
        self.results.clear();
        match &self.path {
            Some(p) => match std::fs::remove_file(p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
            None => Ok(()),
        }
    }

    fn write(&self) -> std::io::Result<()> {
        let Some(p) = &self.path else {
            return Ok(());
        };
        write_atomic(p, &serde_json::to_vec_pretty(&self.results)?)
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_core::chat::{EventStream, FinishReason, ToolCall, Usage};
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct Events(VecDeque<StreamEvent>);

    impl futures_core::Stream for Events {
        type Item = Result<StreamEvent, CoreError>;
        fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.0.pop_front().map(Ok))
        }
    }

    /// Answers with a tool call or with text, and records the request it got.
    struct Fake {
        calls_tool: bool,
        seen: std::sync::Mutex<Option<ChatRequest>>,
    }

    impl ChatModel for Fake {
        fn chat(&self, req: ChatRequest) -> EventStream {
            *self.seen.lock().unwrap() = Some(req);
            let mut ev = VecDeque::new();
            if self.calls_tool {
                ev.push_back(StreamEvent::ToolCall {
                    call: ToolCall {
                        id: "1".into(),
                        name: TOOL.into(),
                        arguments: serde_json::json!({}),
                    },
                });
            } else {
                ev.push_back(StreamEvent::TextDelta {
                    text: "I can't check the time.".into(),
                });
            }
            ev.push_back(StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                },
            });
            Box::pin(Events(ev))
        }
    }

    fn local() -> ModelRef {
        "local:phi-4-mini-instruct-q4_k_m".parse().unwrap()
    }

    #[tokio::test]
    async fn a_tool_call_means_supported() {
        for calls_tool in [true, false] {
            let m = Fake {
                calls_tool,
                seen: Default::default(),
            };
            assert_eq!(probe(&m, local()).await.unwrap(), calls_tool);
            let req = m.seen.lock().unwrap().take().unwrap();
            // One tiny request with one tool, greedy, thinking off.
            assert_eq!(req.tools.len(), 1);
            assert_eq!(req.max_tokens, Some(MAX_TOKENS));
            assert_eq!(req.temperature, Some(0.0));
            assert_eq!(req.reasoning, Reasoning::Off);
            assert_eq!(req.messages.len(), 1);
        }
    }

    /// The probe against a real GGUF through llama.cpp: prints the answer.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs DN_TEST_GGUF"]
    async fn probe_a_real_model() {
        let path = std::env::var("DN_TEST_GGUF").expect("set DN_TEST_GGUF");
        let m = dn_inference::LoadedModel::load(
            std::sync::Arc::new(dn_inference::LlamaCppBackend::new()),
            &dn_inference::ModelSpec::gguf(&path),
            &dn_inference::LoadOptions {
                n_ctx: Some(2048),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let calls = probe(&m, local()).await.unwrap();
        eprintln!("{path}: makes tool calls = {calls}");
    }

    #[test]
    fn results_persist() {
        let p =
            std::env::temp_dir().join(format!("dn-probes-{}/tool-probes.json", dn_core::Id::new()));
        let mut s = Store::open(Some(p.clone()));
        assert_eq!(s.get("m"), None);
        s.set("m", true).unwrap();
        s.set("n", false).unwrap();
        let s2 = Store::open(Some(p.clone()));
        assert_eq!((s2.get("m"), s2.get("n")), (Some(true), Some(false)));
        let mut s3 = Store::open(Some(p.clone()));
        s3.remove("m").unwrap();
        assert_eq!(Store::open(Some(p.clone())).get("m"), None);
        s3.clear().unwrap();
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
