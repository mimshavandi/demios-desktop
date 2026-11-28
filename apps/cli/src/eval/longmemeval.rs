//! LongMemEval (Wu et al., 2024; MIT): questions about a long history of chat sessions.
//! The data isn't in this repository: `scripts/eval/fetch-longmemeval.sh` downloads a pinned
//! revision and checks its SHA-256.
//!
//! Two modes, so the report can show the drop between them (ADR-0020 §8 "memory"):
//! - `history`: the whole history is put in front of the question (reading it directly);
//! - `retrieval`: the sessions go into a dn-knowledge index (in memory, discarded after the
//!   run) and only the best passages reach the model, through the same untrusted-content
//!   rendering the app uses for project files. Keyword (BM25) search for now.
//!
//! Answers are written for LongMemEval's own judge (`--hypotheses`, JSONL with
//! `question_id` and `hypothesis`); the report adds the run result per question and, in
//! retrieval mode, the session recall: how many of the sessions holding the answer were
//! retrieved.

use super::{ModelArgs, Report, Turn, eval_config, turn};
use clap::{Args, ValueEnum};
use dn_core::chat::{ChatMessage, Role};
use dn_knowledge::{Index, SourceFile};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The whole history in context.
    History,
    /// Only passages found by our own search (dn-knowledge).
    Retrieval,
}

#[derive(Args)]
pub struct LongMemEvalArgs {
    /// A LongMemEval file, e.g. longmemeval_s_cleaned.json or longmemeval_oracle.json.
    data: PathBuf,
    #[command(flatten)]
    model: ModelArgs,
    #[arg(long, value_enum, default_value = "retrieval")]
    mode: Mode,
    /// Passages retrieved per question (retrieval mode).
    #[arg(long, default_value_t = 8)]
    top_k: usize,
    /// Only the first N questions.
    #[arg(long)]
    limit: Option<usize>,
    /// Output tokens per answer at most.
    #[arg(long, default_value_t = 256)]
    max_tokens: u32,
    /// Also write the answers as JSONL for LongMemEval's evaluation script.
    #[arg(long)]
    hypotheses: Option<PathBuf>,
    /// Report path (default: eval-longmemeval-<time>.json).
    #[arg(long, short)]
    out: Option<PathBuf>,
}

/// One question with its history (the fields this runner reads).
#[derive(Debug, Deserialize)]
pub struct Instance {
    pub question_id: String,
    pub question_type: String,
    pub question: String,
    /// A string in most items, a number in a few.
    pub answer: serde_json::Value,
    pub question_date: String,
    pub haystack_session_ids: Vec<String>,
    pub haystack_dates: Vec<String>,
    pub haystack_sessions: Vec<Vec<Message>>,
    #[serde(default)]
    pub answer_session_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Instance {
    fn check(&self) -> Result<(), String> {
        let n = self.haystack_sessions.len();
        if self.haystack_session_ids.len() != n || self.haystack_dates.len() != n {
            return Err(format!(
                "{}: {} sessions but {} ids and {} dates",
                self.question_id,
                n,
                self.haystack_session_ids.len(),
                self.haystack_dates.len()
            ));
        }
        Ok(())
    }

    /// Abstention questions (no answer in the history) end in `_abs`.
    pub fn is_abstention(&self) -> bool {
        self.question_id.ends_with("_abs")
    }
}

/// A session as plain text, one line per message.
pub fn render_session(date: &str, messages: &[Message]) -> String {
    let mut s = format!("Chat session on {date}\n");
    for m in messages {
        s.push_str(&m.role);
        s.push_str(": ");
        s.push_str(&m.content.replace('\n', " "));
        s.push('\n');
    }
    s
}

const SYSTEM: &str = "You answer questions about the user's past chats with you. Use only \
                      the chats given. If they don't contain the answer, say you don't know.";

fn question(inst: &Instance) -> String {
    format!(
        "Today is {}.\nQuestion: {}\nAnswer briefly.",
        inst.question_date, inst.question
    )
}

fn history_prompt(inst: &Instance) -> Vec<ChatMessage> {
    let mut chats = String::new();
    for (date, msgs) in inst.haystack_dates.iter().zip(&inst.haystack_sessions) {
        chats.push_str(&render_session(date, msgs));
        chats.push('\n');
    }
    vec![
        ChatMessage::text(Role::System, SYSTEM),
        ChatMessage::text(
            Role::User,
            format!("Past chats:\n\n{chats}\n{}", question(inst)),
        ),
    ]
}

/// Indexes the sessions and returns the prompt plus the ids of the sessions retrieved.
fn retrieval_prompt(inst: &Instance, k: usize) -> Result<(Vec<ChatMessage>, Vec<String>), String> {
    let conn = dn_store::open_in_memory(&[dn_store::MigrationSet::new(
        "knowledge",
        dn_knowledge::migrations(),
    )])
    .map_err(|e| e.to_string())?;
    let index = Index::new(&conn);
    let files: Vec<SourceFile> = inst
        .haystack_session_ids
        .iter()
        .zip(&inst.haystack_dates)
        .zip(&inst.haystack_sessions)
        .map(|((id, date), msgs)| SourceFile {
            file_id: id.clone(),
            display_path: format!("chat on {date}"),
            content: render_session(date, msgs).into_bytes(),
        })
        .collect();
    index
        .sync_source("history", &files, None)
        .map_err(|e| e.to_string())?;
    let hits = index
        .search(&["history"], &inst.question, k, None)
        .map_err(|e| e.to_string())?;
    let mut sessions: Vec<String> = Vec::new();
    for h in &hits {
        if !sessions.contains(&h.file_id) {
            sessions.push(h.file_id.clone());
        }
    }
    let (block, _) = dn_knowledge::context_block(&hits);
    let prompt = vec![
        ChatMessage::text(Role::System, format!("{SYSTEM}\n\n{block}")),
        ChatMessage::text(Role::User, question(inst)),
    ];
    Ok((prompt, sessions))
}

/// Share of the answer sessions that were retrieved; `None` without answer sessions.
pub fn session_recall(answer_sessions: &[String], retrieved: &[String]) -> Option<f64> {
    if answer_sessions.is_empty() {
        return None;
    }
    let found = answer_sessions
        .iter()
        .filter(|s| retrieved.contains(s))
        .count();
    Some(found as f64 / answer_sessions.len() as f64)
}

#[derive(Serialize)]
struct Item {
    question_id: String,
    question_type: String,
    abstention: bool,
    /// The question, for the judge.
    question: String,
    /// The expected answer (the judge compares).
    answer: serde_json::Value,
    #[serde(flatten)]
    turn: Turn,
    /// Retrieval mode: the sessions the passages came from, best first.
    #[serde(skip_serializing_if = "Option::is_none")]
    retrieved_sessions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_recall: Option<f64>,
}

pub async fn run(a: LongMemEvalArgs) -> Result<(), String> {
    let bytes = std::fs::read(&a.data).map_err(|e| format!("{}: {e}", a.data.display()))?;
    let data: Vec<Instance> =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", a.data.display()))?;
    drop(bytes);
    for inst in &data {
        inst.check()?;
    }
    let n = a.limit.unwrap_or(data.len()).min(data.len());
    let m = a.model.load(None).await?;
    let caller = match a.mode {
        Mode::History => "eval.longmemeval.history",
        Mode::Retrieval => "eval.longmemeval.retrieval",
    };
    let cfg = eval_config(caller, &m.model_ref, Some(a.max_tokens));
    let mut hyp = match &a.hypotheses {
        Some(p) => Some(std::fs::File::create(p).map_err(|e| format!("{}: {e}", p.display()))?),
        None => None,
    };
    let mut items = Vec::with_capacity(n);
    for (i, inst) in data.iter().take(n).enumerate() {
        let (prompt, retrieved) = match a.mode {
            Mode::History => (history_prompt(inst), None),
            Mode::Retrieval => {
                let (p, s) = retrieval_prompt(inst, a.top_k)?;
                (p, Some(s))
            }
        };
        let t = turn(&m.chat, &cfg, prompt).await;
        let recall = retrieved
            .as_ref()
            .and_then(|r| session_recall(&inst.answer_session_ids, r));
        eprintln!(
            "[eval longmemeval] {}/{n} {}: {:?}, {} input tokens{}",
            i + 1,
            inst.question_id,
            t.result.stop,
            t.result.usage.input_tokens,
            recall
                .map(|r| format!(", session recall {r:.2}"))
                .unwrap_or_default()
        );
        if let Some(f) = &mut hyp {
            let line = serde_json::json!({"question_id": inst.question_id, "hypothesis": t.text});
            writeln!(f, "{line}").map_err(|e| e.to_string())?;
        }
        items.push(Item {
            question_id: inst.question_id.clone(),
            question_type: inst.question_type.clone(),
            abstention: inst.is_abstention(),
            question: inst.question.clone(),
            answer: inst.answer.clone(),
            turn: t,
            retrieved_sessions: retrieved,
            session_recall: recall,
        });
    }
    let failed = items.iter().filter(|i| i.turn.error.is_some()).count();
    let recalls: Vec<f64> = items.iter().filter_map(|i| i.session_recall).collect();
    let mut by_type = BTreeMap::<&str, usize>::new();
    for i in &items {
        *by_type.entry(i.question_type.as_str()).or_default() += 1;
    }
    let summary = serde_json::json!({
        "data": a.data.file_name().map(|s| s.to_string_lossy()),
        "mode": a.mode,
        "search": if a.mode == Mode::Retrieval { Some("keyword") } else { None },
        "top_k": if a.mode == Mode::Retrieval { Some(a.top_k) } else { None },
        "questions": items.len(),
        "failed": failed,
        "by_type": by_type,
        "mean_session_recall": (!recalls.is_empty()).then(|| recalls.iter().sum::<f64>() / recalls.len() as f64),
        "judged": false,
    });
    let report = Report::new("longmemeval", &m, summary, items);
    let path = report.write(a.out)?;
    m.chat.unload().await?;
    println!(
        "{} questions, {failed} failed · report: {}",
        report.items.len(),
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_core::chat::ContentPart;

    trait Text {
        fn content_text(&self) -> String;
    }
    impl Text for ChatMessage {
        fn content_text(&self) -> String {
            self.content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    ContentPart::Image { .. } => None,
                })
                .collect()
        }
    }

    fn inst() -> Instance {
        serde_json::from_value(serde_json::json!({
            "question_id": "q1",
            "question_type": "single-session-user",
            "question": "Which city did I move to?",
            "answer": "Lisbon",
            "question_date": "2023/05/30 (Tue) 10:00",
            "haystack_session_ids": ["s1", "s2", "s3"],
            "haystack_dates": ["2023/05/01", "2023/05/10", "2023/05/20"],
            "haystack_sessions": [
                [{"role": "user", "content": "Any tips for growing basil on a balcony?"},
                 {"role": "assistant", "content": "Lots of sun and regular water."}],
                [{"role": "user", "content": "I just moved to Lisbon and need a dentist.", "has_answer": true},
                 {"role": "assistant", "content": "Here is how to find one."}],
                [{"role": "user", "content": "Recommend a sci-fi novel."},
                 {"role": "assistant", "content": "Try a classic."}]
            ],
            "answer_session_ids": ["s2"]
        }))
        .unwrap()
    }

    #[test]
    fn retrieval_finds_the_answer_session_and_quotes_it_as_untrusted() {
        let i = inst();
        i.check().unwrap();
        let (prompt, sessions) = retrieval_prompt(&i, 2).unwrap();
        assert_eq!(sessions.first().map(String::as_str), Some("s2"));
        assert_eq!(session_recall(&i.answer_session_ids, &sessions), Some(1.0));
        let system = prompt[0].content_text();
        assert!(system.contains("Lisbon"));
        assert!(system.contains("chat on 2023/05/10"), "{system}");
        assert_eq!(prompt[1].role, Role::User);
        assert!(
            prompt[1]
                .content_text()
                .contains("Which city did I move to?")
        );
    }

    #[test]
    fn history_mode_puts_every_session_in_order() {
        let p = history_prompt(&inst());
        let u = p[1].content_text();
        let (a, b, c) = (
            u.find("basil").unwrap(),
            u.find("Lisbon").unwrap(),
            u.find("sci-fi").unwrap(),
        );
        assert!(a < b && b < c);
    }

    #[test]
    fn mismatched_sessions_are_refused_and_recall_handles_no_answer_sessions() {
        let mut i = inst();
        i.haystack_dates.pop();
        assert!(
            i.check()
                .unwrap_err()
                .contains("3 sessions but 3 ids and 2 dates")
        );
        assert_eq!(session_recall(&[], &["s1".into()]), None);
        assert_eq!(
            session_recall(&["a".into(), "b".into()], &["b".into()]),
            Some(0.5)
        );
    }
}
