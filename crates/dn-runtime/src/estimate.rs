//! Token estimates for models that can't count (remote ones), ADR-0020 §2.1: calibrated on
//! what each provider reported before, with a safety margin, never a provider call per step.
//!
//! The text is measured in *units*: an ASCII character is a third of a unit (English runs
//! at about 3–4 characters per token), any other character a whole unit (CJK and many other
//! scripts run near one token per character). A provider's factor starts at 1 and moves
//! towards `reported tokens / units` after each call it reports, kept within bounds.

use dn_core::chat::{ChatMessage, ChatRequest, ContentPart};
use std::collections::HashMap;
use std::sync::Mutex;

/// Added on top of the calibrated estimate.
pub const MARGIN: f64 = 1.15;
/// Per message, for role and separator tokens.
const PER_MESSAGE: f64 = 4.0;
/// How far one observation moves the factor.
const LEARN: f64 = 0.3;
const FACTOR_BOUNDS: (f64, f64) = (0.3, 2.5);

/// The text measure of `s`.
pub fn units(s: &str) -> f64 {
    let (mut ascii, mut other) = (0usize, 0usize);
    for c in s.chars() {
        if c.is_ascii() {
            ascii += 1;
        } else {
            other += 1;
        }
    }
    ascii as f64 / 3.0 + other as f64
}

fn message_units(m: &ChatMessage) -> f64 {
    let mut u = PER_MESSAGE;
    for p in &m.content {
        u += match p {
            ContentPart::Text { text } => units(text),
            // Images aren't sent to providers yet (dn-providers drops them).
            _ => 0.0,
        };
    }
    for c in &m.tool_calls {
        u += units(&c.name) + units(&c.arguments.to_string());
    }
    u
}

/// The measure of a whole request: messages and tool definitions.
pub fn request_units(req: &ChatRequest) -> f64 {
    let tools: f64 = req
        .tools
        .iter()
        .map(|t| units(&t.name) + units(&t.description) + units(&t.input_schema.to_string()))
        .sum();
    req.messages.iter().map(message_units).sum::<f64>() + tools
}

/// Per-provider calibration, shared by every surface in the process.
#[derive(Debug, Default)]
pub struct Estimator {
    factors: Mutex<HashMap<String, f64>>,
}

impl Estimator {
    fn factor(&self, key: &str) -> f64 {
        self.factors
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .copied()
            .unwrap_or(1.0)
    }

    /// The estimated input tokens of `req` for provider `key`, margin included.
    pub fn estimate(&self, key: &str, req: &ChatRequest) -> u32 {
        let t = request_units(req) * self.factor(key) * MARGIN;
        t.ceil().min(f64::from(u32::MAX)) as u32
    }

    /// Learns from a call: `units` was the request's measure, `reported` the input tokens
    /// the provider said it read.
    pub fn calibrate(&self, key: &str, units: f64, reported: u32) {
        if units < 1.0 || reported == 0 {
            return;
        }
        let seen = (f64::from(reported) / units).clamp(FACTOR_BOUNDS.0, FACTOR_BOUNDS.1);
        let mut f = self.factors.lock().unwrap_or_else(|p| p.into_inner());
        let e = f.entry(key.to_owned()).or_insert(1.0);
        *e = (*e * (1.0 - LEARN) + seen * LEARN).clamp(FACTOR_BOUNDS.0, FACTOR_BOUNDS.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_core::chat::Role;

    fn req(text: &str) -> ChatRequest {
        ChatRequest {
            model: "local:test".parse().unwrap(),
            messages: vec![ChatMessage::text(Role::User, text)],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            response_schema: None,
            reasoning: Default::default(),
            caller: "t".into(),
        }
    }

    #[test]
    fn non_latin_text_counts_as_more_tokens() {
        assert_eq!(units("abc"), 1.0);
        assert_eq!(units("日本語"), 3.0);
        assert!(units("سلام دنیا") > units("hello wor"));
    }

    #[test]
    fn the_estimate_has_a_margin_and_learns_from_the_provider() {
        let e = Estimator::default();
        let r = req(&"word ".repeat(300)); // 1500 ASCII characters
        let u = request_units(&r);
        assert_eq!(e.estimate("anthropic", &r), (u * MARGIN).ceil() as u32);
        // The provider reports fewer tokens than the measure: the estimate comes down, but
        // keeps its margin over what was reported.
        for _ in 0..20 {
            e.calibrate("anthropic", u, 300);
        }
        let est = e.estimate("anthropic", &r);
        assert!((300..=360).contains(&est), "{est}");
        // Another provider isn't affected.
        assert_eq!(e.estimate("openai", &r), (u * MARGIN).ceil() as u32);
    }

    #[test]
    fn calibration_stays_in_bounds() {
        let e = Estimator::default();
        for _ in 0..50 {
            e.calibrate("p", 10.0, 1_000_000);
        }
        assert!(e.factor("p") <= FACTOR_BOUNDS.1);
        e.calibrate("p", 0.0, 5);
        e.calibrate("p", 10.0, 0);
    }
}
