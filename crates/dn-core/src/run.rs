//! What one run (one or more model calls for one request) did: the typed run result of
//! ADR-0020 §8. Every surface gets one, stores it, and the eval suite reads it.
//!
//! [`RunMeter`] builds it from the stream events a surface already sees, so the agent loop
//! and the few direct model calls (probes, benchmarks) measure the same way.

use crate::Id;
use crate::chat::{StreamEvent, Usage};
use crate::model::ModelRef;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Why a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model answered without asking for tools.
    Completed,
    /// The model hit its output token limit.
    Length,
    /// The step limit was reached and the model still wanted tools. The tools of the last
    /// step did run, so history ends with tool results and no final answer: the UI should
    /// offer "continue".
    StepLimit,
    Cancelled,
    /// The run ended with an error (the model failed, or a tool ended the run).
    Failed,
}

/// The context assembler's slots, in their fixed order (ADR-0020 §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    System,
    Tools,
    Skills,
    /// The core profile (memory).
    Profile,
    /// Project or add-on knowledge.
    Knowledge,
    Retrieved,
    /// Summary and recent turns.
    Conversation,
    /// The new input.
    Input,
    /// The one-line task recap.
    Recap,
}

/// What the context assembler did to an item to fit the budget (ADR-0020 §2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DropAction {
    Dropped,
    Summarised,
    Truncated,
    /// An old tool result replaced by a placeholder (ADR-0020 §2.5).
    Placeholder,
}

/// One item the context assembler changed or left out. Nothing fills these before the
/// assembler exists (ADR-0020 R1), so [`RunResult::dropped`] is empty until then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Dropped {
    pub slot: Slot,
    /// What it was: a source id, a message id, a tool name.
    pub source: String,
    /// Tokens it would have taken (or that were saved, when summarised or truncated).
    pub tokens: u32,
    pub action: DropAction,
}

/// The typed result of a run (ADR-0020 §8).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RunResult {
    pub run_id: Id,
    /// The surface that made the call, as in `ChatRequest::caller` (e.g. `chat.reply`).
    pub caller: String,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub model: ModelRef,
    pub stop: StopReason,
    /// Model calls made.
    pub steps: u32,
    /// Summed over the model calls that reported usage. A call cancelled or failed
    /// mid-stream reports nothing, so its tokens aren't included.
    pub usage: Usage,
    /// Start of the run to its end, in milliseconds.
    pub wall_ms: u64,
    /// Start of the run to the first text or reasoning token; `None` if none came.
    pub first_token_ms: Option<u64>,
    /// What the context assembler left out; empty until it exists.
    pub dropped: Vec<Dropped>,
}

/// Measures a run from its stream events.
#[derive(Debug)]
pub struct RunMeter {
    run_id: Id,
    caller: String,
    model: ModelRef,
    start: Instant,
    first_token: Option<Instant>,
    steps: u32,
    usage: Usage,
}

impl RunMeter {
    /// Starts the clock.
    pub fn start(run_id: Id, caller: impl Into<String>, model: ModelRef) -> Self {
        Self {
            run_id,
            caller: caller.into(),
            model,
            start: Instant::now(),
            first_token: None,
            steps: 0,
            usage: Usage::default(),
        }
    }

    /// A model call is about to be made.
    pub fn step(&mut self) {
        self.steps += 1;
    }

    /// Notes one event of the current model call's stream.
    pub fn observe(&mut self, ev: &StreamEvent) {
        match ev {
            StreamEvent::TextDelta { .. } | StreamEvent::ReasoningDelta { .. } => {
                self.first_token.get_or_insert_with(Instant::now);
            }
            StreamEvent::Done { usage, .. } => self.usage.add(usage),
            StreamEvent::ToolCall { .. } => {}
        }
    }

    /// Usage so far.
    pub fn usage(&self) -> Usage {
        self.usage
    }

    pub fn finish(self, stop: StopReason) -> RunResult {
        let start = self.start;
        let ms = |t: Instant| u64::try_from((t - start).as_millis()).unwrap_or(u64::MAX);
        RunResult {
            run_id: self.run_id,
            caller: self.caller,
            model: self.model,
            stop,
            steps: self.steps,
            usage: self.usage,
            wall_ms: ms(Instant::now()),
            first_token_ms: self.first_token.map(ms),
            dropped: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{FinishReason, ToolCall};

    fn done(input: u32, output: u32, cache_read: u32) -> StreamEvent {
        StreamEvent::Done {
            finish_reason: FinishReason::Stop,
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                cached_input_tokens: cache_read,
                ..Usage::default()
            },
        }
    }

    fn local() -> ModelRef {
        ModelRef::Local {
            model_id: "m".into(),
        }
    }

    #[test]
    fn the_meter_sums_usage_counts_steps_and_notes_the_first_token() {
        let mut m = RunMeter::start(Id::new(), "chat.reply", local());
        m.step();
        m.observe(&StreamEvent::ToolCall {
            call: ToolCall {
                id: "c".into(),
                name: "t".into(),
                arguments: serde_json::json!({}),
            },
        });
        m.observe(&done(100, 5, 0));
        m.step();
        m.observe(&StreamEvent::TextDelta { text: "hi".into() });
        m.observe(&done(120, 7, 100));
        let r = m.finish(StopReason::Completed);
        assert_eq!(r.steps, 2);
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 220,
                output_tokens: 12,
                cached_input_tokens: 100,
                ..Usage::default()
            }
        );
        let first = r.first_token_ms.expect("a text token came");
        assert!(first <= r.wall_ms);
        assert!(r.dropped.is_empty());
    }

    #[test]
    fn a_run_without_text_has_no_first_token_and_serialises_snake_case() {
        let r = RunMeter::start(Id::new(), "probe", local()).finish(StopReason::StepLimit);
        assert_eq!(r.first_token_ms, None);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["stop"], "step_limit");
        assert_eq!(v["model"], "local:m");
        let back: RunResult = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }
}
