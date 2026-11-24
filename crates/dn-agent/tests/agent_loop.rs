//! Agent loop tests with a scripted fake `ChatModel` and fake tools.

use dn_agent::{
    AgentEvent, BoxFuture, CancelToken, LoopConfig, StopReason, ToolContext, ToolExecutor,
    ToolOutput, run_loop,
};
use dn_core::CoreError;
use dn_core::chat::{
    ChatMessage, ChatModel, ChatRequest, ContentPart, EventStream, FinishReason, Role, StreamEvent,
    ToolCall, ToolSpec, Usage,
};
use futures_core::Stream;
use serde_json::json;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

/// Replays one scripted response per `chat` call and records every request.
#[derive(Default)]
struct ScriptedModel {
    script: Mutex<VecDeque<Vec<StreamEvent>>>,
    requests: Mutex<Vec<ChatRequest>>,
    /// After the scripted events, keep the stream open forever (to test cancellation).
    hang: bool,
}

impl ScriptedModel {
    fn new(script: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            script: Mutex::new(script.into()),
            ..Default::default()
        }
    }
    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

struct Replay(VecDeque<StreamEvent>, bool);

impl Stream for Replay {
    type Item = Result<StreamEvent, CoreError>;
    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.0.pop_front() {
            Some(e) => Poll::Ready(Some(Ok(e))),
            None if self.1 => Poll::Pending,
            None => Poll::Ready(None),
        }
    }
}

impl ChatModel for ScriptedModel {
    fn chat(&self, req: ChatRequest) -> EventStream {
        self.requests.lock().unwrap().push(req);
        let events = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .expect("model called more often than scripted");
        Box::pin(Replay(events.into(), self.hang))
    }
}

fn text(t: &str) -> StreamEvent {
    StreamEvent::TextDelta { text: t.into() }
}
fn call(id: &str, name: &str, args: serde_json::Value) -> StreamEvent {
    StreamEvent::ToolCall {
        call: ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args,
        },
    }
}
fn done(f: FinishReason, i: u32, o: u32) -> StreamEvent {
    StreamEvent::Done {
        finish_reason: f,
        usage: Usage {
            input_tokens: i,
            output_tokens: o,
        },
    }
}

/// Fake notes tools: `search` finds a path, `read` returns its contents.
#[derive(Default)]
struct NotesTools {
    calls: Mutex<Vec<ToolCall>>,
    /// Block `read` until cancelled (simulates waiting for approval).
    block_read: bool,
}

impl ToolExecutor for NotesTools {
    fn tools(&self) -> Vec<ToolSpec> {
        ["search", "read"]
            .into_iter()
            .map(|n| ToolSpec {
                name: n.into(),
                description: format!("{n} notes"),
                input_schema: json!({"type": "object"}),
            })
            .collect()
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput, CoreError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(call.clone());
            match call.name.as_str() {
                "search" => Ok(ToolOutput::text("/notes/groceries.md")),
                "read" if self.block_read => {
                    ctx.cancel.cancelled().await;
                    Err(CoreError::Cancelled)
                }
                "read" if call.arguments["path"] == "/notes/groceries.md" => {
                    Ok(ToolOutput::text("eggs, milk"))
                }
                "read" => Ok(ToolOutput::error("no such file")),
                other => Ok(ToolOutput::error(format!("unknown tool `{other}`"))),
            }
        })
    }
}

fn cfg() -> LoopConfig {
    LoopConfig::new(
        "assistant.default",
        "byok:openai:gpt-5-mini".parse().unwrap(),
    )
}

fn history() -> Vec<ChatMessage> {
    vec![
        ChatMessage::text(Role::System, "You help with notes."),
        ChatMessage::text(Role::User, "What's on my grocery list?"),
    ]
}

fn texts(m: &ChatMessage) -> String {
    m.content
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => text.as_str(),
            _ => "",
        })
        .collect()
}

#[tokio::test]
async fn runs_a_two_step_tool_chain() {
    let model = ScriptedModel::new(vec![
        vec![
            text("Searching."),
            call("c1", "search", json!({"q": "grocery"})),
            done(FinishReason::ToolCalls, 100, 10),
        ],
        vec![
            call("c2", "read", json!({"path": "/notes/groceries.md"})),
            done(FinishReason::ToolCalls, 130, 12),
        ],
        vec![
            text("You need "),
            text("eggs and milk."),
            done(FinishReason::Stop, 150, 8),
        ],
    ]);
    let tools = NotesTools::default();
    let mut events = Vec::new();
    let out = run_loop(
        &model,
        &tools,
        &cfg(),
        history(),
        &CancelToken::new(),
        &mut |e| events.push(e),
    )
    .await
    .unwrap();

    assert_eq!(out.stop, StopReason::Completed);
    assert_eq!(out.steps, 3);
    assert_eq!(out.final_text, "You need eggs and milk.");
    assert_eq!(
        out.usage,
        Usage {
            input_tokens: 380,
            output_tokens: 30
        }
    );

    // Tools ran in order with the model's arguments.
    let ran: Vec<String> = tools
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    assert_eq!(ran, ["search", "read"]);

    // Each model call saw the previous tool result.
    let reqs = model.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0].tools.len(), 2);
    assert_eq!(reqs[0].caller, "chat.reply");
    let step1 = &reqs[1].messages;
    assert_eq!(step1.len(), 4);
    assert_eq!(step1[2].tool_calls[0].id, "c1");
    assert_eq!(step1[3].role, Role::Tool);
    assert_eq!(step1[3].tool_call_id.as_deref(), Some("c1"));
    assert_eq!(texts(&step1[3]), "/notes/groceries.md");
    let step2 = &reqs[2].messages;
    assert_eq!(texts(step2.last().unwrap()), "eggs, milk");

    // New messages: assistant(c1), tool, assistant(c2), tool, assistant(final).
    let roles: Vec<Role> = out.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [
            Role::Assistant,
            Role::Tool,
            Role::Assistant,
            Role::Tool,
            Role::Assistant
        ]
    );

    // UI events are streamed in order.
    assert_eq!(events[0], AgentEvent::StepStarted { step: 0 });
    assert_eq!(
        events[1],
        AgentEvent::TextDelta {
            step: 0,
            text: "Searching.".into()
        }
    );
    assert!(events.contains(&AgentEvent::ToolCallFinished {
        step: 1,
        call_id: "c2".into(),
        output: ToolOutput::text("eggs, milk")
    }));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::StepFinished { .. }))
            .count(),
        3
    );
}

#[tokio::test]
async fn tool_errors_go_back_to_the_model() {
    let model = ScriptedModel::new(vec![
        vec![
            call("c1", "read", json!({"path": "/nope"})),
            done(FinishReason::ToolCalls, 1, 1),
        ],
        vec![
            text("That note doesn't exist."),
            done(FinishReason::Stop, 1, 1),
        ],
    ]);
    let out = run_loop(
        &model,
        &NotesTools::default(),
        &cfg(),
        history(),
        &CancelToken::new(),
        &mut |_| {},
    )
    .await
    .unwrap();
    assert_eq!(out.stop, StopReason::Completed);
    let tool_msg = &model.requests()[1].messages[3];
    assert_eq!(texts(tool_msg), "Error: no such file");
}

#[tokio::test]
async fn stops_at_the_step_limit_with_consistent_history() {
    let looping = || {
        vec![
            call("c", "search", json!({})),
            done(FinishReason::ToolCalls, 5, 5),
        ]
    };
    let model = ScriptedModel::new(vec![looping(), looping()]);
    let mut c = cfg();
    c.max_steps = 2;
    let out = run_loop(
        &model,
        &NotesTools::default(),
        &c,
        history(),
        &CancelToken::new(),
        &mut |_| {},
    )
    .await
    .unwrap();
    assert_eq!(out.stop, StopReason::StepLimit);
    assert_eq!(out.steps, 2);
    assert_eq!(model.requests().len(), 2);
    assert_eq!(out.usage.output_tokens, 10);
    // Last tool call still has its result.
    assert_eq!(out.messages.last().unwrap().role, Role::Tool);
}

#[tokio::test]
async fn cancel_while_waiting_for_a_tool_closes_the_turn() {
    let model = ScriptedModel::new(vec![vec![
        call("c1", "read", json!({"path": "/notes/groceries.md"})),
        call("c2", "search", json!({})),
        done(FinishReason::ToolCalls, 1, 1),
    ]]);
    let tools = NotesTools {
        block_read: true,
        ..Default::default()
    };
    let cancel = CancelToken::new();
    let canceller = {
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            c.cancel();
        })
    };
    let out = run_loop(&model, &tools, &cfg(), history(), &cancel, &mut |_| {})
        .await
        .unwrap();
    canceller.await.unwrap();
    assert_eq!(out.stop, StopReason::Cancelled);
    // Both calls have a result, so the next turn is valid for every provider.
    let tool_ids: Vec<_> = out
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.tool_call_id.clone().unwrap())
        .collect();
    assert_eq!(tool_ids, ["c1", "c2"]);
    assert_eq!(tools.calls.lock().unwrap().len(), 1, "c2 never ran");
}

#[tokio::test]
async fn cancel_mid_stream_keeps_partial_text() {
    let model = ScriptedModel {
        script: Mutex::new(vec![vec![text("Partial ans")]].into()),
        hang: true,
        ..Default::default()
    };
    let cancel = CancelToken::new();
    let c = cancel.clone();
    let seen = Arc::new(Mutex::new(0));
    let seen2 = seen.clone();
    let out = run_loop(
        &model,
        &NotesTools::default(),
        &cfg(),
        history(),
        &cancel,
        &mut move |e| {
            if matches!(e, AgentEvent::TextDelta { .. }) {
                *seen2.lock().unwrap() += 1;
                c.cancel();
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(out.stop, StopReason::Cancelled);
    assert_eq!(out.final_text, "Partial ans");
    assert_eq!(texts(&out.messages[0]), "Partial ans");
    assert_eq!(*seen.lock().unwrap(), 1);
}

#[tokio::test]
async fn model_errors_end_the_run() {
    struct Failing;
    impl ChatModel for Failing {
        fn chat(&self, _: ChatRequest) -> EventStream {
            struct E(bool);
            impl Stream for E {
                type Item = Result<StreamEvent, CoreError>;
                fn poll_next(
                    mut self: Pin<&mut Self>,
                    _: &mut Context<'_>,
                ) -> Poll<Option<Self::Item>> {
                    if self.0 {
                        return Poll::Ready(None);
                    }
                    self.0 = true;
                    Poll::Ready(Some(Err(CoreError::ModelUnavailable {
                        user_message: "down".into(),
                        developer_message: "test".into(),
                        recovery: vec![],
                    })))
                }
            }
            Box::pin(E(false))
        }
    }
    let err = run_loop(
        &Failing,
        &NotesTools::default(),
        &cfg(),
        history(),
        &CancelToken::new(),
        &mut |_| {},
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CoreError::ModelUnavailable { .. }));
}
