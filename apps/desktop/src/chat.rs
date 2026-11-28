//! Chat turns (T-SHELL-01, T-SHELL-04): write the user message and an assistant
//! placeholder, run the agent loop (`dn_agent::run_loop`: model → tools → model, with
//! approvals in dn-tools), stream progress to the UI, then store every message of the run
//! in order. Independent of Tauri so it can be tested with a plain event sink.

use crate::agent::model_error;
use crate::artifacts::{ArtifactMode, ArtifactTools, RunTools};
use crate::error::{IpcError, IpcResult};
use crate::skills::Skill;
use crate::state::AppState;
use dn_agent::{
    AgentEvent, CancelToken, LoopConfig, RunMeter, RunOutcome, RunResult, StopReason, ToolExecutor,
};
use dn_core::chat::{ChatMessage, ChatModel, ContentPart, FinishReason, Role, Usage};
use dn_core::model::ModelRef;
use dn_core::{CoreError, Id, RecoveryAction};
use dn_store::chat::{self as store, MessageOutcome, MessageStatus, NewMessage, StoredMessage};
use dn_tools::CapabilityResolver;
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
    /// The label of the skill this answer is written with, if any (T-SKL-03).
    pub skill: Option<String>,
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
/// The model for the next turn and its backend. Blocking (settings, keychain, and loading
/// a local model's weights).
///
/// Only the chosen Chat model answers (#219). An empty slot gets the first suitable model
/// once ([`crate::commands::fill_empty_slots`]); otherwise the turn asks for a choice.
fn model_for_turn(state: &AppState) -> IpcResult<(ModelRef, Arc<dyn ChatModel>)> {
    // No chat without the assistant's instructions (#141): refuse before anything is written.
    state.agent.ready()?;
    let keys = crate::models::provider_keys(&state.keys);
    let settings = crate::commands::fill_empty_slots(state, state.settings()?, &keys)?;
    let chosen = settings.slot("chat");
    let model = match state.agent.resolve_model(&settings, &state.models, &keys) {
        Ok(m) => m,
        // The resolver only knows "disabled"; say why when it's a local model too big here.
        Err(e) => return Err(chosen.and_then(|m| wont_fit_error(state, &m)).unwrap_or(e)),
    };
    let backend = state.models.get(&model, &state.keys).map_err(model_error)?;
    Ok((model, backend))
}

fn wont_fit_error(state: &AppState, model: &ModelRef) -> Option<IpcError> {
    let local = state.models.local.as_ref()?;
    let id = crate::local::catalogue_id(model)?;
    if !(local.is_ready(id) && local.wont_fit(id)) {
        return None;
    }
    Some(IpcError {
        code: "model_unavailable",
        message: format!(
            "{} won't fit on this computer. Choose a smaller model in Settings → Models.",
            local.name(id).unwrap_or_else(|| id.to_string())
        ),
        recovery: vec![RecoveryAction::ChooseModelForSlot {
            agent_id: crate::agent::AGENT_ID.into(),
            slot_id: "primary".into(),
        }],
    })
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
    // Reserve first: loading a local model can take seconds, and a second send meanwhile
    // must get "busy", not load the weights again.
    let reservation = state.reserve(conversation_id)?;
    let (model, backend) = model_for_turn(state)?;
    let mode = ArtifactMode::for_model(state, &model);
    let (user, assistant, title, history, skill) = {
        let db = state.db()?;
        let conv = store::get_conversation(&db, conversation_id)?;
        // Before any write: a chat whose skill is gone stops here with a clear message.
        let skill = turn_skill(state, &db, conversation_id)?;
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
        if let Some(s) = &skill {
            crate::skills::record_answer_skill(&db, conversation_id, assistant.id, s)?;
        }
        let history = turn_parts(state, &db, conversation_id, assistant.id, skill.as_deref())?;
        (user, assistant, title, history, skill)
    };
    let label = skill.as_ref().map(|s| s.skill.label());
    start(
        state,
        reservation,
        model,
        backend,
        mode,
        &assistant,
        history,
        skill,
        sink,
    );
    Ok(TurnStarted {
        user_message: Some(user),
        assistant_message: assistant,
        title,
        skill: label,
    })
}

/// Drops the answer after the last user message and answers it again. Refused while an
/// answer is streaming, so the live answer is never truncated.
pub fn regenerate(
    state: &Arc<AppState>,
    conversation_id: Id,
    sink: impl EventSink,
) -> IpcResult<TurnStarted> {
    // Reserve first: loading a local model can take seconds, and a second send meanwhile
    // must get "busy", not load the weights again.
    let reservation = state.reserve(conversation_id)?;
    let (model, backend) = model_for_turn(state)?;
    let mode = ArtifactMode::for_model(state, &model);
    let (assistant, history, skill) = {
        let db = state.db()?;
        let msgs = store::list_messages(&db, conversation_id)?;
        let last_user = msgs
            .iter()
            .rposition(|m| m.role == Role::User)
            .ok_or_else(|| IpcError::invalid("Nothing to regenerate."))?;
        let skill = turn_skill(state, &db, conversation_id)?;
        if let Some(after) = msgs.get(last_user + 1) {
            store::truncate_from(&db, conversation_id, after.id)?;
        }
        store::set_conversation_model(&db, conversation_id, &model.to_string())?;
        let assistant = store::add_message(
            &db,
            conversation_id,
            NewMessage::assistant_placeholder(Some(model.to_string())),
        )?;
        if let Some(s) = &skill {
            crate::skills::record_answer_skill(&db, conversation_id, assistant.id, s)?;
        }
        let history = turn_parts(state, &db, conversation_id, assistant.id, skill.as_deref())?;
        (assistant, history, skill)
    };
    let label = skill.as_ref().map(|s| s.skill.label());
    start(
        state,
        reservation,
        model,
        backend,
        mode,
        &assistant,
        history,
        skill,
        sink,
    );
    Ok(TurnStarted {
        user_message: None,
        assistant_message: assistant,
        title: None,
        skill: label,
    })
}

/// Stops the running answer in `conversation_id`, if any. Returns whether one was running.
pub fn stop(state: &AppState, conversation_id: Id) -> bool {
    state.stop(conversation_id)
}

/// The chat's skill (T-SKL-01), if one is chosen and still in the library.
fn turn_skill(
    state: &AppState,
    db: &dn_store::Connection,
    conversation_id: Id,
) -> IpcResult<Option<Arc<Skill>>> {
    let Some(name) = crate::skills::chat_skill(db, conversation_id)? else {
        return Ok(None);
    };
    // A skill that was removed or stopped loading never silently turns into "no skill".
    state.skills.get(&name).map(Some).ok_or_else(|| {
        IpcError::new(
            "skill_unavailable",
            format!(
                "The skill \"{name}\" chosen for this chat isn't available any more. Choose \
                 another skill, or No skill, in the message box."
            ),
        )
    })
}

/// Model-facing tool name → capability id: artifact tools by name, MCP tools through the
/// executor (read live). A name neither knows resolves to nothing, so `Narrowed` refuses it.
fn tool_resolver(state: &AppState) -> CapabilityResolver {
    let mcp = state.executor.as_ref().map(|e| e.capability_resolver());
    Arc::new(move |name: &str| {
        crate::artifacts::capability_id(name).or_else(|| mcp.as_ref().and_then(|r| r(name)))
    })
}

/// Tells the model which data files are attached to this chat (#370), so it can read them
/// with the data tools. File names are the person's, quoted and kept to one line.
fn attached_files_note(
    db: &dn_store::Connection,
    conversation_id: Id,
) -> IpcResult<Option<String>> {
    let names: Vec<String> = crate::chat_files::list(db, conversation_id)?
        .into_iter()
        .filter(|f| dn_data::tool::reads(&f.name))
        .map(|f| {
            let flat: String = f
                .name
                .chars()
                .map(|c| {
                    // One line: no control characters, line or paragraph separators.
                    if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                        ' '
                    } else {
                        c
                    }
                })
                .collect();
            format!(
                "\"{}\"",
                dn_core::untrusted::neutralise(&flat.replace('"', "'"))
            )
        })
        .collect();
    Ok((!names.is_empty()).then(|| {
        format!(
            "Data files attached to this chat: {}. Read their numbers with data_profile and \
             data_aggregate.",
            names.join(", ")
        )
    }))
}

/// What one turn sends (ADR-0020 §2): the slots the assembler fills around the stored
/// conversation, and that conversation with the new input last.
pub(crate) struct TurnInput {
    pub parts: dn_runtime::Parts,
    /// The stored conversation, minus the new placeholder and failed empty turns.
    pub history: Vec<ChatMessage>,
    /// How many of `history` come before the input (the input is the last user message).
    pub turns: usize,
}

/// The turn's slots: the agent prompt, the shared answer rules, the chat's project
/// instructions, attached data files and skill (system); the question's project excerpts
/// (retrieved); and the date (recap). Blocking.
fn turn_parts(
    state: &AppState,
    db: &dn_store::Connection,
    conversation_id: Id,
    exclude: Id,
    skill: Option<&Skill>,
) -> IpcResult<TurnInput> {
    let project = crate::projects::instructions_for(db, conversation_id)?;
    let messages = store::list_messages(db, conversation_id)?;
    let question = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.text())
        .unwrap_or_default();
    let retrieved = crate::knowledge::context_for(state, db, conversation_id, exclude, &question)?;
    let attached = attached_files_note(db, conversation_id)?;
    let system: Vec<String> = state
        .agent
        .system_prompt()
        .map(str::to_owned)
        .into_iter()
        .chain(
            dn_runtime::Policy::chat()
                .answer_rules
                .then(|| state.agent.answer_rules().map(str::to_owned))
                .flatten(),
        )
        .chain(project)
        .chain(attached)
        .chain(skill.map(Skill::system_text))
        .collect();
    let history: Vec<ChatMessage> = messages
        .into_iter()
        .filter(|m| m.id != exclude)
        .filter(|m| m.status == MessageStatus::Complete || !m.content.is_empty())
        .map(|m| m.to_chat_message())
        .collect();
    let turns = match history.last() {
        Some(m) if m.role == Role::User => history.len() - 1,
        _ => history.len(),
    };
    Ok(TurnInput {
        parts: dn_runtime::Parts {
            system,
            retrieved,
            recap: Some(format!("Today is {}.", crate::agent::today())),
            ..Default::default()
        },
        history,
        turns,
    })
}

#[allow(clippy::too_many_arguments)]
fn start(
    state: &Arc<AppState>,
    reservation: crate::state::Reservation,
    model: ModelRef,
    backend: Arc<dyn ChatModel>,
    mode: ArtifactMode,
    assistant: &StoredMessage,
    history: TurnInput,
    skill: Option<Arc<Skill>>,
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
    // The chat's project, for the data tool (the callers have released the database).
    let project_id = state
        .db()
        .ok()
        .and_then(|db| store::get_conversation(&db, conversation_id).ok())
        .and_then(|c| c.project_id);
    let has_chat_files = state
        .db()
        .ok()
        .is_some_and(|db| crate::chat_files::has_data_files(&db, conversation_id));
    // Artifacts only through tool calls (ADR-0004): a model without tool calling gets none.
    let contrast = crate::artifacts::contrast_offered(&state, mode, skill.as_deref());
    let tools: Arc<dyn ToolExecutor> = Arc::new(RunTools {
        base: Arc::clone(&state.tools),
        contrast,
        artifacts: (mode == ArtifactMode::Tools).then(|| ArtifactTools {
            state: Arc::clone(&state),
            conversation_id,
            message_id,
            sink: Arc::clone(&sink),
        }),
        // In a project chat, or a chat with attached data files (#370).
        data: (mode == ArtifactMode::Tools && (project_id.is_some() || has_chat_files)).then(
            || crate::data_tools::DataTools {
                state: Arc::clone(&state),
                project_id,
                conversation_id: Some(conversation_id),
            },
        ),
    });
    // The chat's skill only narrows: approvals are decided behind this, by the executor.
    let tools = match &skill {
        Some(s) => crate::skills::narrow(tools, s, tool_resolver(&state)),
        None => tools,
    };
    // A size the person set answers for a model nobody else knows (design ContextSize).
    let user_size = state
        .settings()
        .ok()
        .and_then(|s| s.context_sizes.get(&model.to_string()).copied());
    let sized: Arc<dyn ChatModel> = Arc::new(dn_runtime::WithUserSize {
        inner: Arc::clone(&backend),
        size: user_size,
    });
    // Every step of the run goes through the context assembler (ADR-0020 §2).
    let builder = dn_runtime::TurnBuilder::new(
        sized,
        dn_runtime::Policy::chat(),
        history.parts,
        history.turns,
        Arc::clone(&state.estimator),
        &model,
    );
    let history = history.history;
    tauri::async_runtime::spawn(async move {
        let (result, failed_calls, run_result) = run(
            &*backend, &*tools, &cfg, history, &builder, &cancel, &state, message_id, &*sink,
        )
        .await;
        let st = Arc::clone(&state);
        let live = state.live_text(message_id);
        // SQLite is blocking: keep it off the async workers.
        let write = tauri::async_runtime::spawn_blocking(move || {
            record_run(&st, conversation_id, message_id, &run_result);
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
                tracing::warn!("could not store the answer {message_id}: {e}");
                sink.send(ChatEvent::Error {
                    message_id,
                    message: "The answer couldn't be saved.".into(),
                    recovery: vec![],
                });
            }
            Err(e) => tracing::warn!("could not store the answer {message_id}: {e}"),
        }
    });
}

/// Runs the agent loop and streams its events.
#[allow(clippy::too_many_arguments)]
async fn run(
    model: &dyn ChatModel,
    tools: &dyn ToolExecutor,
    cfg: &LoopConfig,
    history: Vec<ChatMessage>,
    builder: &dyn dn_agent::RequestBuilder,
    cancel: &CancelToken,
    state: &AppState,
    message_id: Id,
    sink: &dyn EventSink,
) -> (
    Result<RunOutcome, CoreError>,
    std::collections::HashSet<String>,
    RunResult,
) {
    // Tool calls that came back as errors: their stored result rows say so.
    let mut failed = std::collections::HashSet::new();
    // A run that ends in Err returns no outcome: its result comes from the events.
    let mut meter = RunMeter::start(Id::new(), cfg.caller.clone(), cfg.model.clone());
    let mut on_event = |ev: AgentEvent| {
        dn_agent::observe_event(&mut meter, &ev);
        on_agent_event(ev, state, message_id, sink, &mut failed)
    };
    let result =
        dn_agent::run_loop_with(model, tools, cfg, history, builder, cancel, &mut on_event).await;
    let run = match &result {
        Ok(o) => o.result.clone(),
        Err(_) => meter.finish(StopReason::Failed),
    };
    (result, failed, run)
}

/// Turns one loop event into UI events.
fn on_agent_event(
    ev: AgentEvent,
    state: &AppState,
    message_id: Id,
    sink: &dyn EventSink,
    failed: &mut std::collections::HashSet<String>,
) {
    match ev {
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
    }
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
        StopReason::Failed => (FinishReason::Error, MessageStatus::Error, "failed"),
    }
}

/// Stores the turn's run result with its answer. Usage is a record, not part of the answer:
/// a failure to store it doesn't lose the answer, but it is reported. Blocking.
fn record_run(state: &AppState, conversation_id: Id, message_id: Id, run: &RunResult) {
    let r = state.db().map_err(|e| e.message).and_then(|db| {
        crate::runs::record(&db, run, Some(conversation_id), Some(message_id))
            .map_err(|e| e.to_string())
    });
    if let Err(e) = r {
        tracing::warn!(run_id = %run.run_id, error = %e, "could not store the run's usage");
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
    let (finish_reason, status, stop) = finish_of(outcome.result.stop);
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
        usage: outcome.result.usage,
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

    #[test]
    fn no_chat_starts_when_the_agent_failed_to_load() {
        let mut s = AppState::in_memory().unwrap();
        s.agent = crate::agent::Agent::load(Some(std::path::Path::new("/definitely/not/here")));
        let st = Arc::new(s);
        let conv = st.create_conversation().unwrap();
        let (_, events) = sink();
        let e = send(&st, conv.id, "hi".into(), events).unwrap_err();
        assert_eq!(e.code, "agent_unavailable");
        let (_, events) = sink();
        assert_eq!(
            regenerate(&st, conv.id, events).unwrap_err().code,
            "agent_unavailable"
        );
        assert!(
            st.list_messages(conv.id).unwrap().is_empty(),
            "nothing was written"
        );
        assert!(!st.is_generating(conv.id), "the slot was released");
    }

    fn sink() -> (Arc<Mutex<Vec<ChatEvent>>>, impl EventSink) {
        let events = Arc::new(Mutex::new(vec![]));
        let e2 = Arc::clone(&events);
        (events, move |ev| e2.lock().unwrap().push(ev))
    }

    /// Waits until no message in the conversation is still streaming (stored outcome).
    /// Waits until the run is fully over: its row stored (not "streaming") *and* the chat's
    /// busy flag released. The flag is cleared just after the row is written, so waiting on
    /// the row alone let the next send hit "busy" (flaky on CI).
    async fn wait_idle(state: &AppState, conv: Id) {
        for _ in 0..600 {
            let streaming = state.is_generating(conv) || {
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
        // The turn's run result is stored with the answer (ADR-0020 §8).
        let run = crate::runs::for_message(&db, msgs[1].id)
            .unwrap()
            .expect("a run record");
        assert_eq!(run.stop, StopReason::Completed);
        assert_eq!(run.caller, st.agent.manifest.policies.caller);
        assert_eq!(run.steps, 1);
        assert!(run.usage.output_tokens > 0, "{run:?}");
        assert!(run.first_token_ms.is_some_and(|t| t <= run.wall_ms));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_model_that_fails_mid_answer_still_leaves_a_failed_run_record() {
        type Item = Result<dn_core::chat::StreamEvent, CoreError>;
        /// Streams a word, then fails.
        struct Breaks;
        struct Items(std::collections::VecDeque<Item>);
        impl futures_core::Stream for Items {
            type Item = Item;
            fn poll_next(
                mut self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Item>> {
                std::task::Poll::Ready(self.0.pop_front())
            }
        }
        impl ChatModel for Breaks {
            fn limits<'a>(
                &'a self,
                _model: &'a dn_core::model::ModelRef,
            ) -> dn_core::chat::BoxFuture<'a, dn_core::chat::ModelLimits> {
                // A fake: a known size, so the context assembler can fit the turn (ADR-0020 §2).
                Box::pin(async {
                    dn_core::chat::ModelLimits::known(32_768, dn_core::chat::LimitSource::Provider)
                })
            }
            fn chat(&self, _: dn_core::chat::ChatRequest) -> dn_core::chat::EventStream {
                Box::pin(Items(
                    vec![
                        Ok(dn_core::chat::StreamEvent::TextDelta { text: "Hal".into() }),
                        Err(CoreError::ModelUnavailable {
                            user_message: "The model stopped.".into(),
                            developer_message: "test".into(),
                            recovery: vec![],
                        }),
                    ]
                    .into(),
                ))
            }
        }
        let mut models = crate::models::ModelRegistry::default();
        models.register("local:breaks".parse().unwrap(), Arc::new(Breaks));
        let mut s = AppState::in_memory().unwrap();
        s.models = models;
        let st = Arc::new(s);
        let mut settings = crate::settings::Settings::default();
        settings.slots.insert("chat".into(), "local:breaks".into());
        st.save_settings(&settings).unwrap();
        let conv = st.create_conversation().unwrap();
        let (_e, sink) = sink();
        let started = send(&st, conv.id, "hi".into(), sink).unwrap();
        wait_idle(&st, conv.id).await;
        let db = st.db().unwrap();
        let run = crate::runs::for_message(&db, started.assistant_message.id)
            .unwrap()
            .expect("a run record for the failed answer");
        assert_eq!((run.stop, run.steps), (StopReason::Failed, 1));
        assert!(run.first_token_ms.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_keeps_partial_text_as_cancelled() {
        let st = state(20);
        let conv = st.create_conversation().unwrap();
        let (events, sink) = sink();
        send(&st, conv.id, "stop me".into(), sink).unwrap();
        // Stop once text has started to stream (not after a guessed delay): the reply is then
        // certainly partial, however slow the machine.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, ChatEvent::TextDelta { .. }))
        {
            assert!(std::time::Instant::now() < deadline, "no text streamed");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(stop(&st, conv.id));
        wait_idle(&st, conv.id).await;
        let db = st.db().unwrap();
        let msgs = store::list_messages(&db, conv.id).unwrap();
        assert_eq!(msgs[1].status, MessageStatus::Cancelled);
        let full = FakeChatModel::reply_for(&crate::fake::test_support::request("stop me"));
        assert!(msgs[1].text().len() < full.len());
        let run = crate::runs::for_message(&db, msgs[1].id)
            .unwrap()
            .expect("a run record");
        assert_eq!(run.stop, StopReason::Cancelled);
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
    async fn no_model_asks_for_a_choice() {
        let st = Arc::new(AppState::in_memory().unwrap());
        let conv = st.create_conversation().unwrap();
        let (_e, s) = sink();
        let err = send(&st, conv.id, "hi".into(), s).unwrap_err();
        assert_eq!(err.code, "model_choice_required");
        assert_eq!(count(&st, conv.id), 0);
    }

    /// Asks for one tool, then answers with its result.
    struct ToolThenAnswer;

    impl ChatModel for ToolThenAnswer {
        fn limits<'a>(
            &'a self,

            _model: &'a dn_core::model::ModelRef,
        ) -> dn_core::chat::BoxFuture<'a, dn_core::chat::ModelLimits> {
            // A fake: a known size, so the context assembler can fit the turn (ADR-0020 §2).

            Box::pin(async {
                dn_core::chat::ModelLimits::known(32_768, dn_core::chat::LimitSource::Provider)
            })
        }
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
        assert!(
            a.versions[0]
                .version
                .content
                .contains(crate::fake::FAKE_ARTIFACT_V1)
        );
        // Provenance for "Made by <model>".
        assert_eq!(
            a.versions[0].model.as_deref(),
            Some(crate::fake::FAKE_MODEL_REF)
        );

        turn(&st, conv.id, "Please update the artifact").await;
        let a = crate::artifacts::get(&st, conv.id, id).unwrap();
        assert_eq!(a.versions.len(), 2);
        assert!(
            a.versions[1]
                .version
                .content
                .contains(crate::fake::FAKE_ARTIFACT_V2)
        );

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

    /// An answer with `<artifact>` text in it: it must never create anything (no text format).
    const ARTIFACT_TEXT_REPLY: &str = "Here it is:\n<artifact id=\"page\" type=\"html\" title=\"Text page\">\n<p>text</p>\n</artifact>\nDone.";

    /// Answers with `ARTIFACT_TEXT_REPLY` and records what it was sent.
    #[derive(Default)]
    struct Recorder {
        requests: Mutex<Vec<dn_core::chat::ChatRequest>>,
        /// Also try an `artifact_create` call (a tool-less model must not get one through).
        call_tool: bool,
    }

    impl ChatModel for Recorder {
        fn limits<'a>(
            &'a self,

            _model: &'a dn_core::model::ModelRef,
        ) -> dn_core::chat::BoxFuture<'a, dn_core::chat::ModelLimits> {
            // A fake: a known size, so the context assembler can fit the turn (ADR-0020 §2).

            Box::pin(async {
                dn_core::chat::ModelLimits::known(32_768, dn_core::chat::LimitSource::Provider)
            })
        }
        fn chat(&self, req: dn_core::chat::ChatRequest) -> dn_core::chat::EventStream {
            use dn_core::chat::{StreamEvent, ToolCall};
            let answered = req.messages.iter().any(|m| m.role == Role::Tool);
            self.requests.lock().unwrap().push(req);
            let mut events = vec![Ok(StreamEvent::TextDelta {
                text: ARTIFACT_TEXT_REPLY.into(),
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
            .map(|m| {
                m.content
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => text.as_str(),
                        ContentPart::Image { .. } => "",
                    })
                    .collect::<String>()
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn project_instructions_are_sent_with_each_chat_of_the_project() {
        let rec = Arc::new(Recorder::default());
        let st = artifact_state("local:scripted", rec.clone(), true);
        let (inside, outside) = {
            let db = st.db().unwrap();
            let p = dn_store::project::create_project(&db, "Thesis").unwrap();
            dn_store::project::update_project(&db, p.id, "Thesis", "Answer in British English.")
                .unwrap();
            (
                store::create_conversation_in(&db, "New chat", Some(p.id)).unwrap(),
                store::create_conversation(&db, "New chat").unwrap(),
            )
        };
        turn(&st, inside.id, "hi").await;
        turn(&st, outside.id, "hi").await;
        let reqs = rec.requests.lock().unwrap();
        assert!(system_text(&reqs[0]).contains("Answer in British English."));
        assert!(!system_text(&reqs[1]).contains("British"));
    }

    /// A model whose size nobody knows: [`Recorder`]'s answers, no `limits`.
    struct NoSize(Arc<Recorder>);
    impl ChatModel for NoSize {
        fn chat(&self, req: dn_core::chat::ChatRequest) -> dn_core::chat::EventStream {
            self.0.chat(req)
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_context_size_asks_the_person_and_their_size_is_used() {
        let rec = Arc::new(Recorder::default());
        let st = artifact_state("local:scripted", Arc::new(NoSize(rec.clone())), true);
        let conv = st.create_conversation().unwrap();
        let (_, evs) = turn(&st, conv.id, "hi").await;
        let Some(ChatEvent::Error {
            recovery, message, ..
        }) = evs.last()
        else {
            panic!("{evs:?}")
        };
        assert!(
            message.contains("doesn't know how much text local:scripted"),
            "{message}"
        );
        assert_eq!(
            recovery,
            &[RecoveryAction::SetContextSize {
                model_ref: "local:scripted".into()
            }]
        );
        assert!(rec.requests.lock().unwrap().is_empty(), "nothing was sent");
        // The person sets a size: the next turn goes through with it.
        let mut settings = st.settings().unwrap();
        settings
            .context_sizes
            .insert("local:scripted".into(), 32_768);
        st.save_settings(&settings).unwrap();
        let (started, evs) = turn(&st, conv.id, "hi again").await;
        assert!(
            matches!(evs.last(), Some(ChatEvent::Done { .. })),
            "{evs:?}"
        );
        let db = st.db().unwrap();
        let run = crate::runs::for_message(&db, started.assistant_message.id)
            .unwrap()
            .unwrap();
        assert_eq!(run.context.unwrap().budget_tokens, 32_768 - 4_096);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_turn_goes_through_the_assembler_and_its_run_keeps_the_context_use() {
        let rec = Arc::new(Recorder::default());
        let st = artifact_state("local:scripted", rec.clone(), true);
        let conv = st.create_conversation().unwrap();
        let (started, _) = turn(&st, conv.id, "hi").await;
        {
            let reqs = rec.requests.lock().unwrap();
            let last = reqs[0].messages.last().unwrap();
            assert_eq!(last.role, Role::User);
            let texts: Vec<String> = last
                .content
                .iter()
                .filter_map(|p| match p {
                    dn_core::chat::ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect();
            // The input as typed, then the date in the recap (ADR-0020 §2.3).
            assert_eq!(texts[0], "hi");
            assert!(texts[1].starts_with("Today is "), "{texts:?}");
        }
        // The stored message keeps only what was typed.
        let db = st.db().unwrap();
        let stored = store::list_messages(&db, conv.id).unwrap();
        assert_eq!(stored[0].text(), "hi");
        let run = crate::runs::for_message(&db, started.assistant_message.id)
            .unwrap()
            .expect("the run is stored");
        let c = run.context.expect("the assembler's context use is stored");
        assert!(c.used_tokens > 0 && c.used_tokens <= c.budget_tokens);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn artifact_text_in_an_answer_never_creates_anything() {
        let rec = Arc::new(Recorder::default());
        let st = artifact_state("local:scripted", rec.clone(), true);
        let conv = st.create_conversation().unwrap();
        let (_, evs) = turn(&st, conv.id, "make a page").await;
        assert!(
            !evs.iter()
                .any(|e| matches!(e, ChatEvent::ArtifactChanged { .. })),
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

    /// A tool-capable model in a chat with the skill `notes`, which allows only `calc`.
    fn skill_state(rec: Arc<Recorder>, tools: bool, allowed: &[&str]) -> (Arc<AppState>, Id) {
        let mut models = crate::models::ModelRegistry::default();
        if tools {
            models.register("local:scripted".parse().unwrap(), rec);
        } else {
            models.register_without_tools("local:scripted".parse().unwrap(), rec);
        }
        let mut s = AppState::in_memory().unwrap();
        s.models = models;
        s.agent = crate::agent::Agent::load(crate::agent::skills_dir(None).as_deref());
        s.tools = Arc::new(Calc(false));
        s.skills = crate::skills::Library::from_skills(vec![dn_agent::ChatSkill {
            name: "notes".into(),
            description: "Notes".into(),
            allowed_tools: allowed.iter().map(|t| t.to_string()).collect(),
            license: None,
            instructions: "Answer in bullet points.".into(),
        }]);
        let st = Arc::new(s);
        let mut settings = crate::settings::Settings::default();
        settings
            .slots
            .insert("chat".into(), "local:scripted".into());
        st.save_settings(&settings).unwrap();
        let conv = st.create_conversation().unwrap();
        crate::skills::set_chat_skill(&st.db().unwrap(), conv.id, Some("notes")).unwrap();
        (st, conv.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_chat_skill_adds_its_instructions_and_narrows_the_tools() {
        let rec = Arc::new(Recorder {
            call_tool: true,
            ..Default::default()
        });
        let (st, conv) = skill_state(rec.clone(), true, &["artifact.create"]);
        turn(&st, conv, "make a page").await;
        let reqs = rec.requests.lock().unwrap();
        let names: Vec<&str> = reqs[0].tools.iter().map(|t| t.name.as_str()).collect();
        // Only the listed tool: not the other artifact tools, not `calc`.
        assert_eq!(names, ["artifact_create"]);
        assert!(system_text(&reqs[0]).contains("Answer in bullet points."));
        // The listed tool ran.
        assert_eq!(crate::artifacts::list(&st, conv).unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contrast_check_is_offered_only_to_a_skill_that_names_it() {
        let rec = Arc::new(Recorder::default());
        let (st, conv) = skill_state(rec.clone(), true, &["contrast.check"]);
        turn(&st, conv, "check my colours").await;
        let names: Vec<String> = rec.requests.lock().unwrap()[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert_eq!(names, ["contrast_check"]);

        // A skill that doesn't name it, and a chat without a skill, don't get it.
        let rec = Arc::new(Recorder::default());
        let (st, conv) = skill_state(rec.clone(), true, &["artifact.*"]);
        turn(&st, conv, "hi").await;
        crate::skills::set_chat_skill(&st.db().unwrap(), conv, None).unwrap();
        turn(&st, conv, "hi").await;
        for req in rec.requests.lock().unwrap().iter() {
            assert!(req.tools.iter().all(|t| t.name != "contrast_check"));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contrast_check_runs_in_code() {
        use dn_agent::ToolExecutor;
        let tools = RunTools {
            base: Arc::new(Calc(false)),
            artifacts: None,
            contrast: true,
            data: None,
        };
        let ctx = dn_agent::ToolContext {
            agent_id: "assistant.default".into(),
            caller: "chat.reply".into(),
            conversation_id: None,
            run_id: Id::new(),
            step: 0,
            cancel: Default::default(),
            allowed_tools: vec![],
        };
        let call = |args: serde_json::Value| dn_core::chat::ToolCall {
            id: "c1".into(),
            name: "contrast_check".into(),
            arguments: args,
        };
        let out = tools
            .execute(
                &call(serde_json::json!({"foreground": "#777777", "background": "white"})),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        let text = match &out.content[..] {
            [dn_core::chat::ContentPart::Text { text }] => text.clone(),
            other => panic!("{other:?}"),
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["ratio"], 4.47);
        assert_eq!(v["aa_normal_text"], false);
        let bad = tools
            .execute(
                &call(serde_json::json!({"foreground": "#zzz", "background": "#fff"})),
                &ctx,
            )
            .await
            .unwrap();
        assert!(bad.is_error);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_call_outside_the_skill_is_refused_not_run() {
        let rec = Arc::new(Recorder {
            call_tool: true,
            ..Default::default()
        });
        // `calc` isn't a tool the app can resolve: listing it doesn't make it available.
        let (st, conv) = skill_state(rec.clone(), true, &["calc"]);
        turn(&st, conv, "make a page").await;
        assert!(rec.requests.lock().unwrap()[0].tools.is_empty());
        assert!(crate::artifacts::list(&st, conv).unwrap().is_empty());
        let msgs = store::list_messages(&st.db().unwrap(), conv).unwrap();
        let tool = msgs
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("tool row");
        assert!(tool.text().contains("isn't available"), "{}", tool.text());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_chat_whose_skill_is_gone_fails_clearly_and_writes_nothing() {
        let rec = Arc::new(Recorder::default());
        let (st, conv) = skill_state(rec.clone(), true, &[]);
        crate::skills::set_chat_skill(&st.db().unwrap(), conv, Some("removed")).unwrap();
        let (_e, s) = sink();
        let err = send(&st, conv, "hi".into(), s).unwrap_err();
        assert_eq!(err.code, "skill_unavailable");
        assert!(err.message.contains("\"removed\""), "{}", err.message);
        assert!(
            store::list_messages(&st.db().unwrap(), conv)
                .unwrap()
                .is_empty()
        );
        assert!(rec.requests.lock().unwrap().is_empty());
        // The chat isn't stuck: choosing no skill sends normally.
        crate::skills::set_chat_skill(&st.db().unwrap(), conv, None).unwrap();
        turn(&st, conv, "hi").await;
        assert_eq!(rec.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_answer_remembers_the_skill_it_was_written_with() {
        let rec = Arc::new(Recorder::default());
        let (st, conv) = skill_state(rec, true, &["calc"]);
        let (first, _) = turn(&st, conv, "one").await;
        assert_eq!(first.skill.as_deref(), Some("Notes"));

        // No skill for the next answer: it has none, and the first keeps its own.
        crate::skills::set_chat_skill(&st.db().unwrap(), conv, None).unwrap();
        let (second, _) = turn(&st, conv, "two").await;
        assert_eq!(second.skill, None);
        let answers = crate::skills::answer_skills(&st.db().unwrap(), conv).unwrap();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].message_id, first.assistant_message.id);
        assert_eq!(
            (answers[0].name.as_str(), answers[0].label.as_str()),
            ("notes", "Notes")
        );

        // Regenerating with the skill back on records the new answer.
        crate::skills::set_chat_skill(&st.db().unwrap(), conv, Some("notes")).unwrap();
        let (events, s) = sink();
        let again = regenerate(&st, conv, s).unwrap();
        wait_idle(&st, conv).await;
        drop(events);
        assert_eq!(again.skill.as_deref(), Some("Notes"));
        let ids: Vec<Id> = crate::skills::answer_skills(&st.db().unwrap(), conv)
            .unwrap()
            .into_iter()
            .map(|a| a.message_id)
            .collect();
        assert!(ids.contains(&again.assistant_message.id));

        // Deleting the chat's data forgets them.
        crate::skills::forget_chat(&st.db().unwrap(), conv).unwrap();
        assert!(
            crate::skills::answer_skills(&st.db().unwrap(), conv)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_a_skill_nothing_changes() {
        let rec = Arc::new(Recorder::default());
        let (st, conv) = skill_state(rec.clone(), true, &["calc"]);
        crate::skills::set_chat_skill(&st.db().unwrap(), conv, None).unwrap();
        turn(&st, conv, "hi").await;
        let reqs = rec.requests.lock().unwrap();
        let names: Vec<&str> = reqs[0].tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"calc") && names.contains(&"artifact_create"),
            "{names:?}"
        );
        assert!(!system_text(&reqs[0]).contains("bullet points"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_less_models_get_no_artifacts() {
        let rec = Arc::new(Recorder {
            call_tool: true,
            ..Default::default()
        });
        let st = artifact_state("local:plain", rec.clone(), false);
        let conv = st.create_conversation().unwrap();
        let (_, evs) = turn(&st, conv.id, "make a page").await;
        // No text format: the `<artifact>` text in the answer creates nothing...
        assert!(
            !evs.iter()
                .any(|e| matches!(e, ChatEvent::ArtifactChanged { .. })),
            "{evs:?}"
        );
        assert!(crate::artifacts::list(&st, conv.id).unwrap().is_empty());
        // ...no artifact tools are offered, and no artifact instructions sent...
        let reqs = rec.requests.lock().unwrap();
        assert!(
            !reqs[0]
                .tools
                .iter()
                .any(|t| t.name.starts_with("artifact_")),
            "no artifact tools for a tool-less model"
        );
        assert!(!system_text(&reqs[0]).contains("<artifact"));
        // ...and a stray artifact tool call is refused, not run.
        let msgs = store::list_messages(&st.db().unwrap(), conv.id).unwrap();
        let tool = msgs
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("tool result row");
        assert!(tool.text().contains("isn't available"), "{}", tool.text());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_only_chat_model_fills_the_empty_slot() {
        let mut models = crate::models::ModelRegistry::default();
        models.register(
            "local:dev-fake".parse().unwrap(),
            Arc::new(FakeChatModel {
                delay: Duration::from_millis(0),
            }),
        );
        let mut s = AppState::in_memory().unwrap();
        s.models = models;
        let st = Arc::new(s);
        assert!(
            st.settings().unwrap().slot("chat").is_none(),
            "nothing chosen yet"
        );
        let conv = st.create_conversation().unwrap();
        let (_e, s1) = sink();
        send(&st, conv.id, "hi".into(), s1).unwrap();
        wait_idle(&st, conv.id).await;
        assert_eq!(
            st.settings()
                .unwrap()
                .slot("chat")
                .map(|m| m.to_string())
                .as_deref(),
            Some("local:dev-fake"),
            "the first suitable model became the saved choice"
        );
    }

    /// An existing user with several chat models and no choice: never an automatic pick; the
    /// turn asks for a choice and the slot stays empty (#219).
    #[tokio::test(flavor = "multi_thread")]
    async fn several_chat_models_and_no_choice_ask_for_one() {
        let dir = std::env::temp_dir().join(format!("dn-chat-several-{}", Id::new()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tiny.gguf"), b"12345").unwrap();
        std::fs::write(dir.join("tiny.gguf.sha256"), "aa").unwrap();
        let catalog = dn_models::Catalog {
            version: 1,
            models: vec![dn_models::CatalogEntry {
                recommended: None,
                id: "tiny".into(),
                name: "Tiny".into(),
                repo: "o/r".into(),
                revision: "x".into(),
                file: "tiny.gguf".into(),
                size: 5,
                sha256: "aa".into(),
                quant: "Q4_K_M".into(),
                params_b: 0.1,
                capabilities: vec![dn_core::model::Capability::Chat],
                license: "mit".into(),
                notes: String::new(),
                tool_support: None,
            }],
        };
        let mut models = crate::models::ModelRegistry::default();
        models.register(
            "local:dev-fake".parse().unwrap(),
            Arc::new(FakeChatModel {
                delay: Duration::from_millis(0),
            }),
        );
        models.local = Some(Arc::new(crate::local::LocalModels::new(
            dir.clone(),
            catalog,
            dn_models::Machine::cpu_only(16 << 30),
        )));
        let mut s = AppState::in_memory().unwrap();
        s.models = models;
        let st = Arc::new(s);
        let conv = st.create_conversation().unwrap();
        let (_e, s1) = sink();
        let err = send(&st, conv.id, "hi".into(), s1).unwrap_err();
        assert_eq!(err.code, "model_choice_required", "{}", err.message);
        assert!(st.settings().unwrap().slot("chat").is_none());
        assert_eq!(count(&st, conv.id), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_chosen_model_that_wont_fit_says_so() {
        let dir = std::env::temp_dir().join(format!("dn-chat-fit-{}", Id::new()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("huge.gguf"), b"12345").unwrap();
        std::fs::write(dir.join("huge.gguf.sha256"), "aa").unwrap();
        let catalog = dn_models::Catalog {
            version: 1,
            models: vec![dn_models::CatalogEntry {
                recommended: None,
                id: "huge".into(),
                name: "Huge Model".into(),
                repo: "o/r".into(),
                revision: "x".into(),
                file: "huge.gguf".into(),
                size: 5,
                sha256: "aa".into(),
                quant: "Q4_K_M".into(),
                params_b: 999.0,
                capabilities: vec![dn_core::model::Capability::Chat],
                license: "mit".into(),
                notes: String::new(),
                tool_support: None,
            }],
        };
        // 1 MiB of RAM: the 999B model can't fit.
        let local = crate::local::LocalModels::new(
            dir.clone(),
            catalog,
            dn_models::Machine::cpu_only(1 << 20),
        );
        assert!(local.wont_fit("huge"));
        let mut s = AppState::in_memory().unwrap();
        s.models.local = Some(Arc::new(local));
        let st = Arc::new(s);
        let mut settings = crate::settings::Settings::default();
        settings.slots.insert("chat".into(), "local:huge".into());
        st.save_settings(&settings).unwrap();
        let conv = st.create_conversation().unwrap();
        let (_e, s1) = sink();
        let err = send(&st, conv.id, "hi".into(), s1).unwrap_err();
        assert!(
            err.message.contains("won't fit on this computer"),
            "{}",
            err.message
        );
        assert_eq!(count(&st, conv.id), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_first_model_fills_the_slot_and_a_failed_load_says_so() {
        // The only ready model is a downloaded local model whose file isn't a valid GGUF: it
        // fills the empty slot (#219), and llama.cpp then fails to load it. No other model is
        // tried.
        let dir = std::env::temp_dir().join(format!("dn-chat-badload-{}", Id::new()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tiny.gguf"), b"12345").unwrap();
        std::fs::write(dir.join("tiny.gguf.sha256"), "aa").unwrap();
        let catalog = dn_models::Catalog {
            version: 1,
            models: vec![dn_models::CatalogEntry {
                recommended: None,
                id: "tiny".into(),
                name: "Tiny".into(),
                repo: "o/r".into(),
                revision: "x".into(),
                file: "tiny.gguf".into(),
                size: 5,
                sha256: "aa".into(),
                quant: "Q4_K_M".into(),
                params_b: 0.1,
                capabilities: vec![dn_core::model::Capability::Chat],
                license: "mit".into(),
                notes: String::new(),
                tool_support: None,
            }],
        };
        let local = crate::local::LocalModels::new(
            dir.clone(),
            catalog,
            dn_models::Machine::cpu_only(16 << 30),
        );
        assert!(local.is_ready("tiny") && !local.wont_fit("tiny"));
        let mut s = AppState::in_memory().unwrap();
        // No other source (not even the dev fake model in feature builds).
        s.models = crate::models::ModelRegistry::default();
        s.models.local = Some(Arc::new(local));
        let st = Arc::new(s);
        let conv = st.create_conversation().unwrap();
        let (_e, s1) = sink();
        // As in the app: chat turns run on a blocking thread (loading uses block_on).
        let st2 = Arc::clone(&st);
        let conv_id = conv.id;
        let err = tokio::task::spawn_blocking(move || send(&st2, conv_id, "hi".into(), s1))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "model_unavailable", "{}", err.message);
        assert_eq!(
            st.settings().unwrap().slots.get("chat").map(String::as_str),
            Some("local:tiny"),
            "the slot keeps the user's only model"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn titles() {
        assert_eq!(title_from("\n  hi there \nmore"), "hi there");
        assert_eq!(title_from(&"x".repeat(60)).chars().count(), 49);
        assert_eq!(title_from("   "), DEFAULT_TITLE);
    }
}
