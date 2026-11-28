//! `dn eval context` (#350, ADR-0020 §2.1): how much of its context window a model really
//! uses well, so the assembler's effective-context default (60% of a local model's loaded
//! context until measured) can be set per model from measurements.
//!
//! Synthetic long documents, generated deterministically here (nothing to download), at
//! growing lengths in the model's own tokens:
//! - **needle:** one fact (an access code) at 10–90% depth; asked for it;
//! - **multikey:** four similar facts; asked for one of them;
//! - **multihop:** a chain of four assignments (`V3 = V2`, …) spread through the document,
//!   with two distractor chains; asked for the last variable's value.
//!
//! Each answer is scored by whether it states the expected number. A length counts as
//! **effective** when the accuracy there, and at every shorter length, is at least
//! [`THRESHOLD`]. The report gives accuracy per length and task, the effective length, and
//! its share of the model's training context. Nothing is sent anywhere.

use super::{ModelArgs, Report, Turn, eval_config, turn};
use clap::Args;
use dn_core::chat::{ChatMessage, ChatModel, ChatRequest, Reasoning, Role};
use dn_core::model::ModelRef;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The accuracy a length needs to count as effective.
pub const THRESHOLD: f64 = 0.85;
const DEFAULT_LENGTHS: &[u32] = &[4096, 8192, 16384, 32768, 65536, 131072];
const DEPTHS: &[f64] = &[0.1, 0.3, 0.5, 0.7, 0.9];
/// Room for the answer: a number, with room for a model that adds a few words.
const ANSWER_TOKENS: u32 = 64;
/// Room left in the loaded context for the question, template and answer.
const HEADROOM: u32 = 1024;

#[derive(Args)]
pub struct ContextArgs {
    #[command(flatten)]
    model: ModelArgs,
    /// Lengths to test, in the model's tokens (default 4k to 128k, up to its training
    /// context).
    #[arg(long, value_delimiter = ',')]
    lengths: Option<Vec<u32>>,
    /// Documents per length for multikey and for multihop; the needle gets two per depth
    /// (five depths), so 20 per length by default.
    #[arg(long, default_value_t = 5)]
    samples: u32,
    /// Only these tasks (needle, multikey, multihop); default all.
    #[arg(long, value_delimiter = ',')]
    tasks: Option<Vec<String>>,
    /// Report path (default: eval-context-<time>.json).
    #[arg(long, short)]
    out: Option<PathBuf>,
}

/// A small deterministic generator (xorshift64*), so every run builds the same documents.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[(self.next() % xs.len() as u64) as usize]
    }
    pub fn digits(&mut self, n: u32) -> String {
        let lo = 10u64.pow(n - 1);
        (lo + self.next() % (9 * lo)).to_string()
    }
}

const NAMES: &[&str] = &[
    "Ana", "Bruno", "Clara", "Dmitri", "Elif", "Farah", "Goran", "Hana", "Ivo", "Jonas", "Kemal",
    "Lena", "Mateo", "Nadia", "Oskar", "Priya", "Quentin", "Rosa", "Sven", "Tariq",
];
const VERBS: &[&str] = &[
    "repaired",
    "painted",
    "measured",
    "cleaned",
    "moved",
    "inspected",
    "photographed",
    "labelled",
    "sorted",
    "weighed",
    "counted",
    "described",
];
const NOUNS: &[&str] = &[
    "bench", "lamp", "fence", "boat", "ladder", "table", "window", "bicycle", "cabinet", "gate",
    "kettle", "clock", "barrel", "shelf",
];
const PLACES: &[&str] = &[
    "harbour",
    "library",
    "orchard",
    "workshop",
    "station",
    "market",
    "chapel",
    "mill",
    "bakery",
    "school",
    "garden",
    "warehouse",
    "bridge",
    "museum",
    "theatre",
    "pharmacy",
];
/// Names for the multihop chains: letters only, never words the filler uses.
const CODE_NAMES: &[&str] = &[
    "KESTREL", "JUNIPER", "OBSIDIAN", "MARIGOLD", "TUNDRA", "SAFFRON", "QUASAR", "BRAMBLE",
    "CINDER", "NARWHAL", "ZEPHYR", "LANTERN",
];
const WEATHER: &[&str] = &["rain", "fog", "sunshine", "wind", "snow", "a cold breeze"];
const DAYS: &[&str] = &[
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// One filler sentence: plausible, varied, and never a number of the kind asked for.
fn filler(r: &mut Rng) -> String {
    match r.next() % 4 {
        0 => format!(
            "On {} {} {} the {} near the {}.",
            r.pick(DAYS),
            r.pick(NAMES),
            r.pick(VERBS),
            r.pick(NOUNS),
            r.pick(PLACES)
        ),
        1 => format!(
            "The {} by the {} was {} again after the {}.",
            r.pick(NOUNS),
            r.pick(PLACES),
            r.pick(VERBS),
            r.pick(WEATHER)
        ),
        2 => format!(
            "{} wrote that the {} at the {} needed care before {}.",
            r.pick(NAMES),
            r.pick(NOUNS),
            r.pick(PLACES),
            r.pick(DAYS)
        ),
        _ => format!(
            "In the {} the {} kept everyone indoors, so {} {} the {} instead.",
            r.pick(PLACES),
            r.pick(WEATHER),
            r.pick(NAMES),
            r.pick(VERBS),
            r.pick(NOUNS)
        ),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    Needle,
    Multikey,
    Multihop,
}

/// One test document and what a correct answer states.
#[derive(Clone, Debug)]
pub struct Case {
    pub task: Task,
    pub depth: Option<f64>,
    pub document: String,
    pub question: String,
    pub expected: String,
}

/// Places `inserts` (sentence, relative position 0..1) among `n` filler sentences.
fn compose(r: &mut Rng, n: usize, mut inserts: Vec<(String, f64)>) -> String {
    inserts.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut out = String::new();
    let mut ins = inserts.into_iter().peekable();
    for i in 0..n {
        let pos = i as f64 / n.max(1) as f64;
        while let Some((s, _)) = ins.next_if(|(_, p)| *p <= pos) {
            out.push_str(&s);
            out.push(' ');
        }
        out.push_str(&filler(r));
        out.push(' ');
    }
    for (s, _) in ins {
        out.push_str(&s);
        out.push(' ');
    }
    out.trim_end().to_owned()
}

/// Builds a case with `n` filler sentences.
pub fn build(task: Task, seed: u64, n: usize, depth: Option<f64>) -> Case {
    let mut r = Rng::new(seed);
    match task {
        Task::Needle => {
            let place = r.pick(PLACES);
            let code = r.digits(7);
            let d = depth.unwrap_or(0.5);
            let doc = compose(
                &mut r,
                n,
                vec![(
                    format!("The access code for the {place} archive is {code}."),
                    d,
                )],
            );
            Case {
                task,
                depth: Some(d),
                document: doc,
                question: format!("What is the access code for the {place} archive?"),
                expected: code,
            }
        }
        Task::Multikey => {
            let mut places: Vec<&str> = Vec::new();
            while places.len() < 4 {
                let p = r.pick(PLACES);
                if !places.contains(&p) {
                    places.push(p);
                }
            }
            let codes: Vec<String> = (0..4).map(|_| r.digits(7)).collect();
            let inserts = places
                .iter()
                .zip(&codes)
                .map(|(p, c)| {
                    let at = (r.next() % 900) as f64 / 1000.0 + 0.05;
                    (format!("The access code for the {p} archive is {c}."), at)
                })
                .collect();
            let ask = (r.next() % 4) as usize;
            let doc = compose(&mut r, n, inserts);
            Case {
                task,
                depth: None,
                document: doc,
                question: format!("What is the access code for the {} archive?", places[ask]),
                expected: codes[ask].clone(),
            }
        }
        Task::Multihop => {
            // Three chains of four; the first is asked about. Names are unique per document.
            let mut inserts = Vec::new();
            let mut asked = (String::new(), String::new());
            for chain in 0..3 {
                let value = r.digits(5);
                // Letter-only names: names with digits got their digits answered instead of
                // the value (#350, first run).
                let vars: Vec<String> = (0..4)
                    .map(|i| CODE_NAMES[chain * 4 + i].to_string())
                    .collect();
                // Assignments in order through the document.
                let mut at = (r.next() % 100) as f64 / 1000.0 + 0.02;
                inserts.push((format!("Note: {} = {value}.", vars[0]), at));
                for w in vars.windows(2) {
                    at += 0.18 + (r.next() % 50) as f64 / 1000.0;
                    inserts.push((format!("Note: {} = {}.", w[1], w[0]), at.min(0.98)));
                }
                if chain == 0 {
                    asked = (vars[3].clone(), value);
                }
            }
            let doc = compose(&mut r, n, inserts);
            Case {
                task,
                depth: None,
                document: doc,
                question: format!(
                    "Each note sets a name to a number or to another name. Following the notes, \
                     what number is {} equal to?",
                    asked.0
                ),
                expected: asked.1,
            }
        }
    }
}

/// The chat request for a case.
pub fn history(c: &Case) -> Vec<ChatMessage> {
    vec![
        ChatMessage::text(
            Role::System,
            "Answer from the document only. Reply with the number and nothing else.",
        ),
        ChatMessage::text(
            Role::User,
            format!(
                "Read this document.\n\n<document>\n{}\n</document>\n\nQuestion: {}\nAnswer with the number only.",
                c.document, c.question
            ),
        ),
    ]
}

/// Whether `answer` states `expected` as a whole number (not inside a longer one).
pub fn correct(answer: &str, expected: &str) -> bool {
    answer
        .split(|c: char| !c.is_ascii_digit())
        .any(|n| n == expected)
}

/// The longest length whose accuracy, and every shorter one's, is at least [`THRESHOLD`].
pub fn effective(acc: &BTreeMap<u32, f64>) -> Option<u32> {
    let mut best = None;
    for (len, a) in acc {
        if *a < THRESHOLD {
            break;
        }
        best = Some(*len);
    }
    best
}

#[derive(Serialize)]
struct Item {
    task: Task,
    target_tokens: u32,
    /// The prompt's exact token count (the model's own tokenizer), when it can count.
    tokens: Option<u32>,
    depth: Option<f64>,
    expected: String,
    answer: String,
    correct: bool,
    #[serde(flatten)]
    turn: Turn,
}

/// The prompt's tokens as the run sends it (same template and settings as [`turn`]).
fn count(model: &dyn ChatModel, model_ref: &ModelRef, h: &[ChatMessage]) -> Option<u32> {
    let req = ChatRequest {
        model: model_ref.clone(),
        messages: h.to_vec(),
        tools: Vec::new(),
        max_tokens: Some(ANSWER_TOKENS),
        temperature: Some(0.0),
        response_schema: None,
        reasoning: Reasoning::Off,
        caller: "eval.context".into(),
    };
    model.count_tokens(&req)?.ok()
}

/// Filler sentences for about `target` prompt tokens, from a measured tokens-per-sentence.
fn sentences_for(target: u32, per_sentence: f64, overhead: u32) -> usize {
    (((target.saturating_sub(overhead)) as f64) / per_sentence)
        .floor()
        .max(1.0) as usize
}

pub async fn run(a: ContextArgs) -> Result<(), String> {
    let path = crate::resolve_model(&a.model.model_name())?;
    let g = dn_models::gguf::Gguf::open(&path).map_err(|e| e.to_string())?;
    let n_ctx_train = dn_models::fit::ModelFootprint::from_gguf(&g).n_ctx_train;
    let mut lengths: Vec<u32> = a
        .lengths
        .clone()
        .unwrap_or_else(|| DEFAULT_LENGTHS.to_vec())
        .into_iter()
        .filter(|l| l + HEADROOM <= n_ctx_train)
        .collect();
    // The training context itself (less the headroom), if the list stops short of it.
    let top = n_ctx_train.saturating_sub(HEADROOM);
    if a.lengths.is_none() && lengths.last().is_some_and(|l| *l < top) && top <= 131_072 {
        lengths.push(top);
    }
    lengths.sort_unstable();
    lengths.dedup();
    let Some(&max) = lengths.last() else {
        return Err(format!(
            "no length fits the model's training context ({n_ctx_train} tokens)"
        ));
    };
    let m = a.model.load(Some(max + HEADROOM)).await?;
    // Tokens per filler sentence and the fixed overhead, measured with the model's own
    // tokenizer.
    let probe = build(Task::Needle, 1, 400, Some(0.5));
    let probe0 = build(Task::Needle, 1, 1, Some(0.5));
    let (per_sentence, overhead) = match (
        count(&m.chat, &m.model_ref, &history(&probe)),
        count(&m.chat, &m.model_ref, &history(&probe0)),
    ) {
        (Some(n400), Some(n1)) => (f64::from(n400 - n1) / 399.0, n1),
        _ => return Err("this backend can't count tokens; run with a local GGUF".into()),
    };
    let cfg = eval_config("eval.context", &m.model_ref, Some(ANSWER_TOKENS));
    let mut items = Vec::new();
    for (li, &len) in lengths.iter().enumerate() {
        let n = sentences_for(len, per_sentence, overhead);
        let mut cases: Vec<Case> = DEPTHS
            .iter()
            .enumerate()
            .flat_map(|(i, d)| {
                (0..2).map(move |rep| {
                    build(
                        Task::Needle,
                        1000 + li as u64 * 100 + i as u64 * 2 + rep,
                        n,
                        Some(*d),
                    )
                })
            })
            .collect();
        for s in 0..a.samples {
            cases.push(build(
                Task::Multikey,
                2000 + li as u64 * 100 + u64::from(s),
                n,
                None,
            ));
            cases.push(build(
                Task::Multihop,
                3000 + li as u64 * 100 + u64::from(s),
                n,
                None,
            ));
        }
        let wanted = |t: Task| {
            a.tasks
                .as_ref()
                .is_none_or(|ts| ts.iter().any(|x| x.eq_ignore_ascii_case(&format!("{t:?}"))))
        };
        cases.retain(|c| wanted(c.task));
        for c in cases {
            let h = history(&c);
            let tokens = count(&m.chat, &m.model_ref, &h);
            let t = turn(&m.chat, &cfg, h).await;
            let ok = correct(&t.text, &c.expected);
            eprintln!(
                "[eval context] {len} tokens ({}), {:?}{}: {}",
                tokens.map_or("?".into(), |t| t.to_string()),
                c.task,
                c.depth
                    .map(|d| format!(" at {:.0}%", d * 100.0))
                    .unwrap_or_default(),
                if ok { "right" } else { "wrong" }
            );
            items.push(Item {
                task: c.task,
                target_tokens: len,
                tokens,
                depth: c.depth,
                expected: c.expected,
                answer: t.text.chars().take(200).collect(),
                correct: ok,
                turn: t,
            });
        }
    }
    let mut acc = BTreeMap::<u32, f64>::new();
    let mut by_task = BTreeMap::<u32, BTreeMap<String, f64>>::new();
    for &len in &lengths {
        let at: Vec<&Item> = items.iter().filter(|i| i.target_tokens == len).collect();
        let rate =
            |xs: &[&Item]| xs.iter().filter(|i| i.correct).count() as f64 / xs.len().max(1) as f64;
        acc.insert(len, rate(&at));
        for task in [Task::Needle, Task::Multikey, Task::Multihop] {
            let t: Vec<&Item> = at.iter().copied().filter(|i| i.task == task).collect();
            by_task
                .entry(len)
                .or_default()
                .insert(format!("{task:?}").to_lowercase(), rate(&t));
        }
    }
    let eff = effective(&acc);
    // Per task too: a task that fails at every length is a task problem, not a context one.
    let mut eff_by_task = BTreeMap::new();
    for task in [Task::Needle, Task::Multikey, Task::Multihop] {
        let name = format!("{task:?}").to_lowercase();
        let per: BTreeMap<u32, f64> = by_task
            .iter()
            .filter_map(|(l, t)| t.get(&name).map(|v| (*l, *v)))
            .filter(|_| items.iter().any(|i| i.task == task))
            .collect();
        if !per.is_empty() {
            eff_by_task.insert(name, effective(&per));
        }
    }
    let summary = serde_json::json!({
        "n_ctx_train": n_ctx_train,
        "loaded_ctx": max + HEADROOM,
        "threshold": THRESHOLD,
        "accuracy": acc,
        "by_task": by_task,
        "effective_tokens": eff,
        "effective_tokens_by_task": eff_by_task,
        "effective_share_of_training_context": eff.map(|e| f64::from(e) / f64::from(n_ctx_train)),
        "tokens_per_filler_sentence": per_sentence,
    });
    let report = Report::new("context", &m, summary, items);
    let out = report.write(a.out)?;
    m.chat.unload().await?;
    println!(
        "effective context: {} of {n_ctx_train} trained · accuracy {:?} · report: {}",
        eff.map_or("below the shortest length".into(), |e| format!(
            "{e} tokens"
        )),
        acc,
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_are_deterministic_and_hold_their_answer() {
        for task in [Task::Needle, Task::Multikey, Task::Multihop] {
            let a = build(task, 7, 300, Some(0.3));
            let b = build(task, 7, 300, Some(0.3));
            assert_eq!(a.document, b.document);
            assert!(a.document.contains(&a.expected), "{task:?}");
            assert_ne!(a.document, build(task, 8, 300, Some(0.3)).document);
        }
    }

    #[test]
    fn the_needle_sits_at_its_depth() {
        let c = build(Task::Needle, 3, 1000, Some(0.9));
        let at = c.document.find(&c.expected).unwrap() as f64 / c.document.len() as f64;
        assert!((at - 0.9).abs() < 0.05, "{at}");
    }

    #[test]
    fn a_multihop_chain_resolves_to_the_asked_value_and_distractors_differ() {
        let c = build(Task::Multihop, 11, 500, None);
        // Follow the chain in the document.
        let notes: Vec<(&str, &str)> = c
            .document
            .split("Note: ")
            .skip(1)
            .filter_map(|s| s.split_once('.').map(|(a, _)| a))
            .filter_map(|s| s.split_once(" = "))
            .collect();
        let var = c
            .question
            .split(|ch: char| !ch.is_ascii_alphabetic())
            .find(|w| CODE_NAMES.contains(w))
            .unwrap();
        assert!(
            var.chars().all(|ch| ch.is_ascii_uppercase()),
            "letters only: {var}"
        );
        let mut cur = var.to_string();
        for _ in 0..8 {
            match notes.iter().find(|(k, _)| *k == cur) {
                Some((_, v)) => cur = v.to_string(),
                None => break,
            }
        }
        assert_eq!(cur, c.expected);
        assert_eq!(notes.len(), 12, "three chains of four");
    }

    #[test]
    fn scoring_needs_the_whole_number_and_effective_stops_at_the_first_drop() {
        assert!(correct("The code is 4711023.", "4711023"));
        assert!(!correct("14711023", "4711023"));
        assert!(!correct("I don't know", "4711023"));
        let acc: BTreeMap<u32, f64> =
            [(4096, 1.0), (8192, 0.9), (16384, 0.6), (32768, 0.95)].into();
        assert_eq!(
            effective(&acc),
            Some(8192),
            "a later recovery doesn't count"
        );
        assert_eq!(effective(&[(4096, 0.5)].into()), None);
        assert_eq!(sentences_for(4096, 12.0, 96), 333);
    }
}
