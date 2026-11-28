//! Per-surface policies (ADR-0020 §1–2): how much of the model's context a surface uses,
//! what may be left out when a prompt doesn't fit, and in which order.

use dn_core::chat::LimitSource;
use dn_core::run::Slot;

/// What happens when the conversation itself doesn't fit after everything droppable is
/// gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    /// The turn fails with a clear message. Nothing the person wrote is dropped silently.
    Fail,
}

/// A surface's context policy.
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    /// The surface, as in `ChatRequest::caller` (`chat.reply`, `raw`).
    pub surface: &'static str,
    /// Share of a local model's loaded context used for input when the model hasn't been
    /// measured (ADR-0020 §2.1: 60%).
    pub local_share: f32,
    /// A local model's measured effective context (#350, from the catalogue): the input may
    /// use the loaded context up to this many tokens instead of `local_share`. `None`: not
    /// measured. Set per model with [`Policy::with_measured`].
    pub measured_context: Option<u32>,
    /// Room kept for the answer when the request doesn't set `max_tokens`.
    pub output_reserve: u32,
    /// Slots that may be left out to fit, lowest priority first. Slots not listed are
    /// never dropped: the system prompt, tools, the new input and the recap.
    pub droppable: Vec<Slot>,
    pub overflow: Overflow,
    /// The surface can have sources, so its system slot carries the shared answer and
    /// citation rules (`answer.rules`, #364) on every turn. Others (add-ons, memory) read
    /// this rather than keeping their own list of surfaces.
    pub answer_rules: bool,
}

impl Policy {
    /// A chat answer. Retrieved items go first, then project knowledge, then the profile
    /// and the skills index. The conversation isn't dropped: until compaction (ADR-0020
    /// §3) a chat that doesn't fit fails clearly.
    pub fn chat() -> Self {
        Self {
            surface: "chat.reply",
            local_share: 0.6,
            measured_context: None,
            output_reserve: 4096,
            droppable: vec![
                Slot::Retrieved,
                Slot::Knowledge,
                Slot::Profile,
                Slot::Skills,
            ],
            overflow: Overflow::Fail,
            answer_rules: true,
        }
    }

    /// Benchmarks, the tool probe and `dn run`: the prompt as given, counted and recorded
    /// like everything else, nothing added or dropped.
    pub fn raw() -> Self {
        Self {
            surface: "raw",
            local_share: 1.0,
            measured_context: None,
            output_reserve: 0,
            droppable: Vec::new(),
            overflow: Overflow::Fail,
            answer_rules: false,
        }
    }

    /// This policy for a model whose effective context was measured (`None`: not measured,
    /// the share applies).
    pub fn with_measured(mut self, tokens: Option<u32>) -> Self {
        self.measured_context = tokens;
        self
    }

    /// The input budget for a model with `context` tokens, answering at most `max_tokens`.
    pub fn budget(&self, context: u32, source: LimitSource, max_tokens: Option<u32>) -> u32 {
        let reserve = max_tokens.unwrap_or(self.output_reserve);
        let room = context.saturating_sub(reserve);
        match source {
            // A local context is shared with the answer and degrades well before it's full.
            // A measured model uses as much as it was measured to use well, never more than
            // what's loaded.
            LimitSource::Loaded => {
                let usable = match self.measured_context {
                    Some(measured) => context.min(measured),
                    None => (f64::from(context) * f64::from(self.local_share)).floor() as u32,
                };
                room.min(usable)
            }
            LimitSource::Provider | LimitSource::Published | LimitSource::User => room,
        }
    }

    /// The most the core profile (memory, ADR-0020 §4.1) may take, in
    /// [`crate::estimate::units`]: a measure that's the same for every model, so switching
    /// models doesn't rebuild the profile and break the cached prefix. The memory store
    /// builds the profile to fit, whole items only; the assembler's real budget check still
    /// drops it whole when the prompt doesn't fit. Units, not tokens.
    pub fn profile_cap(&self) -> f64 {
        match self.surface {
            "chat.reply" => 1_500.0,
            _ => 0.0,
        }
    }

    /// Where `slot` stands in the drop order (`None`: never dropped).
    pub fn drop_rank(&self, slot: Slot) -> Option<usize> {
        self.droppable.iter().position(|s| *s == slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_models_use_a_share_and_remote_ones_keep_room_for_the_answer() {
        let p = Policy::chat();
        assert_eq!(p.budget(8192, LimitSource::Loaded, None), 4096);
        assert_eq!(p.budget(8192, LimitSource::Loaded, Some(512)), 4915);
        assert_eq!(p.budget(200_000, LimitSource::Published, None), 195_904);
        assert_eq!(
            p.budget(200_000, LimitSource::Provider, Some(8000)),
            192_000
        );
        assert_eq!(p.budget(1000, LimitSource::User, Some(4000)), 0);
        assert_eq!(Policy::raw().budget(8192, LimitSource::Loaded, None), 8192);
    }

    #[test]
    fn a_measured_model_uses_its_loaded_context_up_to_the_measured_size() {
        let p = Policy::chat().with_measured(Some(40_960));
        // Loaded below the measured size: all of it, less the answer's room.
        assert_eq!(p.budget(16_384, LimitSource::Loaded, None), 12_288);
        // Loaded above it: capped at the measured size.
        assert_eq!(p.budget(65_536, LimitSource::Loaded, Some(1024)), 40_960);
        // Unmeasured: 60% as before.
        assert_eq!(
            Policy::chat().budget(16_384, LimitSource::Loaded, None),
            9_830
        );
        // Only a local model's loaded context is affected.
        assert_eq!(p.budget(200_000, LimitSource::Published, None), 195_904);
    }

    #[test]
    fn the_system_prompt_input_and_tools_are_never_dropped() {
        let p = Policy::chat();
        for s in [
            Slot::System,
            Slot::Tools,
            Slot::Input,
            Slot::Recap,
            Slot::Conversation,
        ] {
            assert_eq!(p.drop_rank(s), None, "{s:?}");
        }
        assert!(p.drop_rank(Slot::Retrieved) < p.drop_rank(Slot::Knowledge));
    }

    #[test]
    fn chat_carries_the_answer_rules_and_raw_doesnt() {
        assert!(Policy::chat().answer_rules);
        assert!(!Policy::raw().answer_rules);
    }
}

#[cfg(test)]
mod profile_cap_tests {
    use super::Policy;

    #[test]
    fn chat_has_a_profile_budget_and_raw_none() {
        assert_eq!(Policy::chat().profile_cap(), 1_500.0);
        assert_eq!(Policy::raw().profile_cap(), 0.0);
    }
}
