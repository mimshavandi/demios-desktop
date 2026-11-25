//! The small quality set behind `dn bench --quality` (T-INF-21): short prompts with answers a
//! program can check (a number, a word, a JSON object). Greedy decoding with thinking off, so
//! two backends running the same GGUF should agree. It catches a broken backend, chat
//! template or quant; it does not rank models.

use dn_core::chat::{ChatMessage, ChatModel, ChatRequest, Reasoning, Role, StreamEvent};
use dn_core::model::ModelRef;
use dn_inference::next_event;
use serde::{Deserialize, Serialize};

const SET: &str = include_str!("quality.json");
/// Enough for a short answer, and for a model that ignores "just the number".
const MAX_TOKENS: u32 = 192;

#[derive(Deserialize)]
struct Set {
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: String,
    prompt: String,
    check: Check,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Check {
    /// The answer contains this number as a separate number.
    Number(f64),
    /// The answer contains one of these words (case-insensitive).
    AnyOf(Vec<String>),
    /// The answer's first JSON object has these keys with these values.
    Json(serde_json::Map<String, serde_json::Value>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemResult {
    pub id: String,
    pub pass: bool,
    /// The answer, cut to 200 characters.
    pub answer: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QualityResult {
    pub passed: u32,
    pub total: u32,
    pub items: Vec<ItemResult>,
}

fn numbers(text: &str) -> Vec<f64> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .map(|t| t.trim_matches(|c| c == '.' || c == '-'))
        .filter_map(|t| t.parse::<f64>().ok())
        .collect()
}

fn first_json_object(text: &str) -> Option<serde_json::Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

fn passes(check: &Check, answer: &str) -> bool {
    match check {
        Check::Number(n) => numbers(answer).iter().any(|x| (x - n).abs() < 1e-9),
        Check::AnyOf(words) => {
            let a = answer.to_lowercase();
            words.iter().any(|w| a.contains(&w.to_lowercase()))
        }
        Check::Json(want) => first_json_object(answer)
            .and_then(|v| v.as_object().cloned())
            .is_some_and(|got| {
                want.iter().all(|(k, v)| match (got.get(k), v) {
                    // 36 and 36.0 are the same answer.
                    (Some(g), serde_json::Value::Number(w)) => g.as_f64() == w.as_f64(),
                    (Some(g), w) => g == w,
                    (None, _) => false,
                })
            }),
    }
}

/// Run the set against `model` (`model_ref` labels the requests).
pub async fn run(model: &dyn ChatModel, model_ref: &ModelRef) -> Result<QualityResult, String> {
    let set: Set = serde_json::from_str(SET).map_err(|e| format!("quality.json: {e}"))?;
    let mut items = vec![];
    for it in &set.items {
        let mut s = model.chat(ChatRequest {
            model: model_ref.clone(),
            messages: vec![ChatMessage::text(Role::User, &it.prompt)],
            tools: vec![],
            max_tokens: Some(MAX_TOKENS),
            temperature: Some(0.0),
            response_schema: None,
            reasoning: Reasoning::Off,
            caller: "bench.quality".into(),
        });
        let mut answer = String::new();
        while let Some(ev) = next_event(&mut s).await {
            if let StreamEvent::TextDelta { text } = ev.map_err(|e| format!("{}: {e}", it.id))? {
                answer.push_str(&text);
            }
        }
        let pass = passes(&it.check, &answer);
        eprintln!(
            "[quality] {:12} {}",
            it.id,
            if pass { "pass" } else { "FAIL" }
        );
        items.push(ItemResult {
            id: it.id.clone(),
            pass,
            answer: answer.trim().chars().take(200).collect(),
        });
    }
    Ok(QualityResult {
        passed: items.iter().filter(|i| i.pass).count() as u32,
        total: items.len() as u32,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &str) -> Check {
        let set: Set = serde_json::from_str(SET).unwrap();
        set.items.into_iter().find(|i| i.id == id).unwrap().check
    }

    #[test]
    fn set_parses_with_unique_ids() {
        let set: Set = serde_json::from_str(SET).unwrap();
        assert!(set.items.len() >= 10);
        let mut ids: Vec<_> = set.items.iter().map(|i| &i.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), set.items.len());
    }

    #[test]
    fn numbers_words_and_json_are_checked() {
        assert!(passes(&check("arith-mul"), "391"));
        assert!(passes(&check("arith-mul"), "17 × 23 = 391."));
        assert!(!passes(&check("arith-mul"), "3910"));
        assert!(passes(&check("minutes"), "210 minutes"));
        assert!(passes(&check("weekday"), "Saturday."));
        assert!(!passes(&check("weekday"), "Friday"));
        assert!(passes(
            &check("json-object"),
            "```json\n{\"name\": \"Ada\", \"age\": 36.0}\n```"
        ));
        assert!(!passes(
            &check("json-object"),
            "{\"name\": \"ada\", \"age\": 36}"
        ));
        assert!(passes(&check("json-primes"), "{\"primes\":[2,3,5,7]}"));
        assert!(!passes(&check("json-primes"), "{\"primes\":[1,2,3,5]}"));
        assert!(!passes(&check("json-primes"), "no json"));
    }
}
