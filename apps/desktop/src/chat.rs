//! Chat turns (T-SHELL-01, T-SHELL-04): write the user message and an assistant
//! placeholder, run the agent loop (`dn_agent::run_loop`: model → tools → model, with
//! approvals in dn-tools), stream progress to the UI, then store every message of the run
//! in order. Independent of Tauri so it can be tested with a plain event sink.

use crate::agent::model_error;
use crate::error::{IpcError, IpcResult};
use crate::state::AppState;
use dn_agent::{AgentEvent, CancelToken, LoopConfig, RunOutcome, StopReason};
use dn_core::chat::{ChatMessage, ChatModel, ContentPart, FinishReason, Role, Usage};
use dn_core::model::ModelRef;
use dn_core::{CoreError, Id, RecoveryAction};
use dn_store::chat::{self as store, MessageOutcome, MessageStatus, NewMessage, StoredMessage};
use serde::Serialize;
use std::sync::Arc;

/// Events streamed to the UI for one turn. `message_id` is the turn's assistant message
/// (the placeholder); tool messages of the run appear after `done` via `list_messages`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    TextDelta {
        message_id: Id,
        /// Length of the answer before this chunk, in UTF-16 code units (JS string
        /// length). Lets the UI drop chunks it already got from `list_messages`.
        offset: u32,
        text: String,
    },
    ReasoningDelta {
        message_id: Id,
        text: String,
    },
    /// The model asked for a tool; it may now wait for approval.
    ToolCallStarted {
        message_id: Id,
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolCallFinished {
        message_id: Id,
        call_id: String,
        is_error: bool,
        /// First part of the result text, for the collapsed card.
        summary: String,
    },
    Done {
        message_id: Id,
        finish_reason: FinishReason,
        usage: Usage,
        /// `step_limit` or `length`: the UI may offer to continue.
        stop: &'static str,
    },
    Error {
        message_id: Id,
        message: String,
        recovery: Vec<RecoveryAction>,
    },
}

/// Returned to the UI as soon as a turn starts; deltas follow on the channel.
#[derive(Clone, Debug, Serialize)]
pub struct TurnStarted {
    pub user_message: Option<StoredMessage>,
    pub assistant_message: StoredMessage,
    /// Set when the conversation title changed (first message).
    pub title: Option<String>,
}

pub trait EventSink: Send + Sync + 'static {
    fn send(&self, ev: ChatEvent);
}

impl<F: Fn(ChatEvent) + Send + Sync + 'static> EventSink for F {
    fn send(&self, ev: ChatEvent) {
        self(ev)
    }
}

const DEFAULT_TITLE: &str = "New chat";

fn title_from(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut t: String = line.trim().chars().take(48).collect();
    if line.trim().chars().count() > 48 {
        t.push('…');
    }
    if t.is_empty() {
        DEFAULT_TITLE.into()
    } else {
        t
    }
}

/// The model for the next turn and its backend. Blocking (settings, keychain).
fn model_for_turn(state: &AppState) -> IpcResult<(ModelRef, Arc<dyn ChatModel>)> {
    let settings = state.settings()?;
    let model = state
        .agent
        .resolve_model(&settings, &state.models, &state.keys)?;
    let backend = state.models.get(&model, &state.keys).map_err(model_error)?;
    Ok((model, backend))
}

/// Sends `text` as a new user message and streams the reply. Blocking (SQLite): call it
/// from a blocking thread. The generation slot is reserved before anything is written, so
/// a busy chat gets no orphan user message.
pub fn send(
    state: &Arc<AppState>,
    conversation_id: Id,
    text: String,
    sink: impl EventSink,
) -> IpcResult<TurnStarted> {
    let text = text.trim_end().to_string();
    if text.trim().is_empty() {
        return Err(IpcError::invalid("Message is empty."));
    }
    let (model, backend) = model_for_turn(state)?;
    let reservation = state.reserve(conversation_id)?;
    let (user, assistant, title, history) = {
        let db = state.db()?;
        let conv = store::get_conversation(&db, conversation_id)?;
        let title = (conv.title == DEFAULT_TITLE).then(|| title_from(&text));
        if let Some(t) = &title {
            store::rename_conversation(&db, conversation_id, t)?;
        }
        store::set_conversation_model(&db, conversation_id, &model.to_string())?;
        let user = store::add_message(&db, conversation_id, NewMessage::user_text(text))?;
        let assistant = store::add_message(
            &db,
            conversation_id,
            NewMessage::assistant_placeholder(Some(model.to_string())),
        )?;
        let history = history(state, &db, conversation_id, assistant.id)?;
        (user, assistant, title, history)
    };
    start(
        state,
        reservation,
        model,
        backend,
        &assistant,
        history,
        sink,
    );
    Ok(TurnStarted {
        user_message: Some(user),
        assistant_message: assistant,
        title,
    })
}

/// Drops the answer after the last user message and answers it again. Refused while an
/// answer is streaming, so the live answer is never truncated.
pub fn regenerate(
    state: &Arc<AppState>,
    conversation_id: Id,
    sink: impl EventSink,
) -> IpcResult<TurnStarted> {
    let (model, backend) = model_for_turn(state)?;
    let reservation = state.reserve(conversation_id)?;
    let (assistant, history) = {
        let db = state.db()?;
        let msgs = store::list_messages(&db, conversation_id)?;
        let last_user = msgs
            .iter()
            .rposition(|m| m.role == Role::User)
            .ok_or_else(|| IpcError::invalid("Nothing to regenerate."))?;
        if let Some(after) = msgs.get(last_user + 1) {
            store::truncate_from(&db, conversation_id, after.id)?;
        }
        store::set_conversation_model(&db, conversation_id, &model.to_string())?;
        let assistant = store::add_message(
            &db,
            conversation_id,
            NewMessage::assistant_placeholder(Some(model.to_string())),
        )?;
        let history = history(state, &db, conversation_id, assistant.id)?;
        (assistant, history)
    };
    start(
        state,
        reservation,
        model,
        backend,
        &assistant,
        history,
        sink,
    );
    Ok(TurnStarted {
        user_message: None,
        assistant_message: assistant,
        title: None,
    })
}

/// Stops the running answer in `conversation_id`, if any. Returns whether one was running.
pub fn stop(state: &AppState, conversation_id: Id) -> bool {
    state.stop(conversation_id)
}

/// System prompt + stored conversation, minus the new placeholder and failed empty turns.
fn history(
    state: &AppState,
    db: &dn_store::Connection,
    conversation_id: Id,
    exclude: Id,
) -> IpcResult<Vec<ChatMessage>> {
    let mut out = vec![];
    if let Some(p) = state.agent.system_prompt() {
        out.push(ChatMessage::text(Role::System, p));
    }
    out.extend(
        store::list_messages(db, conversation_id)?
            .into_iter()
            .filter(|m| m.id != exclude)
            .filter(|m| m.status == MessageStatus::Complete || !m.content.is_empty())
            .map(|m| m.to_chat_message()),
    );
    Ok(out)
}

fn start(
    state: &Arc<AppState>,
    reservation: crate::state::Reservation,
    model: ModelRef,
    backend: Arc<dyn ChatModel>,
    assistant: &StoredMessage,
    history: Vec<ChatMessage>,
    sink: impl EventSink,
) {
    let conversation_id = reservation.conversation_id;
    let (token, cancel) = reservation.into_parts();
    let mut cfg = LoopConfig::from_manifest(&state.agent.manifest, model.clone());
    cfg.conversation_id = Some(conversation_id);
    let state = Arc::clone(state);
    let message_id = assistant.id;
    let model_ref = model.to_string();
    tauri::async_runtime::spawn(async move {
        let result = run(&state, &*backend, &cfg, history, &cancel, message_id, &sink).await;
        let st = Arc::clone(&state);
        let live = state.live_text(message_id);
        // SQLite is blocking: keep it off the async workers.
        let write = tauri::async_runtime::spawn_blocking(move || {
            persist(&st, conversation_id, message_id, &model_ref, result, live)
        })
        .await;
        state.end_live(message_id);
        state.drop_approvals_for(conversation_id);
        state.release(conversation_id, token);
        match write {
            Ok(Ok(ev)) => sink.send(ev),
            Ok(Err(e)) => {
                eprintln!("could not store the answer {message_id}: {e}");
                sink.send(ChatEvent::Error {
                    message_id,
                    message: "The answer couldn't be saved.".into(),
                    recovery: vec![],
                });
            }
            Err(e) => eprintln!("could not store the answer {message_id}: {e}"),
        }
    });
}

/// Runs the agent loop and streams its events.
async fn run(
    state: &AppState,
    model: &dyn ChatModel,
    cfg: &LoopConfig,
    history: Vec<ChatMessage>,
    cancel: &CancelToken,
    message_id: Id,
    sink: &impl EventSink,
) -> Result<RunOutcome, CoreError> {
    let tools = Arc::clone(&state.tools);
    let mut on_event = |ev: AgentEvent| match ev {
        AgentEvent::StepStarted { step } if step > 0 && state.live_len(message_id) > 0 => {
            // Separate the text of successive steps (after tool results).
            let text = "\n\n".to_string();
            let offset = state.append_live(message_id, &text);
            sink.send(ChatEvent::TextDelta {
                message_id,
                offset,
                text,
            });
        }
        AgentEvent::TextDelta { text, .. } => {
            let offset = state.append_live(message_id, &text);
            sink.send(ChatEvent::TextDelta {
                message_id,
                offset,
                text,
            });
        }
        AgentEvent::ReasoningDelta { text, .. } => {
            sink.send(ChatEvent::ReasoningDelta { message_id, text })
        }
        AgentEvent::ToolCallStarted { call, .. } => sink.send(ChatEvent::ToolCallStarted {
            message_id,
            call_id: call.id,
            name: call.name,
            arguments: call.arguments,
        }),
        AgentEvent::ToolCallFinished {
            call_id, output, ..
        } => sink.send(ChatEvent::ToolCallFinished {
            message_id,
            call_id,
            is_error: output.is_error,
            summary: summary(&output.content),
        }),
        AgentEvent::StepStarted { .. } | AgentEvent::StepFinished { .. } => {}
    };
    dn_agent::run_loop(model, &*tools, cfg, history, cancel, &mut on_event).await
}

fn summary(parts: &[ContentPart]) -> String {
    let text: String = parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } => None,
        })
        .collect();
    let mut s: String = text.chars().take(240).collect();
    if text.chars().count() > 240 {
        s.push('…');
    }
    s
}

fn text_parts(text: String) -> Vec<ContentPart> {
    if text.is_empty() {
        vec![]
    } else {
        vec![ContentPart::Text { text }]
    }
}

fn finish_of(stop: StopReason) -> (FinishReason, MessageStatus, &'static str) {
    match stop {
        StopReason::Completed => (FinishReason::Stop, MessageStatus::Complete, "completed"),
        StopReason::Length => (FinishReason::Length, MessageStatus::Complete, "length"),
        StopReason::StepLimit => (
            FinishReason::ToolCalls,
            MessageStatus::Complete,
            "step_limit",
        ),
        StopReason::Cancelled => (
            FinishReason::Cancelled,
            MessageStatus::Cancelled,
            "cancelled",
        ),
    }
}

/// Stores the run: the first assistant turn goes into the placeholder, later turns and
/// tool results follow as their own rows (every tool call keeps its result). Returns the
/// final event for the UI. Blocking.
fn persist(
    state: &AppState,
    conversation_id: Id,
    placeholder: Id,
    model: &str,
    result: Result<RunOutcome, CoreError>,
    live_text: String,
) -> IpcResult<ChatEvent> {
    let db = state.db()?;
    let outcome = match result {
        Ok(o) => o,
        Err(e) => {
            let err = model_error(e);
            store::finish_message(
                &db,
                placeholder,
                MessageOutcome {
                    content: text_parts(live_text),
                    tool_calls: vec![],
                    status: MessageStatus::Error,
                    finish_reason: Some(FinishReason::Error),
                    error: Some(err.message.clone()),
                },
            )?;
            return Ok(ChatEvent::Error {
                message_id: placeholder,
                message: err.message,
                recovery: err.recovery,
            });
        }
    };
    let (finish_reason, status, stop) = finish_of(outcome.stop);
    let mut msgs = outcome.messages.into_iter();
    match msgs.next() {
        Some(first) if first.role == Role::Assistant => {
            let last_row = msgs.len() == 0;
            store::finish_message(
                &db,
                placeholder,
                MessageOutcome {
                    content: first.content,
                    tool_calls: first.tool_calls,
                    status: if last_row {
                        status
                    } else {
                        MessageStatus::Complete
                    },
                    finish_reason: Some(if last_row {
                        finish_reason
                    } else {
                        FinishReason::ToolCalls
                    }),
                    error: None,
                },
            )?;
        }
        other => {
            // Cancelled before the model said anything: keep what streamed, if anything.
            store::finish_message(
                &db,
                placeholder,
                MessageOutcome {
                    content: text_parts(live_text),
                    tool_calls: vec![],
                    status,
                    finish_reason: Some(finish_reason),
                    error: None,
                },
            )?;
            if let Some(m) = other {
                add_run_message(&db, conversation_id, model, m, status, finish_reason, true)?;
            }
        }
    }
    let rest: Vec<ChatMessage> = msgs.collect();
    let n = rest.len();
    for (i, m) in rest.into_iter().enumerate() {
        add_run_message(
            &db,
            conversation_id,
            model,
            m,
            status,
            finish_reason,
            i + 1 == n,
        )?;
    }
    Ok(ChatEvent::Done {
        message_id: placeholder,
        finish_reason,
        usage: outcome.usage,
        stop,
    })
}

fn add_run_message(
    db: &dn_store::Connection,
    conversation_id: Id,
    model: &str,
    m: ChatMessage,
    status: MessageStatus,
    finish_reason: FinishReason,
    last: bool,
) -> IpcResult<()> {
    let row = store::add_message(
        db,
        conversation_id,
        NewMessage {
            role: m.role,
            content: m.content.clone(),
            tool_call_id: m.tool_call_id.clone(),
            model: (m.role == Role::Assistant).then(|| model.to_string()),
            status: MessageStatus::Complete,
        },
    )?;
    if m.role == Role::Assistant {
        store::finish_message(
            db,
            row.id,
            MessageOutcome {
                content: m.content,
                tool_calls: m.tool_calls,
                status: if last {
                    status
                } else {
                    MessageStatus::Complete
                },
                finish_reason: Some(if last {
                    finish_reason
                } else {
                    FinishReason::ToolCalls
                }),
                error: None,
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeChatModel;
    use std::sync::Mutex;
    use std::time::Duration;

    fn state(delay_ms: u64) -> Arc<AppState> {
        let mut models = crate::models::ModelRegistry::default();
        models.register(
            "local:dev-fake".parse().unwrap(),
            Arc::new(FakeChatModel {
                delay: Duration::from_millis(delay_ms),
            }),
        );
        let mut s = AppState::in_memory().unwrap();
        s.models = models;
        let st = Arc::new(s);
        let mut settings = crate::settings::Settings::default();
        settings
            .slots
            .insert("chat".into(), "local:dev-fake".into());
        st.save_settings(&settings).unwrap();
        st
    }

    fn sink() -> (Arc<Mutex<Vec<ChatEvent>>>, impl EventSink) {
        let events = Arc::new(Mutex::new(vec![]));
        let e2 = Arc::clone(&events);
        (events, move |ev| e2.lock().unwrap().push(ev))
    }

    /// Waits until no message in the conversation is still streaming (stored outcome).
    async fn wait_idle(state: &AppState, conv: Id) {
        for _ in 0..600 {
            let streaming = {
                let db = state.db().unwrap();
                store::list_messages(&db, conv)
                    .unwrap()
                    .iter()
                    .any(|m| m.status == MessageStatus::Streaming)
            };
            if !streaming {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("generation did not finish");
    }

    fn count(state: &AppState, conv: Id) -> usize {
        store::list_messages(&state.db().unwrap(), conv)
            .unwrap()
            .len()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn send_streams_and_persists() {
        let st = state(0);
        let conv = st.create_conversation().unwrap();
        let (events, sink) = sink();
        let started = send(&st, conv.id, "Hello there".into(), sink).unwrap();
        assert_eq!(started.title.as_deref(), Some("Hello there"));
        wait_idle(&st, conv.id).await;

        let evs = events.lock().unwrap().clone();
        let streamed: String = evs
            .iter()
            .filter_map(|e| match e {
                ChatEvent::TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(matches!(
            evs.last(),
            Some(ChatEvent::Done {
                finish_reason: FinishReason::Stop,
                ..
            })
        ));
        let db = st.db().unwrap();
        let msgs = store::list_messages(&db, conv.id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].status, MessageStatus::Complete);
        assert_eq!(msgs[1].text(), streamed);
        assert!(streamed.contains("> Hello there"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_keeps_partial_text_as_cancelled() {
        let st = state(20);
        let conv = st.create_conversation().unwrap();
        let (events, sink) = sink();
        send(&st, conv.id, "stop me".into(), sink).unwrap();
        tokio::time::sleep(Duration::from_millis(90)).await;
        assert!(stop(&st, conv.id));
        wait_idle(&st, conv.id).await;
        let db = st.db().unwrap();
        let msgs = store::list_messages(&db, conv.id).unwrap();
        assert_eq!(msgs[1].status, MessageStatus::Cancelled);
        let full = FakeChatModel::reply_for(&crate::fake::test_support::request("stop me"));
        assert!(msgs[1].text().len() < full.len());
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(ChatEvent::Done {
                finish_reason: FinishReason::Cancelled,
                ..
            })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_replaces_last_answer() {
        let st = state(0);
        let conv = st.create_conversation().unwrap();
        let (_e, s1) = sink();
        let first = send(&st, conv.id, "again".into(), s1).unwrap();
        wait_idle(&st, conv.id).await;
        let (_e, s2) = sink();
        let second = regenerate(&st, conv.id, s2).unwrap();
        wait_idle(&st, conv.id).await;
        let db = st.db().unwrap();
        let msgs = store::list_messages(&db, conv.id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_ne!(msgs[1].id, first.assistant_message.id);
        assert_eq!(msgs[1].id, second.assistant_message.id);
        assert_eq!(msgs[1].status, MessageStatus::Complete);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_model_is_reported_not_faked() {
        let st = state(0);
        let mut settings = crate::settings::Settings::default();
        settings
            .slots
            .insert("chat".into(), "byok:openai:gpt-5".into());
        st.save_settings(&settings).unwrap();
        let conv = st.create_conversation().unwrap();
        let (_e, s) = sink();
        let err = send(&st, conv.id, "hi".into(), s).unwrap_err();
        assert_eq!(err.code, "model_unavailable");
        let db = st.db().unwrap();
        assert!(store::list_messages(&db, conv.id).unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn busy_send_writes_nothing() {
        let st = state(20);
        let conv = st.create_conversation().unwrap();
        let (_e, s1) = sink();
        send(&st, conv.id, "first".into(), s1).unwrap();
        let (_e, s2) = sink();
        let err = send(&st, conv.id, "second".into(), s2).unwrap_err();
        assert_eq!(err.code, "busy");
        assert_eq!(count(&st, conv.id), 2, "no orphan user message");
        assert!(stop(&st, conv.id));
        wait_idle(&st, conv.id).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_while_streaming_is_refused() {
        let st = state(20);
        let conv = st.create_conversation().unwrap();
        let (_e, s1) = sink();
        let first = send(&st, conv.id, "q".into(), s1).unwrap();
        let (_e, s2) = sink();
        assert_eq!(regenerate(&st, conv.id, s2).unwrap_err().code, "busy");
        let msgs = store::list_messages(&st.db().unwrap(), conv.id).unwrap();
        assert_eq!(msgs[1].id, first.assistant_message.id, "live answer kept");
        stop(&st, conv.id);
        wait_idle(&st, conv.id).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_then_resend_keeps_the_new_answer_stoppable() {
        let st = state(20);
        let conv = st.create_conversation().unwrap();
        let (_e, s1) = sink();
        let first = send(&st, conv.id, "one".into(), s1).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(stop(&st, conv.id));
        let (_e, s2) = sink();
        let second = send(&st, conv.id, "two".into(), s2).unwrap();
        // Let the stopped run finish and release; it must not free the new run's slot.
        for _ in 0..200 {
            let m = store::list_messages(&st.db().unwrap(), conv.id).unwrap();
            if m.iter()
                .any(|m| m.id == first.assistant_message.id && m.status == MessageStatus::Cancelled)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(st.is_generating(conv.id), "second answer still registered");
        assert!(stop(&st, conv.id), "second answer can be stopped");
        wait_idle(&st, conv.id).await;
        let msgs = store::list_messages(&st.db().unwrap(), conv.id).unwrap();
        assert_eq!(msgs.len(), 4);
        assert!(msgs.iter().any(|m| m.id == second.assistant_message.id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reopening_mid_stream_shows_text_so_far() {
        let st = state(15);
        let conv = st.create_conversation().unwrap();
        let (events, s1) = sink();
        let started = send(&st, conv.id, "hello".into(), s1).unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        let msgs = st.list_messages(conv.id).unwrap();
        let live = &msgs[1];
        assert_eq!(live.id, started.assistant_message.id);
        assert_eq!(live.status, MessageStatus::Streaming);
        assert!(!live.text().is_empty());
        stop(&st, conv.id);
        wait_idle(&st, conv.id).await;
        // Offsets are running UTF-16 lengths, so the UI can skip what it already has.
        let mut expected = 0u32;
        for e in events.lock().unwrap().iter() {
            if let ChatEvent::TextDelta { offset, text, .. } = e {
                assert_eq!(*offset, expected);
                expected += text.encode_utf16().count() as u32;
            }
        }
        assert!(expected > 0);
    }

    #[cfg(not(fake_model))]
    #[tokio::test(flavor = "multi_thread")]
    async fn no_model_is_model_source_required() {
        let st = Arc::new(AppState::in_memory().unwrap());
        let conv = st.create_conversation().unwrap();
        let (_e, s) = sink();
        let err = send(&st, conv.id, "hi".into(), s).unwrap_err();
        assert_eq!(err.code, "model_source_required");
        assert_eq!(count(&st, conv.id), 0);
    }

    /// Asks for one tool, then answers with its result.
    struct ToolThenAnswer;

    impl ChatModel for ToolThenAnswer {
        fn chat(&self, req: dn_core::chat::ChatRequest) -> dn_core::chat::EventStream {
            use dn_core::chat::{StreamEvent, ToolCall};
            let has_result = req.messages.iter().any(|m| m.role == Role::Tool);
            let events: Vec<Result<StreamEvent, CoreError>> = if has_result {
                vec![
                    Ok(StreamEvent::TextDelta {
                        text: "The answer is 42.".into(),
                    }),
                    Ok(StreamEvent::Done {
                        finish_reason: FinishReason::Stop,
                        usage: Usage::default(),
                    }),
                ]
            } else {
                vec![
                    Ok(StreamEvent::ToolCall {
                        call: ToolCall {
                            id: "call_1".into(),
                            name: "calc".into(),
                            arguments: serde_json::json!({ "q": "6*7" }),
                        },
                    }),
                    Ok(StreamEvent::Done {
                        finish_reason: FinishReason::ToolCalls,
                        usage: Usage::default(),
                    }),
                ]
            };
            Box::pin(Iter(events.into_iter()))
        }
    }

    struct Iter<I>(I);
    impl<I: Iterator<Item = Result<dn_core::chat::StreamEvent, CoreError>> + Unpin>
        futures_core::Stream for Iter<I>
    {
        type Item = Result<dn_core::chat::StreamEvent, CoreError>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(self.0.next())
        }
    }

    struct Calc;
    impl dn_agent::ToolExecutor for Calc {
        fn tools(&self) -> Vec<dn_core::chat::ToolSpec> {
            vec![dn_core::chat::ToolSpec {
                name: "calc".into(),
                description: "Calculates".into(),
                input_schema: serde_json::json!({ "type": "object" }),
            }]
        }
        fn execute<'a>(
            &'a self,
            _call: &'a dn_core::chat::ToolCall,
            _ctx: &'a dn_agent::ToolContext,
        ) -> dn_agent::BoxFuture<'a, Result<dn_agent::ToolOutput, CoreError>> {
            Box::pin(async { Ok(dn_agent::ToolOutput::text("42")) })
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_calls_run_and_are_stored_in_order() {
        let mut models = crate::models::ModelRegistry::default();
        models.register("local:scripted".parse().unwrap(), Arc::new(ToolThenAnswer));
        let mut s = AppState::in_memory().unwrap();
        s.models = models;
        s.tools = Arc::new(Calc);
        let st = Arc::new(s);
        let mut settings = crate::settings::Settings::default();
        settings
            .slots
            .insert("chat".into(), "local:scripted".into());
        st.save_settings(&settings).unwrap();
        let conv = st.create_conversation().unwrap();
        let (events, s1) = sink();
        let started = send(&st, conv.id, "what is 6*7?".into(), s1).unwrap();
        wait_idle(&st, conv.id).await;
        for _ in 0..100 {
            if events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, ChatEvent::Done { .. }))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let msgs = store::list_messages(&st.db().unwrap(), conv.id).unwrap();
        let roles: Vec<Role> = msgs.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            [Role::User, Role::Assistant, Role::Tool, Role::Assistant]
        );
        assert_eq!(msgs[1].id, started.assistant_message.id);
        assert_eq!(msgs[1].tool_calls[0].id, "call_1");
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(msgs[2].text(), "42");
        assert_eq!(msgs[3].text(), "The answer is 42.");
        assert!(msgs.iter().all(|m| m.status == MessageStatus::Complete));

        let evs = events.lock().unwrap().clone();
        assert!(
            evs.iter()
                .any(|e| matches!(e, ChatEvent::ToolCallStarted { name, .. } if name == "calc"))
        );
        assert!(evs.iter().any(|e| matches!(e, ChatEvent::ToolCallFinished { is_error: false, summary, .. } if summary == "42")));
        assert!(matches!(
            evs.last(),
            Some(ChatEvent::Done {
                stop: "completed",
                ..
            })
        ));

        // The stored history is valid input for the next turn (every call has its result).
        let (_e, s2) = sink();
        send(&st, conv.id, "thanks".into(), s2).unwrap();
        wait_idle(&st, conv.id).await;
        assert_eq!(
            store::list_messages(&st.db().unwrap(), conv.id)
                .unwrap()
                .len(),
            6
        );
    }

    #[test]
    fn titles() {
        assert_eq!(title_from("\n  hi there \nmore"), "hi there");
        assert_eq!(title_from(&"x".repeat(60)).chars().count(), 49);
        assert_eq!(title_from("   "), DEFAULT_TITLE);
    }
}
