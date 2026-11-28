//! The agent loop (T-AGT-02, ADR-0005): model → tool calls → results → model, until the
//! model answers without tools, the step limit is hit, or the run is cancelled.

use crate::{CancelToken, ToolContext, ToolExecutor, ToolOutput};
use dn_core::chat::{
    ChatMessage, ChatModel, ChatRequest, ContentPart, EventStream, FinishReason, Reasoning, Role,
    StreamEvent, ToolCall, Usage,
};
use dn_core::model::ModelRef;
use dn_core::run::{RunMeter, RunResult, StopReason};
use dn_core::{CoreError, RecoveryAction};
use tracing::Instrument;

/// Settings for one run, usually built from an `AgentManifest` + the resolved model.
#[derive(Clone, Debug)]
pub struct LoopConfig {
    pub agent_id: String,
    /// Caller tag for model calls (drives the reasoning policy), e.g. `chat.reply`.
    pub caller: String,
    pub model: ModelRef,
    /// Maximum number of model calls in this run.
    pub max_steps: u32,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub reasoning: Reasoning,
    pub conversation_id: Option<dn_core::Id>,
}

impl LoopConfig {
    pub fn new(agent_id: impl Into<String>, model: ModelRef) -> Self {
        Self {
            agent_id: agent_id.into(),
            caller: "chat.reply".into(),
            model,
            max_steps: 8,
            max_tokens: None,
            temperature: None,
            reasoning: Reasoning::Auto,
            conversation_id: None,
        }
    }

    pub fn from_manifest(m: &crate::AgentManifest, model: ModelRef) -> Self {
        Self {
            agent_id: m.id.clone(),
            caller: m.policies.caller.clone(),
            model,
            max_steps: m.policies.max_steps,
            max_tokens: m.policies.max_tokens,
            temperature: m.policies.temperature,
            reasoning: m.policies.reasoning,
            conversation_id: None,
        }
    }
}

/// Progress events for the UI, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    StepStarted {
        step: u32,
    },
    TextDelta {
        step: u32,
        text: String,
    },
    ReasoningDelta {
        step: u32,
        text: String,
    },
    ToolCallStarted {
        step: u32,
        call: ToolCall,
    },
    ToolCallFinished {
        step: u32,
        call_id: String,
        output: ToolOutput,
    },
    StepFinished {
        step: u32,
        finish_reason: FinishReason,
        usage: Usage,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunOutcome {
    /// Messages produced by this run (assistant turns and tool results), to append to
    /// the conversation. Always consistent: every tool call has its result.
    pub messages: Vec<ChatMessage>,
    /// What the run did (ADR-0020 §8). `result.run_id` is the `ToolContext::run_id` every
    /// tool call of this run received. Usage is kept on cancellation; a call cancelled
    /// mid-stream reports nothing, so its tokens are not included.
    pub result: RunResult,
    /// Text of the last assistant turn.
    pub final_text: String,
}

/// Feeds a progress event into a [`RunMeter`]: a surface that needs a result for a run
/// that ended in `Err` (where [`run_loop`] returns no outcome) meters its own events and
/// finishes with [`StopReason::Failed`].
pub fn observe_event(meter: &mut RunMeter, ev: &AgentEvent) {
    match ev {
        AgentEvent::StepStarted { .. } => meter.step(),
        AgentEvent::TextDelta { text, .. } => {
            meter.observe(&StreamEvent::TextDelta { text: text.clone() })
        }
        AgentEvent::ReasoningDelta { text, .. } => {
            meter.observe(&StreamEvent::ReasoningDelta { text: text.clone() })
        }
        AgentEvent::StepFinished {
            finish_reason,
            usage,
            ..
        } => meter.observe(&StreamEvent::Done {
            finish_reason: *finish_reason,
            usage: *usage,
        }),
        AgentEvent::ToolCallStarted { .. } | AgentEvent::ToolCallFinished { .. } => {}
    }
}

/// Runs the loop. `history` is the conversation so far (system prompt included).
/// Returns `Err` only for failures that end the run (model errors, `Err` from a tool);
/// cancellation is a normal outcome (`StopReason::Cancelled`) so partial work can be kept.
pub async fn run_loop(
    model: &dyn ChatModel,
    tools: &dyn ToolExecutor,
    cfg: &LoopConfig,
    history: Vec<ChatMessage>,
    cancel: &CancelToken,
    on_event: &mut (dyn FnMut(AgentEvent) + Send),
) -> Result<RunOutcome, CoreError> {
    let specs = tools.tools();
    // One id per run: the idempotency scope for tool calls (see `ToolContext::run_id`).
    let run_id = dn_core::Id::new();
    // Seeded from the conversation so far: a backend that reuses `call_0` across runs must
    // not put a duplicate tool-call id into the history (Anthropic rejects those).
    let mut seen_ids: std::collections::HashSet<String> = history
        .iter()
        .flat_map(|m| m.tool_calls.iter().map(|c| c.id.clone()))
        .collect();
    let mut messages = history;
    let start = messages.len();
    let mut meter = RunMeter::start(run_id, cfg.caller.clone(), cfg.model.clone());
    let mut final_text = String::new();
    let outcome = |messages: Vec<ChatMessage>, meter: RunMeter, stop, final_text| {
        let result = meter.finish(stop);
        tracing::info!(
            run_id = %result.run_id,
            caller = %result.caller,
            model = %result.model,
            stop = ?result.stop,
            steps = result.steps,
            input_tokens = result.usage.input_tokens,
            output_tokens = result.usage.output_tokens,
            cached_input_tokens = result.usage.cached_input_tokens,
            wall_ms = result.wall_ms,
            first_token_ms = ?result.first_token_ms,
            "run finished"
        );
        RunOutcome {
            messages: messages[start..].to_vec(),
            result,
            final_text,
        }
    };

    for step in 0..cfg.max_steps {
        if cancel.is_cancelled() {
            return Ok(outcome(messages, meter, StopReason::Cancelled, final_text));
        }
        meter.step();
        on_event(AgentEvent::StepStarted { step });
        let req = ChatRequest {
            model: cfg.model.clone(),
            messages: messages.clone(),
            tools: specs.clone(),
            max_tokens: cfg.max_tokens,
            temperature: cfg.temperature,
            response_schema: None,
            reasoning: cfg.reasoning,
            caller: cfg.caller.clone(),
        };
        // Names, counts and timings only: never message or tool content (traces stay local,
        // but they're still not the place for what the user wrote).
        let call_span = tracing::info_span!(
            "model_call",
            caller = %cfg.caller,
            model = %cfg.model,
            step,
            finish = tracing::field::Empty,
            input_tokens = tracing::field::Empty,
            output_tokens = tracing::field::Empty,
        );
        let mut stream = model.chat(req);
        let mut text = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut finish = FinishReason::Stop;
        let mut got_done = false;
        let mut cancelled = false;
        loop {
            let ev = async {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => None,
                    ev = next(&mut stream) => Some(ev),
                }
            }
            .instrument(call_span.clone())
            .await;
            let Some(ev) = ev else {
                cancelled = true;
                break;
            };
            if let Some(Ok(e)) = &ev {
                meter.observe(e);
            }
            match ev {
                None => break,
                Some(Err(e)) => {
                    tracing::warn!(parent: &call_span, error = %e, "model call failed");
                    return Err(e);
                }
                Some(Ok(StreamEvent::TextDelta { text: t })) => {
                    text.push_str(&t);
                    on_event(AgentEvent::TextDelta { step, text: t });
                }
                Some(Ok(StreamEvent::ReasoningDelta { text: t })) => {
                    on_event(AgentEvent::ReasoningDelta { step, text: t });
                }
                Some(Ok(StreamEvent::ToolCall { call })) => calls.push(call),
                Some(Ok(StreamEvent::Done {
                    finish_reason,
                    usage: u,
                })) => {
                    finish = finish_reason;
                    got_done = true;
                    call_span.record("finish", tracing::field::debug(finish_reason));
                    call_span.record("input_tokens", u.input_tokens);
                    call_span.record("output_tokens", u.output_tokens);
                    on_event(AgentEvent::StepFinished {
                        step,
                        finish_reason,
                        usage: u,
                    });
                }
            }
        }
        drop(stream); // cancels the model call if it is still running
        if cancelled {
            // Keep the partial answer, but not half-announced tool calls.
            if !text.is_empty() {
                messages.push(ChatMessage::text(Role::Assistant, text.clone()));
            }
            return Ok(outcome(messages, meter, StopReason::Cancelled, text));
        }
        if !got_done {
            // A `ChatModel` must end every stream with `Done`; anything else is a cut-off.
            return Err(CoreError::ModelUnavailable {
                user_message: "The model stopped before finishing its answer.".into(),
                developer_message: format!(
                    "agent `{}` step {step}: stream ended without Done",
                    cfg.agent_id
                ),
                recovery: vec![RecoveryAction::Retry {
                    after_seconds: None,
                }],
            });
        }
        if finish == FinishReason::Length {
            // Out of tokens: tool calls may be half-formed, so they never run (and are not
            // kept, so history has no calls without results).
            calls.clear();
        }
        // Model-provided ids aren't unique (local models send "" or reuse `call_0`), and
        // tool ledgers key on (run_id, call.id): make them unique within the run. The
        // rewritten call is the one stored, executed and answered, so results line up.
        for (i, c) in calls.iter_mut().enumerate() {
            if c.id.is_empty() || !seen_ids.insert(c.id.clone()) {
                // Keep a provider suffix after `~` (Gemini's thought signature rides there).
                let suffix =
                    c.id.find('~')
                        .map(|i| c.id[i..].to_string())
                        .unwrap_or_default();
                let mut base = format!("call_{step}_{i}");
                while !seen_ids.insert(format!("{base}{suffix}")) {
                    base.push('_');
                }
                c.id = format!("{base}{suffix}");
            }
        }
        final_text = text.clone();
        messages.push(ChatMessage {
            role: Role::Assistant,
            content: if text.is_empty() {
                vec![]
            } else {
                vec![ContentPart::Text { text }]
            },
            tool_calls: calls.clone(),
            tool_call_id: None,
        });
        if calls.is_empty() {
            let stop = if finish == FinishReason::Length {
                StopReason::Length
            } else {
                StopReason::Completed
            };
            return Ok(outcome(messages, meter, stop, final_text));
        }
        let ctx = ToolContext {
            agent_id: cfg.agent_id.clone(),
            caller: cfg.caller.clone(),
            conversation_id: cfg.conversation_id,
            run_id,
            step,
            cancel: cancel.clone(),
            allowed_tools: Vec::new(),
        };
        for (i, call) in calls.iter().enumerate() {
            on_event(AgentEvent::ToolCallStarted {
                step,
                call: call.clone(),
            });
            let tool_span = tracing::info_span!(
                "tool_call",
                caller = %cfg.caller,
                step,
                tool = %call.name,
                call_id = %call.id,
                is_error = tracing::field::Empty,
            );
            let result = async {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(CoreError::Cancelled),
                    r = tools.execute(call, &ctx) => r,
                }
            }
            .instrument(tool_span.clone())
            .await;
            match &result {
                Ok(o) => {
                    tool_span.record("is_error", o.is_error);
                }
                Err(e) => tracing::warn!(parent: &tool_span, error = %e, "tool call ended the run"),
            }
            let output = match result {
                Ok(o) => o,
                Err(CoreError::Cancelled) => {
                    // Close the turn: every call gets a result so history stays valid.
                    for c in &calls[i..] {
                        messages.push(tool_message(
                            c,
                            &ToolOutput::error("Cancelled by the user."),
                        ));
                    }
                    return Ok(outcome(messages, meter, StopReason::Cancelled, final_text));
                }
                Err(e) => return Err(e),
            };
            messages.push(tool_message(call, &output));
            on_event(AgentEvent::ToolCallFinished {
                step,
                call_id: call.id.clone(),
                output,
            });
        }
    }
    Ok(outcome(messages, meter, StopReason::StepLimit, final_text))
}

fn tool_message(call: &ToolCall, out: &ToolOutput) -> ChatMessage {
    let mut content = out.content.clone();
    if out.is_error {
        // dn-core messages have no error flag; make failures unmistakable to the model.
        content.insert(
            0,
            ContentPart::Text {
                text: "Error: ".into(),
            },
        );
    }
    ChatMessage {
        role: Role::Tool,
        content,
        tool_calls: vec![],
        tool_call_id: Some(call.id.clone()),
    }
}

async fn next(s: &mut EventStream) -> Option<Result<StreamEvent, CoreError>> {
    std::future::poll_fn(|cx| s.as_mut().poll_next(cx)).await
}
