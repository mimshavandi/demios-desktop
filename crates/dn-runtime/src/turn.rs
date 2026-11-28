//! A run's requests through the assembler: dn-agent's per-step hook (ADR-0020 §2).

use crate::assemble::{Parts, assemble};
use crate::estimate::Estimator;
use crate::policy::Policy;
use dn_agent::{Built, RequestBuilder};
use dn_core::CoreError;
use dn_core::chat::{BoxFuture, ChatMessage, ChatModel, ChatRequest};
use dn_core::model::ModelRef;
use dn_core::run::TokenCount;
use std::sync::{Arc, Mutex};

/// The key a provider's estimates are calibrated under.
pub fn estimate_key(model: &ModelRef) -> String {
    match model {
        ModelRef::Byok { provider_id, .. } => format!("byok:{provider_id}"),
        ModelRef::Remote { endpoint_id, .. } => format!("remote:{endpoint_id}"),
        other => other.to_string(),
    }
}

/// Builds every step of one run with [`assemble`]. The run's history is the stored
/// conversation with the new input last: the first `turns` messages are the conversation,
/// the next is the input, and whatever the run adds comes after it.
pub struct TurnBuilder {
    model: Arc<dyn ChatModel>,
    policy: Policy,
    parts: Parts,
    turns: usize,
    estimator: Arc<Estimator>,
    key: String,
    /// The measure of the last request built, when it was estimated.
    last_units: Mutex<Option<f64>>,
}

impl TurnBuilder {
    /// `parts` holds the slots other than `turns`, `input` and `continuation`, which come
    /// from the run's messages; `turns` is how many of those are the earlier conversation.
    pub fn new(
        model: Arc<dyn ChatModel>,
        policy: Policy,
        parts: Parts,
        turns: usize,
        estimator: Arc<Estimator>,
        model_ref: &ModelRef,
    ) -> Self {
        Self {
            model,
            policy,
            parts,
            turns,
            estimator,
            key: estimate_key(model_ref),
            last_units: Mutex::new(None),
        }
    }
}

impl RequestBuilder for TurnBuilder {
    fn build<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        base: ChatRequest,
    ) -> BoxFuture<'a, Result<Built, CoreError>> {
        Box::pin(async move {
            let split = self.turns.min(messages.len());
            let mut parts = self.parts.clone();
            // The run's tools (the loop puts them in `base`); the assembler sorts them.
            parts.tools = base.tools.clone();
            parts.turns = messages[..split].to_vec();
            parts.input = messages.get(split).cloned();
            parts.continuation = messages.get(split + 1..).unwrap_or_default().to_vec();
            let a = assemble(
                &*self.model,
                &self.policy,
                parts,
                &base,
                &self.estimator,
                &self.key,
            )
            .await?;
            *self.last_units.lock().unwrap_or_else(|p| p.into_inner()) =
                (a.context.counted_by == TokenCount::Estimate).then_some(a.units);
            Ok(Built {
                request: a.request,
                context: Some(a.context),
                dropped: a.dropped,
            })
        })
    }

    fn reported(&self, input_tokens: u32) {
        let units = *self.last_units.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(u) = units {
            self.estimator.calibrate(&self.key, u, input_tokens);
        }
    }
}
