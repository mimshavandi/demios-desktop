//! The agent loop (T-AGT-02, ADR-0005): model → tool calls → results → model, until the
//! model answers without tools, the step limit is hit, or the run is cancelled.

use crate::{CancelToken, ToolContext, ToolExecutor, ToolOutput};
use dn_core::chat::{
    ChatMessage, ChatModel, ChatRequest, ContentPart, EventStream, FinishReason, Reasoning, Role,
    StreamEvent, ToolCall, Usage,
};
use dn_core::model::ModelRef;
use dn_core::{CoreError, RecoveryAction};

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
    /// The provider's own web search, when the person chose it in Settings (#369).
    pub native_web_search: Option<dn_core::chat::NativeWebSearch>,
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
            native_web_search: None,
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
            native_web_search: None,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The model answered without asking for tools.
    Completed,
    /// The model hit its output token limit.
    Length,
    /// `max_steps` model calls were made and the model still wanted tools. The tools of
    /// the last step *did* run, so history ends with tool results and no final answer:
    /// the UI should offer "continue" (another run picks up from those results).
    StepLimit,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunOutcome {
    /// The `ToolContext::run_id` every tool call of this run received.
    pub run_id: dn_core::Id,
    /// Messages produced by this run (assistant turns and tool results), to append to
    /// the conversation. Always consistent: every tool call has its result.
    pub messages: Vec<ChatMessage>,
    pub stop: StopReason,
    pub steps: u32,
    /// Sum over all model calls that reported usage. Kept on cancellation; a call
    /// cancelled mid-stream reports nothing, so its tokens are not included.
    pub usage: Usage,
    /// Text of the last assistant turn.
    pub final_text: String,
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
    let mut usage = Usage::default();
    let mut final_text = String::new();
    let outcome = |messages: Vec<ChatMessage>, stop, steps, usage, final_text| RunOutcome {
        run_id,
        messages: messages[start..].to_vec(),
        stop,
        steps,
        usage,
        final_text,
    };

    for step in 0..cfg.max_steps {
        if cancel.is_cancelled() {
            return Ok(outcome(
                messages,
                StopReason::Cancelled,
                step,
                usage,
                final_text,
            ));
        }
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
            native_web_search: cfg.native_web_search,
        };
        let mut stream = model.chat(req);
        let mut text = String::new();
        let mut web_citations = Vec::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut finish = FinishReason::Stop;
        let mut got_done = false;
        let mut cancelled = false;
        loop {
            let ev = tokio::select! {
                biased;
                _ = cancel.cancelled() => { cancelled = true; break; }
                ev = next(&mut stream) => ev,
            };
            match ev {
                None => break,
                Some(Err(e)) => return Err(e),
                Some(Ok(StreamEvent::TextDelta { text: t })) => {
                    text.push_str(&t);
                    on_event(AgentEvent::TextDelta { step, text: t });
                }
                Some(Ok(StreamEvent::ReasoningDelta { text: t })) => {
                    on_event(AgentEvent::ReasoningDelta { step, text: t });
                }
                Some(Ok(StreamEvent::ToolCall { call })) => calls.push(call),
                Some(Ok(StreamEvent::WebCitations { citations })) => {
                    web_citations.extend(citations);
                }
                Some(Ok(StreamEvent::Done {
                    finish_reason,
                    usage: u,
                })) => {
                    finish = finish_reason;
                    got_done = true;
                    usage.input_tokens += u.input_tokens;
                    usage.output_tokens += u.output_tokens;
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
            return Ok(outcome(
                messages,
                StopReason::Cancelled,
                step + 1,
                usage,
                text,
            ));
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
            web_citations,
        });
        if calls.is_empty() {
            let stop = if finish == FinishReason::Length {
                StopReason::Length
            } else {
                StopReason::Completed
            };
            return Ok(outcome(messages, stop, step + 1, usage, final_text));
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
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(CoreError::Cancelled),
                r = tools.execute(call, &ctx) => r,
            };
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
                    return Ok(outcome(
                        messages,
                        StopReason::Cancelled,
                        step + 1,
                        usage,
                        final_text,
                    ));
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
    Ok(outcome(
        messages,
        StopReason::StepLimit,
        cfg.max_steps,
        usage,
        final_text,
    ))
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
        web_citations: vec![],
    }
}

async fn next(s: &mut EventStream) -> Option<Result<StreamEvent, CoreError>> {
    std::future::poll_fn(|cx| s.as_mut().poll_next(cx)).await
}
