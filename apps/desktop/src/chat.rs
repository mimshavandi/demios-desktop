//! Chat turns: write the user message and an assistant placeholder, stream the model's
//! answer to the UI, then store the final text. Independent of Tauri so it can be tested
//! with a plain event sink.

use crate::error::{IpcError, IpcResult};
use crate::state::AppState;
use dn_core::chat::{
    ChatRequest, ContentPart, FinishReason, Reasoning, Role, StreamEvent, ToolCall, Usage,
};
use dn_core::model::{Capability, ModelRef};
use dn_core::{CoreError, Id, RecoveryAction};
use dn_store::chat::{self as store, MessageOutcome, MessageStatus, NewMessage, StoredMessage};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::oneshot;

/// Events streamed to the UI for one assistant message.
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
    Done {
        message_id: Id,
        finish_reason: FinishReason,
        usage: Usage,
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

fn resolve_model(state: &AppState) -> Result<ModelRef, IpcError> {
    let settings = state.settings()?;
    if let Some(m) = settings.slot("chat") {
        return Ok(m);
    }
    #[cfg(fake_model)]
    {
        Ok(crate::fake::FAKE_MODEL_REF.parse().expect("valid ref"))
    }
    #[cfg(not(fake_model))]
    {
        Err(model_source_required())
    }
}

#[cfg_attr(fake_model, allow(dead_code))]
fn model_source_required() -> IpcError {
    let e = CoreError::ModelSourceRequired {
        capability: Capability::Chat,
        agent_id: "chat".into(),
        slot_id: "chat".into(),
        recovery: vec![
            RecoveryAction::DownloadModel {
                capability: Capability::Chat,
            },
            RecoveryAction::AddProviderKey,
            RecoveryAction::ChooseModelForSlot {
                agent_id: "chat".into(),
                slot_id: "chat".into(),
            },
        ],
    };
    let recovery = match &e {
        CoreError::ModelSourceRequired { recovery, .. } => recovery.clone(),
        _ => vec![],
    };
    IpcError {
        code: "model_source_required",
        message: "No chat model is set up. Choose one in Settings → Models.".into(),
        recovery,
    }
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
    let model = resolve_model(state)?;
    let backend = state
        .models
        .get(&model)
        .ok_or_else(model_unavailable(&model))?;
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
        let history = history(&db, conversation_id, assistant.id)?;
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
    let model = resolve_model(state)?;
    let backend = state
        .models
        .get(&model)
        .ok_or_else(model_unavailable(&model))?;
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
        let history = history(&db, conversation_id, assistant.id)?;
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

fn model_unavailable(model: &ModelRef) -> impl FnOnce() -> IpcError + '_ {
    move || IpcError {
        code: "model_unavailable",
        message: format!(
            "`{model}` is not available in this build yet. Choose another model in Settings → Models."
        ),
        recovery: vec![RecoveryAction::ChooseModelForSlot {
            agent_id: "chat".into(),
            slot_id: "chat".into(),
        }],
    }
}

fn history(
    db: &dn_store::Connection,
    conversation_id: Id,
    exclude: Id,
) -> IpcResult<Vec<dn_core::chat::ChatMessage>> {
    Ok(store::list_messages(db, conversation_id)?
        .into_iter()
        .filter(|m| m.id != exclude)
        // Failed or empty turns are not context the model should see.
        .filter(|m| m.status == MessageStatus::Complete || !m.content.is_empty())
        .map(|m| m.to_chat_message())
        .collect())
}

fn start(
    state: &Arc<AppState>,
    reservation: crate::state::Reservation,
    model: ModelRef,
    backend: Arc<dyn dn_core::chat::ChatModel>,
    assistant: &StoredMessage,
    history: Vec<dn_core::chat::ChatMessage>,
    sink: impl EventSink,
) {
    let req = ChatRequest {
        model,
        messages: history,
        tools: vec![],
        max_tokens: None,
        temperature: None,
        response_schema: None,
        reasoning: Reasoning::Auto,
        caller: "chat.reply".into(),
    };
    let conversation_id = reservation.conversation_id;
    let (token, rx) = reservation.into_parts();
    let state = Arc::clone(state);
    let message_id = assistant.id;
    tauri::async_runtime::spawn(async move {
        let stream = backend.chat(req);
        let outcome = drive(stream, rx, message_id, &state, &sink).await;
        let st = Arc::clone(&state);
        // SQLite is blocking: keep it off the async workers.
        let write = tauri::async_runtime::spawn_blocking(move || {
            st.db()
                .and_then(|db| store::finish_message(&db, message_id, outcome).map_err(Into::into))
        })
        .await;
        state.end_live(message_id);
        state.release(conversation_id, token);
        match write {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("could not store message {message_id}: {e}"),
            Err(e) => eprintln!("could not store message {message_id}: {e}"),
        }
    });
}

fn cancelled_outcome(text: String) -> MessageOutcome {
    MessageOutcome {
        content: text_parts(text),
        tool_calls: vec![],
        status: MessageStatus::Cancelled,
        finish_reason: Some(FinishReason::Cancelled),
        error: None,
    }
}

fn text_parts(text: String) -> Vec<ContentPart> {
    if text.is_empty() {
        vec![]
    } else {
        vec![ContentPart::Text { text }]
    }
}

/// Pulls the stream until it ends or `cancel` fires; dropping the stream cancels the call.
async fn drive(
    mut stream: dn_core::chat::EventStream,
    mut cancel: oneshot::Receiver<()>,
    message_id: Id,
    state: &AppState,
    sink: &impl EventSink,
) -> MessageOutcome {
    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = vec![];
    loop {
        tokio::select! {
            biased;
            _ = &mut cancel => {
                drop(stream);
                sink.send(ChatEvent::Done {
                    message_id,
                    finish_reason: FinishReason::Cancelled,
                    usage: Usage::default(),
                });
                return cancelled_outcome(text);
            }
            ev = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)) => match ev {
                Some(Ok(StreamEvent::TextDelta { text: t })) => {
                    text.push_str(&t);
                    let offset = state.append_live(message_id, &t);
                    sink.send(ChatEvent::TextDelta { message_id, offset, text: t });
                }
                Some(Ok(StreamEvent::ReasoningDelta { text: t })) => {
                    sink.send(ChatEvent::ReasoningDelta { message_id, text: t });
                }
                Some(Ok(StreamEvent::ToolCall { call })) => tool_calls.push(call),
                Some(Ok(StreamEvent::Done { finish_reason, usage })) => {
                    sink.send(ChatEvent::Done { message_id, finish_reason, usage });
                    return MessageOutcome {
                        content: text_parts(text),
                        tool_calls,
                        status: MessageStatus::Complete,
                        finish_reason: Some(finish_reason),
                        error: None,
                    };
                }
                Some(Err(e)) => {
                    let (message, recovery) = user_message(&e);
                    sink.send(ChatEvent::Error { message_id, message: message.clone(), recovery });
                    return MessageOutcome {
                        content: text_parts(text),
                        tool_calls,
                        status: MessageStatus::Error,
                        finish_reason: Some(FinishReason::Error),
                        error: Some(message),
                    };
                }
                None => {
                    sink.send(ChatEvent::Done {
                        message_id,
                        finish_reason: FinishReason::Stop,
                        usage: Usage::default(),
                    });
                    return MessageOutcome {
                        content: text_parts(text),
                        tool_calls,
                        status: MessageStatus::Complete,
                        finish_reason: Some(FinishReason::Stop),
                        error: None,
                    };
                }
            }
        }
    }
}

fn user_message(e: &CoreError) -> (String, Vec<RecoveryAction>) {
    match e {
        CoreError::ModelSourceRequired { recovery, .. } => {
            ("No model is set up for this chat.".into(), recovery.clone())
        }
        CoreError::ModelUnavailable {
            user_message,
            recovery,
            ..
        } => (user_message.clone(), recovery.clone()),
        CoreError::Capability { user_message, .. } => (user_message.clone(), vec![]),
        CoreError::Cancelled => ("Cancelled.".into(), vec![]),
    }
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

    #[test]
    fn titles() {
        assert_eq!(title_from("\n  hi there \nmore"), "hi there");
        assert_eq!(title_from(&"x".repeat(60)).chars().count(), 49);
        assert_eq!(title_from("   "), DEFAULT_TITLE);
        assert_eq!(model_source_required().recovery.len(), 3);
    }
}
