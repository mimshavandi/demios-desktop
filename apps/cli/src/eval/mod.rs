//! `dn eval` (ADR-0020 §8, R0): measures what Demios does with a model, through the same
//! agent loop the app runs, so every case comes with its typed run result.
//!
//! Suites:
//! - `parity`: a golden-prompt set (prompts/, files/, tools.yaml) answered with mock tools;
//!   answers, tool calls and run results are recorded for the judge. A private holdout stays
//!   outside any repository.
//! - `longmemeval`: LongMemEval (MIT) questions over a long chat history, answered by
//!   reading the history directly or through our own retrieval (dn-knowledge).
//! - `speed`: time to first token, tokens per second, p50/p95, cold and warm, from a list of
//!   cases (format `dn.speed.v1`).
//!
//! - `judge`: scores a parity answers report with the fixed judge named in `judge.yaml`
//!   (#355); a separate step, and the only one that sends anything (to the judge).
//! - `calibrate`: the pairwise judge's agreement with people (Cohen's κ) over labelled pairs.
//! - `references`: reference answers from Claude or ChatGPT (#407), sent to that provider
//!   with the owner's key, only when run.
//!
//! Every answering run writes one JSON report (schema `dn.eval.v1`), never anything else, and
//! sends nothing anywhere: the model is a local GGUF or the endpoint you name.

mod calibrate;
mod context;
mod judge;
mod judge_memory;
mod longmemeval;
mod parity;
mod references;
mod speed;

use crate::LoadArgs;
use crate::bench::{gguf_json, machine_json, now};
use clap::{Args, Subcommand};
use dn_agent::{CancelToken, LoopConfig, NoTools, RunMeter, RunResult, StopReason, run_loop};
use dn_core::chat::{ChatMessage, ChatModel, Reasoning};
use dn_core::model::ModelRef;
use dn_inference::{LlamaCppBackend, LoadedModel, ModelSpec, OpenAiCompatBackend};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

#[derive(Args)]
pub struct EvalArgs {
    #[command(subcommand)]
    suite: Suite,
}

#[derive(Subcommand)]
enum Suite {
    /// Answer a golden-prompt set with mock tools; record answers, tool calls and run
    /// results for the judge.
    Parity(parity::ParityArgs),
    /// LongMemEval: questions over a long chat history (data via
    /// scripts/eval/fetch-longmemeval.sh; not in this repository).
    Longmemeval(longmemeval::LongMemEvalArgs),
    /// Time to first token and tokens per second, p50/p95, cold and warm.
    Speed(speed::SpeedArgs),
    /// Judge a parity or LongMemEval report with the fixed judge (`judge.yaml`). The only
    /// step that sends anything, to the judge's endpoint.
    Judge(judge::JudgeArgs),
    /// How far the pairwise judge agrees with people: agreement and Cohen's κ over a
    /// labels file of answer pairs (dn.labels.v1).
    Calibrate(calibrate::CalibrateArgs),
    /// Reference answers for pairwise judging, from Claude or ChatGPT through their APIs
    /// (the owner's keys): each case answered as parity answers it.
    References(references::ReferencesArgs),
    /// How much of its context a local model uses well: retrieval (needle, multikey) and
    /// combining facts (multihop, count) at growing lengths (#350).
    Context(context::ContextArgs),
}

pub async fn run(a: EvalArgs) -> Result<(), String> {
    match a.suite {
        Suite::Parity(a) => parity::run(a).await,
        Suite::Longmemeval(a) => longmemeval::run(a).await,
        Suite::Speed(a) => speed::run(a).await,
        Suite::Judge(a) => judge::run(a).await,
        Suite::Calibrate(a) => calibrate::run(a).await,
        Suite::References(a) => references::run(a).await,
        Suite::Context(a) => context::run(a).await,
    }
}

/// Which model answers: a GGUF (path or downloaded catalogue id) loaded in-process, or a
/// model on an OpenAI-compatible server.
#[derive(Args)]
pub struct ModelArgs {
    /// GGUF path or downloaded catalogue id; with --endpoint, the server's model name.
    model: String,
    /// Use an OpenAI-compatible server at this base URL instead of loading in-process.
    #[arg(long)]
    endpoint: Option<String>,
    /// Environment variable holding the endpoint's API key.
    #[arg(long, requires = "endpoint")]
    api_key_env: Option<String>,
    #[command(flatten)]
    load: LoadArgs,
}

/// The model, in-process or on a server.
pub enum Loaded {
    Local(LoadedModel<LlamaCppBackend>),
    Endpoint(LoadedModel<OpenAiCompatBackend>),
}

impl ChatModel for Loaded {
    fn chat(&self, req: dn_core::chat::ChatRequest) -> dn_core::chat::EventStream {
        match self {
            Self::Local(m) => m.chat(req),
            Self::Endpoint(m) => m.chat(req),
        }
    }

    fn count_tokens(
        &self,
        req: &dn_core::chat::ChatRequest,
    ) -> Option<Result<u32, dn_core::CoreError>> {
        match self {
            Self::Local(m) => m.count_tokens(req),
            Self::Endpoint(m) => m.count_tokens(req),
        }
    }
}

impl Loaded {
    pub async fn unload(self) -> Result<(), String> {
        match self {
            Self::Local(m) => m.unload().await,
            Self::Endpoint(m) => m.unload().await,
        }
        .map_err(|e| e.to_string())
    }
}

/// A loaded model and what the report says about it.
pub struct Model {
    pub chat: Loaded,
    pub model_ref: ModelRef,
    pub backend: &'static str,
    pub info: serde_json::Value,
    pub load_ms: Option<f64>,
}

impl ModelArgs {
    /// The model as given (a GGUF path or catalogue id; with `--endpoint`, the server's name).
    pub fn model_name(&self) -> String {
        self.model.clone()
    }

    /// Loads the model. `min_ctx`: context the suite needs at least (in-process only).
    pub async fn load(&self, min_ctx: Option<u32>) -> Result<Model, String> {
        if let Some(ep) = &self.endpoint {
            let key = match &self.api_key_env {
                Some(v) => Some(std::env::var(v).map_err(|_| format!("${v} is not set"))?),
                None => None,
            };
            let chat = LoadedModel::load(
                Arc::new(OpenAiCompatBackend::new()),
                &ModelSpec::endpoint(ep, &self.model, key),
                &Default::default(),
            )
            .await
            .map_err(|e| e.to_string())?;
            return Ok(Model {
                chat: Loaded::Endpoint(chat),
                model_ref: ModelRef::Remote {
                    endpoint_id: "eval".into(),
                    model_id: self.model.clone(),
                },
                backend: "openai_compat",
                info: serde_json::json!({ "server_model": self.model }),
                load_ms: None,
            });
        }
        let path = crate::resolve_model(&self.model)?;
        let checked = self.load.check(&path).await?;
        let load = match (min_ctx, self.load.ctx()) {
            (Some(min), None) => self.load.with_ctx(min),
            _ => self.load.clone(),
        };
        let (mut opts, _) = load.auto_options(&path)?;
        opts.chat_template = checked.confirm()?;
        let t0 = Instant::now();
        let chat = LoadedModel::load(
            Arc::new(LlamaCppBackend::new()),
            &ModelSpec::gguf(&path),
            &opts,
        )
        .await
        .map_err(|e| e.to_string())?;
        let load_ms = t0.elapsed().as_secs_f64() * 1e3;
        let mut info = gguf_json(&path)?;
        info["options"] = serde_json::to_value(&opts).unwrap_or_default();
        Ok(Model {
            chat: Loaded::Local(chat),
            model_ref: ModelRef::Local {
                model_id: path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            },
            backend: "llamacpp",
            info,
            load_ms: Some(load_ms),
        })
    }
}

/// One answered case.
#[derive(Clone, Debug, Serialize)]
pub struct Turn {
    pub text: String,
    pub result: RunResult,
    /// The run's error, when it failed (the suite goes on with the next case).
    pub error: Option<String>,
}

/// Loop settings for eval runs: deterministic, no thinking, one model call (no tools yet).
pub fn eval_config(caller: &str, model: &ModelRef, max_tokens: Option<u32>) -> LoopConfig {
    let mut cfg = LoopConfig::new("eval", model.clone());
    cfg.caller = caller.into();
    cfg.max_steps = 1;
    cfg.max_tokens = max_tokens;
    cfg.temperature = Some(0.0);
    cfg.reasoning = Reasoning::Off;
    cfg
}

/// Runs one case through the agent loop (the app's path), without tools.
pub async fn turn(model: &dyn ChatModel, cfg: &LoopConfig, history: Vec<ChatMessage>) -> Turn {
    turn_with(model, &NoTools, cfg, history).await
}

/// [`turn`] with tools.
pub async fn turn_with(
    model: &dyn ChatModel,
    tools: &dyn dn_agent::ToolExecutor,
    cfg: &LoopConfig,
    history: Vec<ChatMessage>,
) -> Turn {
    let mut meter = RunMeter::start(dn_core::Id::new(), cfg.caller.clone(), cfg.model.clone());
    let r = run_loop(model, tools, cfg, history, &CancelToken::new(), &mut |e| {
        dn_agent::observe_event(&mut meter, &e)
    })
    .await;
    match r {
        Ok(o) => Turn {
            text: o.final_text,
            result: o.result,
            error: None,
        },
        Err(e) => Turn {
            text: String::new(),
            result: meter.finish(StopReason::Failed),
            error: Some(e.to_string()),
        },
    }
}

/// The report every suite writes (schema `dn.eval.v1`).
#[derive(Serialize)]
pub struct Report<T: Serialize> {
    pub schema: &'static str,
    pub suite: &'static str,
    pub dn_version: &'static str,
    pub unix_time: u64,
    pub backend: &'static str,
    pub model: serde_json::Value,
    pub machine: serde_json::Value,
    pub load_ms: Option<f64>,
    /// Suite-specific totals.
    pub summary: serde_json::Value,
    pub items: Vec<T>,
}

impl<T: Serialize> Report<T> {
    pub fn new(suite: &'static str, m: &Model, summary: serde_json::Value, items: Vec<T>) -> Self {
        Self {
            schema: "dn.eval.v1",
            suite,
            dn_version: env!("CARGO_PKG_VERSION"),
            unix_time: now(),
            backend: m.backend,
            model: m.info.clone(),
            machine: machine_json(),
            load_ms: m.load_ms,
            summary,
            items,
        }
    }

    /// Writes the report to `out` (default `eval-<suite>-<unix time>.json`) and returns the
    /// path.
    pub fn write(&self, out: Option<PathBuf>) -> Result<PathBuf, String> {
        let out = out.unwrap_or_else(|| {
            PathBuf::from(format!("eval-{}-{}.json", self.suite, self.unix_time))
        });
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&out, json + "\n").map_err(|e| format!("{}: {e}", out.display()))?;
        Ok(out)
    }
}

/// The `p`-th percentile (0–100) by nearest rank; `None` for no values.
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * v.len() as f64).ceil().max(1.0) as usize;
    Some(v[rank.min(v.len()) - 1])
}

/// Whether `path` (a file that may not exist yet) is inside a git work tree. Private eval
/// data and its results must stay outside the repository, so they can't be committed.
pub fn in_git_work_tree(path: &Path) -> bool {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    abs.ancestors().skip(1).any(|d| d.join(".git").exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_by_nearest_rank() {
        let v = [5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(percentile(&v, 50.0), Some(3.0));
        assert_eq!(percentile(&v, 95.0), Some(5.0));
        assert_eq!(percentile(&v, 0.0), Some(1.0));
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(percentile(&[f64::NAN, 2.0], 50.0), Some(2.0));
    }

    #[test]
    fn this_repository_is_a_work_tree_and_a_temp_folder_is_not() {
        assert!(in_git_work_tree(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("x.json")
                .as_path()
        ));
        let tmp = std::env::temp_dir().join(format!("dn-eval-{}", dn_core::Id::new()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(!in_git_work_tree(&tmp.join("holdout.json")));
        let _ = std::fs::remove_dir_all(tmp);
    }
}
