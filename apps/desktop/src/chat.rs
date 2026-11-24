//! Chat turns (T-SHELL-01, T-SHELL-04): write the user message and an assistant
//! placeholder, run the agent loop (`dn_agent::run_loop`: model → tools → model, with
//! approvals in dn-tools), stream progress to the UI, then store every message of the run
//! in order. Independent of Tauri so it can be tested with a plain event sink.

use crate::agent::model_error;
use crate::artifacts::{ArtifactMode, ArtifactTools, FencedRun, RunTools};
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
    /// An artifact was created or got a new version in this turn (T-ART-06).
    ArtifactChanged {
        message_id: Id,
        conversation_id: Id,
        artifact_id: Id,
        title: String,
        version: u32,
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
    let mode = ArtifactMode::for_model(state, &model);
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
        let history = history(state, &db, conversation_id, assistant.id, mode)?;
        (user, assistant, title, history)
    };
    start(
        state,
        reservation,
        model,
        backend,
        mode,
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
    let mode = ArtifactMode::for_model(state, &model);
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
        let history = history(state, &db, conversation_id, assistant.id, mode)?;
        (assistant, history)
    };
    start(
        state,
        reservation,
        model,
        backend,
        mode,
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
/// Models without tool calling get the fenced artifact format in the system prompt.
fn history(
    state: &AppState,
    db: &dn_store::Connection,
    conversation_id: Id,
    exclude: Id,
    mode: ArtifactMode,
) -> IpcResult<Vec<ChatMessage>> {
    let mut out = vec![];
    let fenced = (mode == ArtifactMode::Fenced
        && state.agent.manifest.allows_tool("artifact.create"))
    .then_some(dn_artifacts::protocol::FENCED_INSTRUCTIONS);
    let system: Vec<&str> = state.agent.system_prompt().into_iter().chain(fenced).collect();
    if !system.is_empty() {
        out.push(ChatMessage::text(Role::System, system.join("\n\n")));
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

#[allow(clippy::too_many_arguments)]
fn start(
    state: &Arc<AppState>,
    reservation: crate::state::Reservation,
    model: ModelRef,
    backend: Arc<dyn ChatModel>,
    mode: ArtifactMode,
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
    let sink: Arc<dyn EventSink> = Arc::new(sink);
    // Exactly one artifact protocol per run (ADR-0004): tools for tool-capable models, the
    // fenced parser for the rest.
    let tools = RunTools {
        base: Arc::clone(&state.tools),
        artifacts: (mode == ArtifactMode::Tools).then(|| ArtifactTools {
            state: Arc::clone(&state),
            conversation_id,
            message_id,
            sink: Arc::clone(&sink),
        }),
    };
    let fenced = (mode == ArtifactMode::Fenced
        && state.agent.manifest.allows_tool("artifact.create"))
    .then(|| FencedRun::start(Arc::clone(&state), conversation_id, message_id, Arc::clone(&sink)));
    tauri::async_runtime::spawn(async move {
        let (result, failed_calls) = run(
            &*backend, &tools, fenced, &cfg, history, &cancel, &state, message_id, &*sink,
        )
        .await;
        let st = Arc::clone(&state);
        let live = state.live_text(message_id);
        // SQLite is blocking: keep it off the async workers.
        let write = tauri::async_runtime::spawn_blocking(move || {
            persist(
                &st,
                conversation_id,
                message_id,
                &model_ref,
                result,
                live,
                &failed_calls,
            )
        })
        .await;
        state.end_live(message_id);
        // This run's waiting approvals closed with it; report them expired now.
        state.prune_closed_approvals();
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

/// Runs the agent loop and streams its events. With `fenced`, the model's text also goes
/// through the fenced artifact parser (models without tool calling only).
#[allow(clippy::too_many_arguments)]
async fn run(
    model: &dyn ChatModel,
    tools: &RunTools,
    mut fenced: Option<FencedRun>,
    cfg: &LoopConfig,
    history: Vec<ChatMessage>,
    cancel: &CancelToken,
    state: &AppState,
    message_id: Id,
    sink: &dyn EventSink,
) -> (
    Result<RunOutcome, CoreError>,
    std::collections::HashSet<String>,
) {
    // Tool calls that came back as errors: their stored result rows say so.
    let mut failed = std::collections::HashSet::new();
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
            if let Some(f) = fenced.as_mut() {
                f.push(&text);
            }
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
        } => {
            if output.is_error {
                failed.insert(call_id.clone());
            }
            sink.send(ChatEvent::ToolCallFinished {
                message_id,
                call_id,
                is_error: output.is_error,
                summary: summary(&output.content),
            })
        }
        AgentEvent::StepStarted { .. } | AgentEvent::StepFinished { .. } => {}
    };
    let result = dn_agent::run_loop(model, tools, cfg, history, cancel, &mut on_event).await;
    if let Some(f) = fenced {
        f.finish().await;
    }
    (result, failed)
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
    failed_calls: &std::collections::HashSet<String>,
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
                add_run_message(
                    &db,
                    conversation_id,
                    model,
                    m,
                    RunEnd {
                        status,
                        finish_reason,
                    },
                    true,
                    failed_calls,
                )?;
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
            RunEnd {
                status,
                finish_reason,
            },
            i + 1 == n,
            failed_calls,
        )?;
    }
    Ok(ChatEvent::Done {
        message_id: placeholder,
        finish_reason,
        usage: outcome.usage,
        stop,
    })
}

/// How the run ended; applied to its last assistant row.
#[derive(Clone, Copy)]
struct RunEnd {
    status: MessageStatus,
    finish_reason: FinishReason,
}

fn add_run_message(
    db: &dn_store::Connection,
    conversation_id: Id,
    model: &str,
    m: ChatMessage,
    end: RunEnd,
    last: bool,
    failed_calls: &std::collections::HashSet<String>,
) -> IpcResult<()> {
    let RunEnd {
        status,
        finish_reason,
    } = end;
    let failed_tool = m.role == Role::Tool
        && m.tool_call_id
            .as_ref()
            .is_some_and(|id| failed_calls.contains(id));
    let row = store::add_message(
        db,
        conversation_id,
        NewMessage {
            role: m.role,
            content: m.content.clone(),
            tool_call_id: m.tool_call_id.clone(),
            model: (m.role == Role::Assistant).then(|| model.to_string()),
            status: if failed_tool {
                MessageStatus::Error
            } else {
                MessageStatus::Complete
            },
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

    struct Calc(bool);
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
            let fail = self.0;
            Box::pin(async move {
                Ok(if fail {
                    dn_agent::ToolOutput::error("calculator offline")
                } else {
                    dn_agent::ToolOutput::text("42")
                })
            })
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_calls_run_and_are_stored_in_order() {
        let mut models = crate::models::ModelRegistry::default();
        models.register("local:scripted".parse().unwrap(), Arc::new(ToolThenAnswer));
        let mut s = AppState::in_memory().unwrap();
        s.models = models;
        s.tools = Arc::new(Calc(false));
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

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_tool_calls_are_stored_as_errors() {
        let mut models = crate::models::ModelRegistry::default();
        models.register("local:scripted".parse().unwrap(), Arc::new(ToolThenAnswer));
        let mut s = AppState::in_memory().unwrap();
        s.models = models;
        s.tools = Arc::new(Calc(true));
        let st = Arc::new(s);
        let mut settings = crate::settings::Settings::default();
        settings
            .slots
            .insert("chat".into(), "local:scripted".into());
        st.save_settings(&settings).unwrap();
        let conv = st.create_conversation().unwrap();
        let (_e, s1) = sink();
        send(&st, conv.id, "what is 6*7?".into(), s1).unwrap();
        wait_idle(&st, conv.id).await;
        let mut tool = None;
        for _ in 0..100 {
            tool = store::list_messages(&st.db().unwrap(), conv.id)
                .unwrap()
                .into_iter()
                .find(|m| m.role == Role::Tool);
            if tool.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let tool = tool.expect("tool result stored");
        assert_eq!(tool.status, MessageStatus::Error);
        assert!(tool.text().contains("calculator offline"));
    }

    // ---- artifacts (T-ART-06)

    /// App state with the repo agent (its manifest allows `artifact.*`) and `model` as the chat model.
    fn artifact_state(model: &str, backend: Arc<dyn ChatModel>, tools: bool) -> Arc<AppState> {
        let mut models = crate::models::ModelRegistry::default();
        if tools {
            models.register(model.parse().unwrap(), backend);
        } else {
            models.register_without_tools(model.parse().unwrap(), backend);
        }
        let mut s = AppState::in_memory().unwrap();
        s.models = models;
        s.agent = crate::agent::Agent::load(crate::agent::skills_dir(None).as_deref());
        let st = Arc::new(s);
        let mut settings = crate::settings::Settings::default();
        settings.slots.insert("chat".into(), model.into());
        st.save_settings(&settings).unwrap();
        st
    }

    async fn turn(st: &Arc<AppState>, conv: Id, text: &str) -> (TurnStarted, Vec<ChatEvent>) {
        let (events, s) = sink();
        let started = send(st, conv, text.into(), s).unwrap();
        for _ in 0..400 {
            if events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, ChatEvent::Done { .. } | ChatEvent::Error { .. }))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        wait_idle(st, conv).await;
        let evs = events.lock().unwrap().clone();
        (started, evs)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fake_model_creates_updates_and_restores_an_artifact() {
        let st = artifact_state(
            crate::fake::FAKE_MODEL_REF,
            Arc::new(FakeChatModel {
                delay: Duration::ZERO,
            }),
            true,
        );
        let conv = st.create_conversation().unwrap();

        let (started, evs) = turn(&st, conv.id, "Make an HTML artifact").await;
        let changed: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ChatEvent::ArtifactChanged {
                    artifact_id,
                    version,
                    message_id,
                    ..
                } => Some((*artifact_id, *version, *message_id)),
                _ => None,
            })
            .collect();
        assert_eq!(changed.len(), 1, "{evs:?}");
        let (id, v, msg) = changed[0];
        assert_eq!(v, 1);
        assert_eq!(msg, started.assistant_message.id);

        let a = crate::artifacts::get(&st, conv.id, id).unwrap();
        assert_eq!(a.versions.len(), 1);
        assert!(a.versions[0].version.content.contains(crate::fake::FAKE_ARTIFACT_V1));
        // Provenance for "Made by <model>".
        assert_eq!(a.versions[0].model.as_deref(), Some(crate::fake::FAKE_MODEL_REF));

        turn(&st, conv.id, "Please update the artifact").await;
        let a = crate::artifacts::get(&st, conv.id, id).unwrap();
        assert_eq!(a.versions.len(), 2);
        assert!(a.versions[1].version.content.contains(crate::fake::FAKE_ARTIFACT_V2));

        let a = crate::artifacts::rollback(&st, conv.id, id, 1).unwrap();
        let v3 = &a.versions[2].version;
        assert_eq!(v3.n, 3);
        assert_eq!(v3.parent_n, Some(1));
        assert_eq!(v3.author, dn_core::artifact::Author::User);
        assert_eq!(v3.content, a.versions[0].version.content);
        assert_eq!(a.versions[2].model, None);

        // Another conversation can't read it.
        let other = st.create_conversation().unwrap();
        assert_eq!(
            crate::artifacts::get(&st, other.id, id).unwrap_err().code,
            "not_found"
        );
        assert_eq!(crate::artifacts::list(&st, conv.id).unwrap().len(), 1);
    }

    const FENCED_REPLY: &str = "Here it is:\n<artifact id=\"page\" type=\"html\" title=\"Fenced page\">\n<p>fenced</p>\n</artifact>\nDone.";

    /// Answers with `FENCED_REPLY` and records what it was sent.
    #[derive(Default)]
    struct Recorder {
        requests: Mutex<Vec<dn_core::chat::ChatRequest>>,
        /// Also try an `artifact_create` call (a tool-less model must not get one through).
        call_tool: bool,
    }

    impl ChatModel for Recorder {
        fn chat(&self, req: dn_core::chat::ChatRequest) -> dn_core::chat::EventStream {
            use dn_core::chat::{StreamEvent, ToolCall};
            let answered = req.messages.iter().any(|m| m.role == Role::Tool);
            self.requests.lock().unwrap().push(req);
            let mut events = vec![Ok(StreamEvent::TextDelta {
                text: FENCED_REPLY.into(),
            })];
            if self.call_tool && !answered {
                events.push(Ok(StreamEvent::ToolCall {
                    call: ToolCall {
                        id: "c1".into(),
                        name: "artifact_create".into(),
                        arguments: serde_json::json!({
                            "type": "html", "title": "Sneaky", "content": "<p>x</p>"
                        }),
                    },
                }));
            }
            events.push(Ok(StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: Usage::default(),
            }));
            Box::pin(Iter(events.into_iter()))
        }
    }

    fn system_text(req: &dn_core::chat::ChatRequest) -> String {
        req.messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content.iter().map(|p| match p {
                ContentPart::Text { text } => text.as_str(),
                ContentPart::Image { .. } => "",
            }).collect::<String>())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_capable_models_never_get_the_fenced_parser() {
        let rec = Arc::new(Recorder::default());
        let st = artifact_state("local:scripted", rec.clone(), true);
        let conv = st.create_conversation().unwrap();
        let (_, evs) = turn(&st, conv.id, "make a page").await;
        assert!(
            !evs.iter().any(|e| matches!(e, ChatEvent::ArtifactChanged { .. })),
            "{evs:?}"
        );
        assert!(crate::artifacts::list(&st, conv.id).unwrap().is_empty());
        let reqs = rec.requests.lock().unwrap();
        let names: Vec<&str> = reqs[0].tools.iter().map(|t| t.name.as_str()).collect();
        for t in ["artifact_create", "artifact_update", "artifact_rewrite"] {
            assert!(names.contains(&t), "{names:?}");
        }
        assert!(!system_text(&reqs[0]).contains("<artifact id="));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_less_models_use_the_fenced_format_only() {
        let rec = Arc::new(Recorder {
            call_tool: true,
            ..Default::default()
        });
        let st = artifact_state("local:plain", rec.clone(), false);
        let conv = st.create_conversation().unwrap();
        let (started, evs) = turn(&st, conv.id, "make a page").await;
        assert!(
            evs.iter().any(|e| matches!(e, ChatEvent::ArtifactChanged { title, .. } if title == "Fenced page")),
            "{evs:?}"
        );
        let list = crate::artifacts::list(&st, conv.id).unwrap();
        assert_eq!(list.len(), 1, "only the fenced block, not the tool call");
        let a = crate::artifacts::get(&st, conv.id, list[0].id).unwrap();
        assert_eq!(a.versions[0].version.content, "<p>fenced</p>");
        assert_eq!(a.versions[0].version.message_id, Some(started.assistant_message.id));
        let reqs = rec.requests.lock().unwrap();
        assert!(
            !reqs[0].tools.iter().any(|t| t.name.starts_with("artifact_")),
            "no artifact tools for a tool-less model"
        );
        assert!(system_text(&reqs[0]).contains(dn_artifacts::protocol::FENCED_INSTRUCTIONS));
        // The stray tool call was refused, not run.
        let msgs = store::list_messages(&st.db().unwrap(), conv.id).unwrap();
        let tool = msgs.iter().find(|m| m.role == Role::Tool).expect("tool result row");
        assert!(tool.text().contains("isn't available"), "{}", tool.text());
    }

    #[test]
    fn titles() {
        assert_eq!(title_from("\n  hi there \nmore"), "hi there");
        assert_eq!(title_from(&"x".repeat(60)).chars().count(), 49);
        assert_eq!(title_from("   "), DEFAULT_TITLE);
    }
}
