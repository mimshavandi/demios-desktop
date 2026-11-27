//! The context assembler (ADR-0020 §2): every model call's prompt, in a fixed slot order,
//! within the model's real budget, with a record of what was left out.
//!
//! **Layout** (§2.2 as amended; byte-stable up to the conversation, §2.3):
//! 1. One system message: the system parts, the skills index, the core profile and
//!    project knowledge (untrusted blocks with the notice), in that order.
//! 2. The running summary, if any, as a second system message.
//! 3. The stored conversation. Tool results are wrapped as untrusted blocks named after
//!    their tool (`tool:<name>`, with the call id) at render time, so the store keeps them as
//!    the tool returned them.
//! 4. The new input as the last user message, preceded by retrieved items (untrusted
//!    blocks with the notice) and followed by the recap. Those two are for this turn only:
//!    the store keeps just what the person wrote.
//!
//! Tools are sorted by name. Nothing here reads a clock or makes ids, so the same parts
//! always give the same bytes.

use crate::estimate::{Estimator, request_units};
use crate::policy::{Overflow, Policy};
use dn_core::CoreError;
use dn_core::chat::{ChatMessage, ChatModel, ChatRequest, ContentPart, Role, ToolSpec};
use dn_core::run::{ContextUse, DropAction, Dropped, Slot, TokenCount};
use dn_core::untrusted::{self, SourceRef};

/// A piece of untrusted text and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub source: SourceRef,
    pub text: String,
}

/// Everything a surface wants in one model call, slot by slot.
#[derive(Clone, Debug, Default)]
pub struct Parts {
    /// The system slot: the agent prompt, shared rules, the chat's project instructions and
    /// skill, in order. Written by Demios or the person, so not quoted.
    pub system: Vec<String>,
    pub tools: Vec<ToolSpec>,
    /// The skills index. Not quoted, so only skills the person installed or Demios ships;
    /// an add-on's skill text goes in as an [`Item`].
    pub skills: Option<String>,
    /// The core profile (memory). Not quoted, so only items the person stated or
    /// confirmed (ADR-0020 §4.1); anything from third-party content goes in as an [`Item`].
    pub profile: Option<String>,
    /// Project or add-on knowledge, in priority order (dropped from the end).
    pub knowledge: Vec<Item>,
    /// The running summary of earlier turns. A model wrote it from a conversation that may
    /// hold tool and retrieved output, so it's quoted as untrusted data.
    pub summary: Option<String>,
    /// The stored conversation, oldest first, without the new input.
    pub turns: Vec<ChatMessage>,
    /// Items retrieved for this turn, best first (dropped from the end).
    pub retrieved: Vec<Item>,
    /// The new input (a user message), or `None` when a step continues after tool results.
    pub input: Option<ChatMessage>,
    /// The one-line task recap with the date and other changing values.
    pub recap: Option<String>,
}

/// The request to send and how it was put together.
#[derive(Clone, Debug)]
pub struct Assembled {
    pub request: ChatRequest,
    pub context: ContextUse,
    pub dropped: Vec<Dropped>,
    /// `request.messages[..stable_through]` only grows from one turn to the next (except
    /// after a compaction): what a cache can reuse.
    pub stable_through: usize,
    /// The estimate's measure, for [`Estimator::calibrate`] once the provider reports.
    pub units: f64,
}

/// Why a prompt couldn't be assembled.
#[derive(Debug, thiserror::Error)]
pub enum AssembleError {
    /// Nobody knows the model's context size; the person sets it (design: ContextSize).
    #[error("Demios doesn't know how much text {model} can read at once")]
    UnknownContext { model: String },
    /// It doesn't fit even with everything droppable left out.
    #[error("this is too long for {model}: {needed} tokens, and it reads {budget} here")]
    TooLong {
        model: String,
        needed: u32,
        budget: u32,
    },
    #[error(transparent)]
    Count(#[from] CoreError),
}

impl From<AssembleError> for CoreError {
    fn from(e: AssembleError) -> Self {
        match e {
            AssembleError::UnknownContext { model } => CoreError::ModelUnavailable {
                user_message: format!(
                    "Demios doesn't know how much text {model} can read at once, so it can't \
                     fit this chat to it."
                ),
                developer_message: format!("{model}: context size unknown"),
                // The "Set context size" action comes with its screens (R1 PR 4).
                recovery: vec![],
            },
            AssembleError::TooLong {
                model,
                needed,
                budget,
            } => CoreError::ModelUnavailable {
                user_message: format!("This chat is too long for {model}."),
                developer_message: format!("{needed} input tokens, budget {budget}"),
                recovery: vec![],
            },
            AssembleError::Count(e) => e,
        }
    }
}

fn section(items: &[Item]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let blocks: Vec<String> = items
        .iter()
        .map(|i| untrusted::block(&i.source, &i.text))
        .collect();
    Some(format!("{}\n\n{}", untrusted::NOTICE, blocks.join("\n\n")))
}

/// The request for `parts` as they stand, and its stable boundary.
fn render(parts: &Parts, template: &ChatRequest) -> (ChatRequest, usize) {
    let mut messages = Vec::new();
    let system: Vec<String> = parts
        .system
        .iter()
        .cloned()
        .chain(parts.skills.clone())
        .chain(parts.profile.clone())
        .chain(section(&parts.knowledge))
        .filter(|s| !s.trim().is_empty())
        .collect();
    if !system.is_empty() {
        messages.push(ChatMessage::text(Role::System, system.join("\n\n")));
    }
    if let Some(s) = parts.summary.as_ref().filter(|s| !s.trim().is_empty()) {
        let block = untrusted::block(&SourceRef::new("summary").attr("of", "earlier messages"), s);
        messages.push(ChatMessage::text(
            Role::System,
            format!(
                "Summary of the earlier part of this conversation.\n{}\n\n{block}",
                untrusted::NOTICE
            ),
        ));
    }
    messages.extend(quote_tool_results(&parts.turns));
    let stable_through = messages.len();
    if let Some(input) = &parts.input {
        let mut m = input.clone();
        let mut content = Vec::new();
        if let Some(r) = section(&parts.retrieved) {
            content.push(ContentPart::Text { text: r });
        }
        content.append(&mut m.content);
        if let Some(recap) = &parts.recap {
            content.push(ContentPart::Text {
                text: recap.clone(),
            });
        }
        m.content = content;
        messages.push(m);
    }
    let mut tools = parts.tools.clone();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let req = ChatRequest {
        messages,
        tools,
        ..template.clone()
    };
    (req, stable_through)
}

/// The turns with each tool result's text in an untrusted block. Tool output (MCP servers,
/// the web, files) is someone else's text (ADR-0020 §2.4).
fn quote_tool_results(turns: &[ChatMessage]) -> Vec<ChatMessage> {
    let names: std::collections::HashMap<&str, &str> = turns
        .iter()
        .flat_map(|m| m.tool_calls.iter())
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();
    turns
        .iter()
        .map(|m| {
            if m.role != Role::Tool {
                return m.clone();
            }
            let id = m.tool_call_id.clone().unwrap_or_default();
            let name = names.get(id.as_str()).copied().unwrap_or("unknown");
            let source = SourceRef::new(format!("tool:{name}")).attr("call", id.clone());
            let mut q = m.clone();
            q.content = m
                .content
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => ContentPart::Text {
                        text: untrusted::block(&source, text),
                    },
                    other => other.clone(),
                })
                .collect();
            q
        })
        .collect()
}

/// The id a dropped item is recorded under.
fn source_id(i: &Item) -> String {
    i.source.id.clone()
}

/// Puts `parts` together for `model` under `policy`. `template` carries the request's
/// other fields (model, max_tokens, reasoning, caller); its messages and tools are
/// replaced. `estimator` and `estimate_key` are used when the model can't count.
pub async fn assemble(
    model: &dyn ChatModel,
    policy: &Policy,
    parts: Parts,
    template: &ChatRequest,
    estimator: &Estimator,
    estimate_key: &str,
) -> Result<Assembled, AssembleError> {
    let name = template.model.to_string();
    let limits = model.limits(&template.model).await;
    let (Some(context), Some(source)) = (limits.context_tokens, limits.source) else {
        return Err(AssembleError::UnknownContext { model: name });
    };
    let budget = policy.budget(context, source, template.max_tokens);
    let count = |req: &ChatRequest| -> Result<(u32, TokenCount), AssembleError> {
        match model.count_tokens(req) {
            Some(n) => Ok((n?, TokenCount::ModelTokenizer)),
            None => Ok((estimator.estimate(estimate_key, req), TokenCount::Estimate)),
        }
    };

    let mut parts = parts;
    let mut dropped = Vec::new();
    let (mut req, mut stable_through) = render(&parts, template);
    let (mut used, mut counted_by) = count(&req)?;
    loop {
        if used <= budget {
            return Ok(Assembled {
                units: request_units(&req),
                request: req,
                context: ContextUse {
                    budget_tokens: budget,
                    used_tokens: used,
                    counted_by,
                    compacted: false,
                },
                dropped,
                stable_through,
            });
        }
        // Leave out the last item of the lowest-priority droppable slot that has any.
        let Some(slot) = policy.droppable.iter().copied().find(|s| has(&parts, *s)) else {
            return match policy.overflow {
                Overflow::Fail => Err(AssembleError::TooLong {
                    model: name,
                    needed: used,
                    budget,
                }),
            };
        };
        let source = drop_one(&mut parts, slot);
        (req, stable_through) = render(&parts, template);
        let (now, how) = count(&req)?;
        dropped.push(Dropped {
            slot,
            source,
            tokens: used.saturating_sub(now),
            action: DropAction::Dropped,
        });
        (used, counted_by) = (now, how);
    }
}

fn has(p: &Parts, s: Slot) -> bool {
    match s {
        Slot::Retrieved => !p.retrieved.is_empty(),
        Slot::Knowledge => !p.knowledge.is_empty(),
        Slot::Profile => p.profile.is_some(),
        Slot::Skills => p.skills.is_some(),
        _ => false,
    }
}

/// Removes one item from `slot` and returns its id.
fn drop_one(p: &mut Parts, slot: Slot) -> String {
    let id = match slot {
        Slot::Retrieved => p.retrieved.pop().map(|i| source_id(&i)),
        Slot::Knowledge => p.knowledge.pop().map(|i| source_id(&i)),
        Slot::Profile => p.profile.take().map(|_| "profile".to_owned()),
        Slot::Skills => p.skills.take().map(|_| "skills".to_owned()),
        _ => None,
    };
    id.unwrap_or_default()
}
