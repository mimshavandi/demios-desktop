//! The speed suite (ADR-0020 §8): time to first token and tokens per second, p50/p95, cold
//! and warm, measured through the agent loop from each run's typed result.
//!
//! Cases come from a `dn.speed.v1` file (default: `apps/cli/eval/speed.default.json`), so new
//! cases (longer prefixes, switching chats, restarts with restored slots, a draft model) are
//! added as data plus, for a new `kind`, a runner arm here.
//!
//! - `after_load`: the first request after loading, once (includes any lazy start-up). It
//!   must be the first case.
//! - `single`, `cache: cold`: every rep sends a prompt with a different first line, so no
//!   prefix can be reused.
//! - `single`, `cache: warm`: one priming request, then every rep sends that same prompt, so
//!   a backend that reuses prefixes can. `cached_input_tokens` shows whether it did.

use super::{ModelArgs, Report, Turn, eval_config, percentile, turn};
use crate::bench::{bench_ctx, chat_prompt};
use clap::Args;
use dn_core::chat::{ChatMessage, ChatModel, Role};
use dn_core::model::ModelRef;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "dn.speed.v1";

#[derive(Args)]
pub struct SpeedArgs {
    #[command(flatten)]
    model: ModelArgs,
    /// The cases (`dn.speed.v1`); default: the built-in set.
    #[arg(long)]
    cases: Option<PathBuf>,
    /// Report path (default: eval-speed-<time>.json).
    #[arg(long, short)]
    out: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseFile {
    pub format: String,
    #[serde(default)]
    pub about: String,
    pub cases: Vec<Case>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub name: String,
    #[serde(default = "single")]
    pub kind: String,
    /// Approximate prompt tokens; the report has the real counts.
    pub prompt_tokens: u32,
    pub gen_tokens: u32,
    #[serde(default)]
    pub cache: Cache,
    #[serde(default = "five")]
    pub reps: u32,
}

fn single() -> String {
    "single".into()
}
fn five() -> u32 {
    5
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Cache {
    #[default]
    Cold,
    Warm,
}

/// The kinds this runner knows.
pub const KINDS: &[&str] = &["after_load", "single"];

const BUILTIN: &str = include_str!("../../eval/speed.default.json");

impl CaseFile {
    pub fn parse(json: &str) -> Result<Self, String> {
        let f: CaseFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
        f.check()?;
        Ok(f)
    }

    fn read(path: Option<&Path>) -> Result<Self, String> {
        match path {
            None => Self::parse(BUILTIN),
            Some(p) => {
                let s = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
                Self::parse(&s).map_err(|e| format!("{}: {e}", p.display()))
            }
        }
    }

    pub fn check(&self) -> Result<(), String> {
        if self.format != FORMAT {
            return Err(format!("format is `{}`, expected `{FORMAT}`", self.format));
        }
        if self.cases.is_empty() {
            return Err("no cases".into());
        }
        let mut names = HashSet::new();
        for (i, c) in self.cases.iter().enumerate() {
            if !names.insert(c.name.as_str()) {
                return Err(format!("case `{}` appears twice", c.name));
            }
            if !KINDS.contains(&c.kind.as_str()) {
                return Err(format!(
                    "case `{}`: kind `{}` isn't run by this version (known: {})",
                    c.name,
                    c.kind,
                    KINDS.join(", ")
                ));
            }
            if c.kind == "after_load" && i != 0 {
                return Err(format!(
                    "case `{}`: after_load must be the first case",
                    c.name
                ));
            }
            if c.gen_tokens < 2 || c.reps == 0 {
                return Err(format!(
                    "case `{}`: needs gen_tokens ≥ 2 and reps ≥ 1",
                    c.name
                ));
            }
        }
        Ok(())
    }

    /// Context that holds the largest case.
    fn ctx(&self) -> u32 {
        self.cases
            .iter()
            .map(|c| bench_ctx(None, c.prompt_tokens, c.gen_tokens))
            .max()
            .unwrap_or(0)
    }
}

/// One timed request.
#[derive(Debug, Serialize)]
pub struct Rep {
    pub ttft_ms: Option<u64>,
    /// Output tokens after the first, over the time after the first.
    pub gen_tokens_per_s: Option<f64>,
    #[serde(flatten)]
    pub turn: TurnSummary,
}

/// A turn without its text (the speed prompts' answers aren't interesting).
#[derive(Debug, Serialize)]
pub struct TurnSummary {
    pub result: dn_agent::RunResult,
    pub error: Option<String>,
}

pub fn rep_of(t: Turn) -> Rep {
    let r = &t.result;
    let tps = match r.first_token_ms {
        Some(first) if r.wall_ms > first && r.usage.output_tokens > 1 => {
            Some(f64::from(r.usage.output_tokens - 1) / ((r.wall_ms - first) as f64 / 1e3))
        }
        _ => None,
    };
    Rep {
        ttft_ms: r.first_token_ms,
        gen_tokens_per_s: tps,
        turn: TurnSummary {
            result: t.result,
            error: t.error,
        },
    }
}

#[derive(Debug, Serialize)]
pub struct CaseResult {
    #[serde(flatten)]
    pub case: Case,
    pub ttft_ms_p50: Option<f64>,
    pub ttft_ms_p95: Option<f64>,
    pub gen_tokens_per_s_p50: Option<f64>,
    pub gen_tokens_per_s_p95: Option<f64>,
    /// Mean over the reps (0 until a backend reuses prefixes).
    pub cached_input_tokens_mean: f64,
    pub failed: usize,
    pub reps: Vec<Rep>,
}

impl CaseResult {
    pub fn new(case: Case, reps: Vec<Rep>) -> Self {
        let ttft: Vec<f64> = reps
            .iter()
            .filter_map(|r| r.ttft_ms.map(|t| t as f64))
            .collect();
        let tps: Vec<f64> = reps.iter().filter_map(|r| r.gen_tokens_per_s).collect();
        let cached: Vec<f64> = reps
            .iter()
            .map(|r| f64::from(r.turn.result.usage.cached_input_tokens))
            .collect();
        Self {
            case,
            ttft_ms_p50: percentile(&ttft, 50.0),
            ttft_ms_p95: percentile(&ttft, 95.0),
            gen_tokens_per_s_p50: percentile(&tps, 50.0),
            gen_tokens_per_s_p95: percentile(&tps, 95.0),
            cached_input_tokens_mean: if cached.is_empty() {
                0.0
            } else {
                cached.iter().sum::<f64>() / cached.len() as f64
            },
            failed: reps.iter().filter(|r| r.turn.error.is_some()).count(),
            reps,
        }
    }
}

fn prompt(c: &Case, variant: u32) -> Vec<ChatMessage> {
    vec![ChatMessage::text(
        Role::User,
        chat_prompt(c.prompt_tokens, variant),
    )]
}

/// Runs the cases in order. `variant` numbers keep cold prompts distinct across the run.
pub async fn run_cases(
    model: &dyn ChatModel,
    model_ref: &ModelRef,
    cases: &[Case],
) -> Vec<CaseResult> {
    let mut variant = 0u32;
    let mut next = || {
        variant += 1;
        variant
    };
    let mut out = Vec::new();
    for c in cases {
        let cfg = eval_config("eval.speed", model_ref, Some(c.gen_tokens));
        let mut reps = Vec::new();
        match (c.kind.as_str(), c.cache) {
            ("after_load", _) => reps.push(rep_of(turn(model, &cfg, prompt(c, next())).await)),
            (_, Cache::Cold) => {
                for _ in 0..c.reps {
                    reps.push(rep_of(turn(model, &cfg, prompt(c, next())).await));
                }
            }
            (_, Cache::Warm) => {
                let v = next();
                let _priming = turn(model, &cfg, prompt(c, v)).await;
                for _ in 0..c.reps {
                    reps.push(rep_of(turn(model, &cfg, prompt(c, v)).await));
                }
            }
        }
        let r = CaseResult::new(c.clone(), reps);
        eprintln!(
            "[eval speed] {}: first token p50 {} ms, p95 {} ms · {} tok/s p50 · cached {:.0}",
            c.name,
            fmt(r.ttft_ms_p50),
            fmt(r.ttft_ms_p95),
            fmt(r.gen_tokens_per_s_p50),
            r.cached_input_tokens_mean
        );
        out.push(r);
    }
    out
}

fn fmt(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}")).unwrap_or_else(|| "–".into())
}

pub async fn run(a: SpeedArgs) -> Result<(), String> {
    let file = CaseFile::read(a.cases.as_deref())?;
    let m = a.model.load(Some(file.ctx())).await?;
    let results = run_cases(&m.chat, &m.model_ref, &file.cases).await;
    let failed: usize = results.iter().map(|r| r.failed).sum();
    let summary = serde_json::json!({
        "about": file.about,
        "cases_file": a.cases.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy()).unwrap_or("built-in".into()),
        "cases": results.len(),
        "failed_reps": failed,
    });
    let report = Report::new("speed", &m, summary, results);
    let path = report.write(a.out)?;
    m.chat.unload().await?;
    println!(
        "{} cases, {failed} failed reps · report: {}",
        report.items.len(),
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_core::chat::{ChatRequest, EventStream, FinishReason, StreamEvent, Usage};
    use std::sync::Mutex;

    #[test]
    fn the_built_in_cases_are_valid_and_bad_ones_are_refused() {
        let f = CaseFile::parse(BUILTIN).unwrap();
        assert_eq!(f.cases[0].kind, "after_load");
        assert!(f.cases.iter().any(|c| c.cache == Cache::Warm));
        let one = |c: &str| format!(r#"{{"format":"dn.speed.v1","cases":[{c}]}}"#);
        assert!(
            CaseFile::parse(&one(
                r#"{"name":"x","kind":"restart","prompt_tokens":8,"gen_tokens":8}"#
            ))
            .unwrap_err()
            .contains("isn't run by this version")
        );
        let late = r#"{"format":"dn.speed.v1","cases":[
            {"name":"a","prompt_tokens":8,"gen_tokens":8},
            {"name":"b","kind":"after_load","prompt_tokens":8,"gen_tokens":8}]}"#;
        assert!(CaseFile::parse(late).unwrap_err().contains("first case"));
    }

    /// Answers with a few tokens and remembers the prompts it got.
    #[derive(Default)]
    struct Echo(Mutex<Vec<String>>);
    struct Items(std::collections::VecDeque<Result<StreamEvent, dn_core::CoreError>>);
    impl futures_core::Stream for Items {
        type Item = Result<StreamEvent, dn_core::CoreError>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(self.0.pop_front())
        }
    }
    impl ChatModel for Echo {
        fn chat(&self, req: ChatRequest) -> EventStream {
            let text: String = req
                .messages
                .iter()
                .flat_map(|m| m.content.iter())
                .map(|p| format!("{p:?}"))
                .collect();
            self.0.lock().unwrap().push(text);
            Box::pin(Items(
                vec![
                    Ok(StreamEvent::TextDelta { text: "a".into() }),
                    Ok(StreamEvent::TextDelta { text: "b".into() }),
                    Ok(StreamEvent::Done {
                        finish_reason: FinishReason::Stop,
                        usage: Usage {
                            input_tokens: 20,
                            output_tokens: 3,
                            ..Default::default()
                        },
                    }),
                ]
                .into(),
            ))
        }
    }

    #[tokio::test]
    async fn cold_reps_send_distinct_prompts_and_warm_reps_repeat_the_primed_one() {
        let f = CaseFile::parse(
            r#"{"format":"dn.speed.v1","cases":[
                {"name":"load","kind":"after_load","prompt_tokens":16,"gen_tokens":4},
                {"name":"cold","prompt_tokens":16,"gen_tokens":4,"reps":3},
                {"name":"warm","prompt_tokens":16,"gen_tokens":4,"cache":"warm","reps":3}]}"#,
        )
        .unwrap();
        let m = Echo::default();
        let model_ref = ModelRef::Local {
            model_id: "echo".into(),
        };
        let r = run_cases(&m, &model_ref, &f.cases).await;
        assert_eq!(
            r.iter().map(|c| c.reps.len()).collect::<Vec<_>>(),
            [1, 3, 3]
        );
        let sent = m.0.lock().unwrap().clone();
        // 1 after-load + 3 cold + 1 priming + 3 warm.
        assert_eq!(sent.len(), 8);
        let cold: HashSet<&String> = sent[1..4].iter().collect();
        assert_eq!(cold.len(), 3, "cold prompts differ");
        assert!(
            sent[4..].iter().all(|p| p == &sent[4]),
            "warm prompts repeat the primed one"
        );
        assert!(!cold.contains(&sent[4]));
        let rep = &r[1].reps[0];
        assert_eq!(rep.turn.result.usage.output_tokens, 3);
        assert!(rep.ttft_ms.is_some());
        assert_eq!(r[1].failed, 0);
        assert!(r[1].ttft_ms_p50.is_some() && r[1].ttft_ms_p95 >= r[1].ttft_ms_p50);
    }

    #[test]
    fn tokens_per_second_counts_the_tokens_after_the_first() {
        let mut res = dn_agent::RunMeter::start(
            dn_core::Id::new(),
            "eval.speed",
            ModelRef::Local {
                model_id: "m".into(),
            },
        )
        .finish(dn_agent::StopReason::Completed);
        res.first_token_ms = Some(100);
        res.wall_ms = 1100;
        res.usage.output_tokens = 51;
        let r = rep_of(Turn {
            text: String::new(),
            result: res,
            error: None,
        });
        assert_eq!(r.gen_tokens_per_s, Some(50.0));
    }
}
