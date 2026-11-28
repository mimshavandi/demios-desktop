//! `dn bench`: throughput, peak memory and (optionally) a small quality set, written as a
//! JSON report (schema `dn.bench.v2`; `dn bench-report` turns reports into a table).
//!
//! Two ways to measure (T-INF-21):
//! - **In-process llama.cpp** (a GGUF path or catalogue id): llama-bench-style runs with
//!   exact token counts (EOS ignored), plus the chat-path runs below.
//! - **An OpenAI-compatible server** (`--endpoint`, e.g. mistral.rs or llama-server): only
//!   the chat-path runs. `--server-pid` samples that process's memory.
//!
//! Chat-path runs send the same long prompt through `ChatModel` and time the stream: time to
//! the first token (prompt processing plus scheduling) and generation speed after it. They
//! are what a user feels, and they compare backends on equal terms.

use crate::{LoadArgs, machine, quality, resolve_model};
use clap::Args;
use dn_core::chat::{ChatMessage, ChatModel, ChatRequest, Reasoning, Role, StreamEvent};
use dn_core::model::ModelRef;
use dn_inference::llamacpp::BenchRun;
use dn_inference::{LlamaCppBackend, LoadedModel, ModelSpec, OpenAiCompatBackend, next_event};
use dn_models::{Gguf, ModelFootprint};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct BenchArgs {
    /// GGUF path or downloaded catalogue id; with --endpoint, the server's model name.
    model: String,
    /// Prompt tokens per run (approximate on the chat path).
    #[arg(long, default_value_t = 512)]
    n_prompt: u32,
    /// Generated tokens per run.
    #[arg(long, default_value_t = 128)]
    n_gen: u32,
    /// Measured runs (after one warm-up run).
    #[arg(long, default_value_t = 3)]
    reps: u32,
    /// Also run the small quality set (14 prompts with checkable answers).
    #[arg(long)]
    quality: bool,
    /// Benchmark an OpenAI-compatible server at this base URL (e.g.
    /// http://127.0.0.1:1234/v1) instead of loading the model in-process.
    #[arg(long)]
    endpoint: Option<String>,
    /// Environment variable holding the endpoint's API key.
    #[arg(long, requires = "endpoint")]
    api_key_env: Option<String>,
    /// With --endpoint: the server's process id, to sample its peak memory.
    #[arg(long, requires = "endpoint")]
    server_pid: Option<u32>,
    /// With --endpoint: the backend name for the report (e.g. mistralrs, llama-server).
    #[arg(long, requires = "endpoint", default_value = "openai_compat")]
    backend_name: String,
    /// With --endpoint: the GGUF the server runs, recorded in the report so rows can be
    /// compared with in-process runs of the same file.
    #[arg(long, requires = "endpoint")]
    gguf: Option<PathBuf>,
    /// Report path (default: bench-<model>[-<backend>].json).
    #[arg(long, short)]
    out: Option<PathBuf>,
    #[command(flatten)]
    load: LoadArgs,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stats {
    pub mean: f64,
    pub stddev: f64,
}

fn stats(v: &[f64]) -> Stats {
    let n = v.len().max(1) as f64;
    let mean = v.iter().sum::<f64>() / n;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    Stats {
        mean,
        stddev: var.sqrt(),
    }
}

/// One chat-path measurement.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatRun {
    pub prompt_tokens: u32,
    pub gen_tokens: u32,
    /// Request sent → first text or reasoning token.
    pub ttft_ms: f64,
    /// Tokens after the first, over the time after the first.
    pub gen_tokens_per_s: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatStats {
    pub runs: Vec<ChatRun>,
    pub ttft_ms: Stats,
    pub gen_tokens_per_s: Stats,
}

#[derive(Serialize)]
struct Report {
    schema: &'static str,
    dn_version: &'static str,
    unix_time: u64,
    backend: String,
    model: serde_json::Value,
    machine: serde_json::Value,
    options: Option<dn_inference::LoadOptions>,
    load_ms: Option<f64>,
    n_prompt: u32,
    n_gen: u32,
    /// llama-bench-style runs (in-process llama.cpp only).
    runs: Vec<BenchRun>,
    prompt_tokens_per_s: Option<Stats>,
    gen_tokens_per_s: Option<Stats>,
    chat: ChatStats,
    quality: Option<quality::QualityResult>,
    /// Highest resident set size, sampled every 20 ms: of `dn` in-process, of the server with
    /// --server-pid, else absent. On unified memory, GPU buffers backed by the mmapped model
    /// count here too.
    peak_rss_bytes: Option<u64>,
}

/// Samples a process's RSS on a background thread until finished.
pub(crate) struct PeakMem {
    peak: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl PeakMem {
    pub(crate) fn start(pid: sysinfo::Pid) -> Self {
        let peak = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (peak.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            let mut sys = sysinfo::System::new();
            while !s.load(Ordering::Relaxed) {
                sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), false);
                if let Some(proc_) = sys.process(pid) {
                    p.fetch_max(proc_.memory(), Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        Self {
            peak,
            stop,
            handle: Some(handle),
        }
    }

    pub(crate) fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.peak.load(Ordering::Relaxed)
    }
}

/// Context for a bench run: `--ctx` if it holds one run, else just enough for it. The chat
/// path's prompt is approximate and wrapped in a chat template, so leave room for that.
pub fn bench_ctx(user: Option<u32>, n_prompt: u32, n_gen: u32) -> u32 {
    user.unwrap_or(0).max(n_prompt + n_prompt / 4 + n_gen + 64)
}

/// About `n_prompt` tokens of plain English (≈ 16 tokens per line in common tokenizers) and
/// a request that keeps the model writing. Reports record the real token counts. `run` makes
/// the first line differ between runs, so a server can't reuse a cached prompt.
pub fn chat_prompt(n_prompt: u32, run: u32) -> String {
    let mut s = format!("Inventory list number {run}.\n");
    for i in 1..=n_prompt.div_ceil(16).max(1) {
        s.push_str(&format!(
            "Item {i}: a box of tools, a lamp and a blue chair.\n"
        ));
    }
    s.push_str(
        "Write a long, detailed story of at least 2000 words that mentions every item above, \
         in order. Do not stop early.",
    );
    s
}

/// Time one streamed reply to [`chat_prompt`].
async fn chat_once(
    model: &dyn ChatModel,
    model_ref: &ModelRef,
    n_prompt: u32,
    n_gen: u32,
    run: u32,
) -> Result<ChatRun, String> {
    let t0 = Instant::now();
    let mut s = model.chat(ChatRequest {
        model: model_ref.clone(),
        messages: vec![ChatMessage::text(Role::User, chat_prompt(n_prompt, run))],
        tools: vec![],
        max_tokens: Some(n_gen),
        temperature: Some(0.0),
        response_schema: None,
        reasoning: Reasoning::Off,
        caller: "bench".into(),
    });
    let (mut first, mut usage) = (None, None);
    while let Some(ev) = next_event(&mut s).await {
        match ev.map_err(|e| e.to_string())? {
            StreamEvent::TextDelta { .. } | StreamEvent::ReasoningDelta { .. } => {
                first.get_or_insert_with(Instant::now);
            }
            StreamEvent::Done { usage: u, .. } => usage = Some(u),
            StreamEvent::ToolCall { .. } => {}
        }
    }
    let end = Instant::now();
    let usage = usage.ok_or("the stream ended without a usage report")?;
    let first = first.ok_or("the model returned no text")?;
    Ok(chat_run(
        usage.input_tokens,
        usage.output_tokens,
        first - t0,
        end - first,
    ))
}

fn chat_run(prompt_tokens: u32, gen_tokens: u32, ttft: Duration, rest: Duration) -> ChatRun {
    let after_first = gen_tokens.saturating_sub(1) as f64;
    ChatRun {
        prompt_tokens,
        gen_tokens,
        ttft_ms: ttft.as_secs_f64() * 1e3,
        gen_tokens_per_s: if rest.is_zero() {
            0.0
        } else {
            after_first / rest.as_secs_f64()
        },
    }
}

async fn chat_runs(
    model: &dyn ChatModel,
    model_ref: &ModelRef,
    a: &BenchArgs,
) -> Result<ChatStats, String> {
    eprintln!("[bench] chat path: warm-up…");
    chat_once(model, model_ref, a.n_prompt.min(64), a.n_gen.min(16), 0).await?;
    let mut runs = vec![];
    for i in 0..a.reps.max(1) {
        let r = chat_once(model, model_ref, a.n_prompt, a.n_gen, i + 1).await?;
        eprintln!(
            "[bench] chat run {}: {} prompt tokens · first token {:.0} ms · {} tokens at {:.1} tok/s",
            i + 1,
            r.prompt_tokens,
            r.ttft_ms,
            r.gen_tokens,
            r.gen_tokens_per_s
        );
        runs.push(r);
    }
    Ok(ChatStats {
        ttft_ms: stats(&runs.iter().map(|r| r.ttft_ms).collect::<Vec<_>>()),
        gen_tokens_per_s: stats(&runs.iter().map(|r| r.gen_tokens_per_s).collect::<Vec<_>>()),
        runs,
    })
}

pub(crate) fn machine_json() -> serde_json::Value {
    let mut sys = sysinfo::System::new();
    sys.refresh_cpu_all();
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_default();
    serde_json::json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpu": cpu,
        "cpu_threads": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "memory": machine(None, None, false),
        "gpu_offload": LlamaCppBackend::supports_gpu(),
        "devices": LlamaCppBackend::devices().unwrap_or_default(),
    })
}

pub(crate) fn gguf_json(path: &std::path::Path) -> Result<serde_json::Value, String> {
    let g = Gguf::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fp = ModelFootprint::from_gguf(&g);
    // The file name only: a full path names the user's home folder, and reports get shared.
    Ok(serde_json::json!({
        "file": path.file_name().map(|s| s.to_string_lossy()),
        "file_size": g.file_size,
        "arch": g.arch(),
        "quant": fp.quant,
        "n_params": fp.n_params,
        "n_layer": fp.n_layer,
        "moe": fp.is_moe(),
    }))
}

pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub async fn run(a: BenchArgs) -> Result<(), String> {
    let report = match &a.endpoint {
        Some(ep) => run_endpoint(&a, ep).await?,
        None => run_local(&a).await?,
    };
    let out = a.out.clone().unwrap_or_else(|| {
        let stem = std::path::Path::new(&a.model)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| a.model.clone())
            .replace(['/', '\\', ':'], "_");
        PathBuf::from(if a.endpoint.is_some() {
            format!("bench-{stem}-{}.json", a.backend_name)
        } else {
            format!("bench-{stem}.json")
        })
    });
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&out, json + "\n").map_err(|e| format!("{}: {e}", out.display()))?;
    if let (Some(pp), Some(tg)) = (&report.prompt_tokens_per_s, &report.gen_tokens_per_s) {
        println!(
            "pp{} {:.1} ± {:.1} tok/s · tg{} {:.1} ± {:.1} tok/s",
            a.n_prompt, pp.mean, pp.stddev, a.n_gen, tg.mean, tg.stddev
        );
    }
    println!(
        "chat: first token {:.0} ms · {:.1} ± {:.1} tok/s{}{}",
        report.chat.ttft_ms.mean,
        report.chat.gen_tokens_per_s.mean,
        report.chat.gen_tokens_per_s.stddev,
        report
            .peak_rss_bytes
            .map(|b| format!(" · peak RSS {:.2} GiB", b as f64 / (1u64 << 30) as f64))
            .unwrap_or_default(),
        report
            .quality
            .as_ref()
            .map(|q| format!(" · quality {}/{}", q.passed, q.total))
            .unwrap_or_default()
    );
    println!("report: {}", out.display());
    Ok(())
}

async fn run_local(a: &BenchArgs) -> Result<Report, String> {
    let path = resolve_model(&a.model)?;
    let checked = a.load.check(&path).await?;
    // Context must hold one run; placement and KV are estimated for that context.
    let ctx = bench_ctx(a.load.ctx(), a.n_prompt, a.n_gen);
    let (mut opts, _) = a.load.with_ctx(ctx).auto_options(&path)?;
    let pid = sysinfo::get_current_pid().map_err(|e| e.to_string())?;
    let mem = PeakMem::start(pid);
    opts.chat_template = checked.confirm()?;
    let t0 = Instant::now();
    let m = LoadedModel::load(
        Arc::new(LlamaCppBackend::new()),
        &ModelSpec::gguf(&path),
        &opts,
    )
    .await
    .map_err(|e| e.to_string())?;
    let load_ms = t0.elapsed().as_secs_f64() * 1e3;
    eprintln!("[bench] loaded in {load_ms:.0} ms; warm-up…");
    let (backend, h) = (m.backend().clone(), m.handle().clone());
    backend
        .bench(&h, a.n_prompt.min(64), a.n_gen.min(16))
        .await
        .map_err(|e| e.to_string())?;
    let mut runs = vec![];
    for i in 0..a.reps.max(1) {
        let r = backend
            .bench(&h, a.n_prompt, a.n_gen)
            .await
            .map_err(|e| e.to_string())?;
        eprintln!(
            "[bench] run {}: pp{} {:.1} tok/s · tg{} {:.1} tok/s",
            i + 1,
            a.n_prompt,
            r.prompt_tokens_per_s,
            a.n_gen,
            r.gen_tokens_per_s
        );
        runs.push(r);
    }
    let model_ref = ModelRef::Local {
        model_id: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let chat = chat_runs(&m, &model_ref, a).await?;
    let quality = if a.quality {
        Some(quality::run(&m, &model_ref).await?)
    } else {
        None
    };
    let info = backend.model_info(&h).map_err(|e| e.to_string())?;
    m.unload().await.map_err(|e| e.to_string())?;
    let peak_rss_bytes = mem.finish();
    let mut model = gguf_json(&path)?;
    model["n_ctx"] = info.n_ctx.into();
    Ok(Report {
        schema: "dn.bench.v2",
        dn_version: env!("CARGO_PKG_VERSION"),
        unix_time: now(),
        backend: "llamacpp".into(),
        model,
        machine: machine_json(),
        options: Some(opts),
        load_ms: Some(load_ms),
        n_prompt: a.n_prompt,
        n_gen: a.n_gen,
        prompt_tokens_per_s: Some(stats(
            &runs
                .iter()
                .map(|r| r.prompt_tokens_per_s)
                .collect::<Vec<_>>(),
        )),
        gen_tokens_per_s: Some(stats(
            &runs.iter().map(|r| r.gen_tokens_per_s).collect::<Vec<_>>(),
        )),
        runs,
        chat,
        quality,
        peak_rss_bytes: Some(peak_rss_bytes),
    })
}

async fn run_endpoint(a: &BenchArgs, ep: &str) -> Result<Report, String> {
    let key = match &a.api_key_env {
        Some(v) => Some(std::env::var(v).map_err(|_| format!("${v} is not set"))?),
        None => None,
    };
    let mem = a
        .server_pid
        .map(|p| PeakMem::start(sysinfo::Pid::from_u32(p)));
    let m = LoadedModel::load(
        Arc::new(OpenAiCompatBackend::new()),
        &ModelSpec::endpoint(ep, &a.model, key),
        &Default::default(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let model_ref = ModelRef::Remote {
        endpoint_id: "bench".into(),
        model_id: a.model.clone(),
    };
    let chat = chat_runs(&m, &model_ref, a).await?;
    let quality = if a.quality {
        Some(quality::run(&m, &model_ref).await?)
    } else {
        None
    };
    let mut model = match &a.gguf {
        Some(p) => gguf_json(p)?,
        None => serde_json::json!({}),
    };
    model["server_model"] = a.model.clone().into();
    Ok(Report {
        schema: "dn.bench.v2",
        dn_version: env!("CARGO_PKG_VERSION"),
        unix_time: now(),
        backend: a.backend_name.clone(),
        model,
        machine: machine_json(),
        options: None,
        load_ms: None,
        n_prompt: a.n_prompt,
        n_gen: a.n_gen,
        runs: vec![],
        prompt_tokens_per_s: None,
        gen_tokens_per_s: None,
        chat,
        quality,
        peak_rss_bytes: mem.map(PeakMem::finish),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_mean_stddev() {
        let s = stats(&[1.0, 3.0]);
        assert_eq!(s.mean, 2.0);
        assert_eq!(s.stddev, 1.0);
    }

    #[test]
    fn chat_speed_counts_tokens_after_the_first() {
        let r = chat_run(500, 101, Duration::from_millis(250), Duration::from_secs(2));
        assert_eq!(r.ttft_ms, 250.0);
        assert_eq!(r.gen_tokens_per_s, 50.0);
        assert_eq!(
            chat_run(5, 1, Duration::ZERO, Duration::ZERO).gen_tokens_per_s,
            0.0
        );
    }

    #[test]
    fn chat_prompt_grows_with_n_prompt() {
        let (a, b) = (chat_prompt(128, 1), chat_prompt(1024, 1));
        assert!(b.len() > 6 * a.len());
        assert!(b.contains("Item 64:"));
        // Runs differ from the first line, so no prompt cache can carry over.
        assert_ne!(
            chat_prompt(128, 1).lines().next(),
            chat_prompt(128, 2).lines().next()
        );
    }
}
