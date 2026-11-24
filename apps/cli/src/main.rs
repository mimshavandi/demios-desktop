//! `dn` — headless entry point for running, sizing, benchmarking and downloading models
//! (T-INF-01..06).

mod bench;

use clap::{Args, Parser, Subcommand, ValueEnum};
use dn_core::chat::{ChatMessage, ChatRequest, Role, StreamEvent};
use dn_core::model::{FitStatus, ModelRef};
use dn_inference::{
    InferenceBackend, KvCacheQuant, LlamaCppBackend, LoadOptions, LoadedModel, ModelSpec,
    MoeOffload, OpenAiCompatBackend, next_event,
};
use dn_models::fit::{GIB, KvType, MemoryKind};
use dn_models::{Catalog, Gguf, Machine, ModelFootprint};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "dn",
    version,
    about = "Demios desktop: run, size, benchmark and download models"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Stream a chat completion from a local GGUF (or an OpenAI-compatible server).
    Run(RunArgs),
    /// Estimate whether a GGUF fits this machine and recommend settings.
    Fit(FitArgs),
    /// Measure prompt / generation tokens per second and peak memory; writes a JSON report.
    Bench(bench::BenchArgs),
    /// The curated model catalogue.
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
}

#[derive(Subcommand)]
enum ModelsCmd {
    /// List catalogue models and whether they are downloaded.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Download a catalogue model (resumable, SHA-256 verified).
    Pull {
        id: String,
        /// Target directory (default: $DN_MODELS_DIR or the platform data dir).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum KvArg {
    F16,
    Q8_0,
    Q4_0,
}

/// Options shared by `run` and `bench` for loading a GGUF. Anything left unset is chosen by
/// the fit estimator (`dn fit`) for this machine, unless `--no-auto`.
#[derive(Args, Clone, Default)]
pub struct LoadArgs {
    /// Context window (default: the fit estimator's choice).
    #[arg(long)]
    ctx: Option<u32>,
    /// Layers on the GPU (default: the fit estimator's choice; all when it fits).
    #[arg(long)]
    gpu_layers: Option<u32>,
    /// Keep MoE experts in CPU RAM: `all` or the number of leading layers.
    #[arg(long)]
    moe_cpu: Option<String>,
    /// KV cache type (default: the fit estimator's choice).
    #[arg(long, value_enum)]
    kv: Option<KvArg>,
    #[arg(long)]
    threads: Option<u32>,
    /// Don't use the fit estimator: llama.cpp defaults (ctx min(train, 8192), all layers on
    /// the GPU, f16 KV).
    #[arg(long)]
    no_auto: bool,
    /// Load even when the fit estimator says the model won't fit.
    #[arg(long)]
    force: bool,
}

impl LoadArgs {
    /// `--ctx` as given.
    pub fn ctx(&self) -> Option<u32> {
        self.ctx
    }

    pub fn options(&self) -> Result<LoadOptions, String> {
        let moe_offload = match self.moe_cpu.as_deref() {
            None => MoeOffload::None,
            Some("all") => MoeOffload::All,
            Some(n) => MoeOffload::FirstLayers(
                n.parse()
                    .map_err(|_| format!("--moe-cpu expects `all` or a number, got `{n}`"))?,
            ),
        };
        Ok(LoadOptions {
            n_ctx: self.ctx,
            gpu_layers: self.gpu_layers,
            moe_offload,
            kv_cache: match self.kv {
                None | Some(KvArg::F16) => KvCacheQuant::F16,
                Some(KvArg::Q8_0) => KvCacheQuant::Q8_0,
                Some(KvArg::Q4_0) => KvCacheQuant::Q4_0,
            },
            threads: self.threads,
            ..Default::default()
        })
    }

    /// [`options`](Self::options) with every unset value taken from the fit estimator for
    /// `path` on this machine. Refuses a model that won't fit unless `--force`.
    pub fn auto_options(
        &self,
        path: &std::path::Path,
    ) -> Result<(LoadOptions, Option<dn_models::FitReport>), String> {
        let opts = self.options()?;
        if self.no_auto {
            return Ok((opts, None));
        }
        let g = Gguf::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let fp = ModelFootprint::from_gguf(&g);
        let report = dn_models::fit(&fp, &machine(None, None, false));
        if report.status == FitStatus::MemoryBlocked && !self.force {
            let hint = report
                .suggested_quant
                .as_deref()
                .map(|q| format!(" Try a smaller quant ({q})."))
                .unwrap_or_default();
            return Err(format!(
                "{} won't fit in this machine's memory (needs {}, budget {}).{hint} \
                 `dn fit` shows the details; --force loads anyway.",
                path.display(),
                gib(report.recommended.need_bytes),
                gib(report.recommended.budget_bytes)
            ));
        }
        Ok((self.apply_fit(opts, &report, fp.n_layer), Some(report)))
    }

    /// Fill what the user didn't set from `report`.
    fn apply_fit(
        &self,
        mut opts: LoadOptions,
        report: &dn_models::FitReport,
        n_layer: u32,
    ) -> LoadOptions {
        let r = &report.recommended;
        if self.ctx.is_none() {
            opts.n_ctx = Some(r.n_ctx);
        }
        if self.gpu_layers.is_none() {
            opts.gpu_layers = r.placement.gpu_layers;
        }
        if self.moe_cpu.is_none() && r.placement.moe_cpu_layers > 0 {
            opts.moe_offload = if r.placement.moe_cpu_layers >= n_layer {
                MoeOffload::All
            } else {
                MoeOffload::FirstLayers(r.placement.moe_cpu_layers)
            };
        }
        if self.kv.is_none() {
            opts.kv_cache = match r.kv {
                KvType::F16 => KvCacheQuant::F16,
                KvType::Q8_0 => KvCacheQuant::Q8_0,
            };
        }
        opts
    }
}

#[derive(Args)]
struct RunArgs {
    /// GGUF path or catalogue id; with --endpoint, the server's model name.
    #[arg(long, short)]
    model: String,
    prompt: String,
    /// OpenAI-compatible base URL, e.g. http://localhost:11434/v1 (Ollama).
    #[arg(long)]
    endpoint: Option<String>,
    /// Environment variable holding the endpoint's API key.
    #[arg(long)]
    api_key_env: Option<String>,
    #[arg(long)]
    system: Option<String>,
    #[arg(long)]
    max_tokens: Option<u32>,
    #[arg(long)]
    temperature: Option<f32>,
    /// JSON file with an array of tools: [{"name", "description", "input_schema"}].
    #[arg(long)]
    tools: Option<PathBuf>,
    /// Thinking for reasoning models. `auto` thinks here, since `dn run` output is prose
    /// for you (caller `chat.reply`); `off` pre-fills an empty think block.
    #[arg(long, value_enum, default_value_t = Think::Auto)]
    think: Think,
    /// Print load time, token counts and speed to stderr.
    #[arg(long, short)]
    verbose: bool,
    #[command(flatten)]
    load: LoadArgs,
}

#[derive(Clone, Copy, ValueEnum)]
enum Think {
    Auto,
    On,
    Off,
}

#[derive(Args)]
struct FitArgs {
    /// GGUF path or catalogue id (downloaded).
    model: String,
    /// Override detected RAM (GiB).
    #[arg(long)]
    ram_gb: Option<f64>,
    /// Pretend a discrete GPU with this much VRAM (GiB).
    #[arg(long)]
    vram_gb: Option<f64>,
    /// Treat memory as unified (Apple Silicon) even if not detected.
    #[arg(long)]
    unified: bool,
    #[arg(long)]
    json: bool,
}

/// Resolve a path or a downloaded catalogue id.
pub fn resolve_model(s: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(s);
    if p.is_file() {
        return Ok(p);
    }
    if let Some(e) = Catalog::builtin().get(s) {
        let p = dn_models::download::default_models_dir().join(&e.file);
        if p.is_file() {
            return Ok(p);
        }
        return Err(format!(
            "`{s}` is not downloaded yet: run `dn models pull {s}`"
        ));
    }
    Err(format!("no such file or catalogue id: {s}"))
}

fn gib(b: u64) -> String {
    format!("{:.2} GiB", b as f64 / GIB as f64)
}

async fn run(a: RunArgs) -> Result<(), String> {
    let mut messages = vec![];
    if let Some(s) = &a.system {
        messages.push(ChatMessage::text(Role::System, s));
    }
    messages.push(ChatMessage::text(Role::User, &a.prompt));
    let tools: Vec<dn_core::chat::ToolSpec> = match &a.tools {
        Some(p) => serde_json::from_str(
            &std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
        )
        .map_err(|e| format!("{}: {e}", p.display()))?,
        None => vec![],
    };
    let t0 = std::time::Instant::now();
    let mut stream = if let Some(ep) = &a.endpoint {
        let key = match &a.api_key_env {
            Some(v) => Some(std::env::var(v).map_err(|_| format!("${v} is not set"))?),
            None => None,
        };
        let spec = ModelSpec::endpoint(ep, &a.model, key);
        let m = LoadedModel::load(
            Arc::new(OpenAiCompatBackend::new()),
            &spec,
            &a.load.options()?,
        )
        .await
        .map_err(|e| e.to_string())?;
        let r = ModelRef::Remote {
            endpoint_id: "cli".into(),
            model_id: a.model.clone(),
        };
        chat(&m, r, messages, tools, &a)
    } else {
        let path = resolve_model(&a.model)?;
        let spec = ModelSpec::gguf(&path);
        let backend = Arc::new(LlamaCppBackend::new());
        let (opts, report) = a.load.auto_options(&path)?;
        if let Some(r) = &report
            && r.status != FitStatus::Ready
        {
            eprintln!("[dn] fit: {} (see `dn fit`)", status_label(r.status));
        }
        let m = LoadedModel::load(backend.clone(), &spec, &opts)
            .await
            .map_err(|e| e.to_string())?;
        if a.verbose {
            eprintln!(
                "[dn] settings: ctx {} · kv {:?} · gpu layers {} · moe {:?}{}",
                opts.n_ctx
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "default".into()),
                opts.kv_cache,
                opts.gpu_layers
                    .map(|g| g.to_string())
                    .unwrap_or_else(|| "all".into()),
                opts.moe_offload,
                if report.is_some() {
                    " (fit estimator)"
                } else {
                    ""
                }
            );
            let info = backend.model_info(m.handle()).map_err(|e| e.to_string())?;
            eprintln!(
                "[dn] loaded {} ({}, {} layers, ctx {}) in {:.2}s; gpu offload: {}",
                path.display(),
                gib(info.size_bytes),
                info.n_layer,
                info.n_ctx,
                t0.elapsed().as_secs_f64(),
                LlamaCppBackend::supports_gpu()
            );
        }
        let r = ModelRef::Local {
            model_id: spec.model_id.clone(),
        };
        chat(&m, r, messages, tools, &a)
    };
    let t1 = std::time::Instant::now();
    let mut first: Option<f64> = None;
    let mut out = std::io::stdout().lock();
    let mut in_reasoning = false;
    while let Some(ev) = next_event(&mut stream).await {
        match ev.map_err(|e| e.to_string())? {
            StreamEvent::TextDelta { text } => {
                if in_reasoning {
                    in_reasoning = false;
                    let _ = writeln!(out, "\x1b[0m");
                }
                first.get_or_insert(t1.elapsed().as_secs_f64());
                let _ = write!(out, "{text}");
                let _ = out.flush();
            }
            StreamEvent::ReasoningDelta { text } => {
                if !in_reasoning {
                    in_reasoning = true;
                    let _ = write!(out, "\x1b[2m");
                }
                first.get_or_insert(t1.elapsed().as_secs_f64());
                let _ = write!(out, "{text}");
                let _ = out.flush();
            }
            StreamEvent::ToolCall { call } => {
                let _ = writeln!(out, "\n[tool call] {} {}", call.name, call.arguments);
            }
            StreamEvent::Done {
                finish_reason,
                usage,
            } => {
                let _ = writeln!(out);
                if a.verbose {
                    let total = t1.elapsed().as_secs_f64();
                    let ttft = first.unwrap_or(total);
                    let gen_s = (total - ttft).max(1e-9);
                    eprintln!(
                        "[dn] {:?}: {} prompt + {} generated tokens; first token {:.0} ms; {:.1} tok/s",
                        finish_reason,
                        usage.input_tokens,
                        usage.output_tokens,
                        ttft * 1e3,
                        usage.output_tokens.saturating_sub(1) as f64 / gen_s
                    );
                }
            }
        }
    }
    Ok(())
}

fn chat<B: InferenceBackend>(
    m: &LoadedModel<B>,
    model: ModelRef,
    messages: Vec<ChatMessage>,
    tools: Vec<dn_core::chat::ToolSpec>,
    a: &RunArgs,
) -> dn_core::chat::EventStream {
    use dn_core::chat::ChatModel;
    m.chat(ChatRequest {
        model,
        messages,
        tools,
        max_tokens: a.max_tokens,
        temperature: a.temperature,
        response_schema: None,
        reasoning: match a.think {
            Think::Auto => dn_core::chat::Reasoning::Auto,
            Think::On => dn_core::chat::Reasoning::On,
            Think::Off => dn_core::chat::Reasoning::Off,
        },
        // A person reads this answer, like a chat reply.
        caller: "chat.reply".into(),
    })
}

/// This machine, refined with what llama.cpp reports about GPUs.
pub fn machine(ram_gb: Option<f64>, vram_gb: Option<f64>, unified: bool) -> Machine {
    let mut m = dn_models::detect_machine();
    if let Ok(devs) = LlamaCppBackend::devices() {
        let gpu = devs
            .iter()
            .filter(|d| d.kind == "gpu")
            .max_by_key(|d| d.memory_total);
        if let Some(g) = gpu {
            m.memory = match m.memory {
                MemoryKind::Unified { .. } => MemoryKind::Unified {
                    gpu_budget: Some(g.memory_total),
                },
                _ => MemoryKind::Discrete {
                    vram: g.memory_total,
                },
            };
        }
    }
    if let Some(r) = ram_gb {
        m.total_ram = (r * GIB as f64) as u64;
        if let MemoryKind::Unified { gpu_budget } = &mut m.memory {
            *gpu_budget = None;
        }
    }
    if unified {
        m.memory = MemoryKind::Unified { gpu_budget: None };
    }
    if let Some(v) = vram_gb {
        m.memory = MemoryKind::Discrete {
            vram: (v * GIB as f64) as u64,
        };
    }
    m
}

fn fit_cmd(a: FitArgs) -> Result<(), String> {
    let path = resolve_model(&a.model)?;
    let g = Gguf::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fp = ModelFootprint::from_gguf(&g);
    let mach = machine(a.ram_gb, a.vram_gb, a.unified);
    let report = dn_models::fit(&fp, &mach);
    if a.json {
        let v =
            serde_json::json!({"model": path, "footprint": fp, "machine": mach, "report": report});
        println!(
            "{}",
            serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    let r = &report.recommended;
    let mem = match mach.memory {
        MemoryKind::Unified { gpu_budget } => format!(
            "unified, GPU budget {}",
            gpu_budget.map(gib).unwrap_or_else(|| "estimated".into())
        ),
        MemoryKind::CpuOnly => "CPU only".into(),
        MemoryKind::Discrete { vram } => format!("discrete GPU, {} VRAM", gib(vram)),
    };
    println!("model     {}", path.display());
    println!(
        "          {} {} · {:.2}B params · {} layers · train ctx {}{}",
        g.arch(),
        fp.quant.as_deref().unwrap_or("?"),
        fp.n_params as f64 / 1e9,
        fp.n_layer,
        fp.n_ctx_train,
        if fp.is_moe() {
            format!(" · MoE, experts {}", gib(fp.expert_bytes))
        } else {
            String::new()
        }
    );
    println!("machine   {} RAM ({mem})", gib(mach.total_ram));
    println!("status    {}", status_label(report.status));
    println!(
        "settings  ctx {} · kv {} · gpu layers {} · moe experts on CPU: {} layers",
        r.n_ctx,
        match r.kv {
            KvType::F16 => "f16",
            KvType::Q8_0 => "q8_0",
        },
        r.placement
            .gpu_layers
            .map(|g| g.to_string())
            .unwrap_or_else(|| "all".into()),
        r.placement.moe_cpu_layers
    );
    let split = r.placement.moe_cpu_layers > 0 || r.placement.gpu_layers.is_some();
    println!(
        "memory    weights {} + kv {} + overhead {} = {}; {}budget {} ({:.0}% used)",
        gib(r.weight_bytes),
        gib(r.kv_bytes),
        gib(r.overhead_bytes),
        gib(r.need_bytes),
        if split {
            "split across GPU + RAM; GPU "
        } else {
            ""
        },
        gib(r.budget_bytes),
        r.ratio * 100.0
    );
    if let Some(q) = &report.suggested_quant {
        println!("suggest   {q}");
    }
    for n in &report.notes {
        println!("note      {n}");
    }
    Ok(())
}

fn status_label(s: FitStatus) -> &'static str {
    match s {
        FitStatus::Ready => "ready",
        FitStatus::Caution => "caution (fits, little headroom)",
        FitStatus::MemoryPressure => "memory pressure (will swap; slow)",
        FitStatus::MemoryBlocked => "blocked (does not fit)",
    }
}

async fn models_cmd(cmd: ModelsCmd) -> Result<(), String> {
    let cat = Catalog::builtin();
    match cmd {
        ModelsCmd::List { json } => {
            let dir = dn_models::download::default_models_dir();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&cat).map_err(|e| e.to_string())?
                );
                return Ok(());
            }
            println!(
                "{:<40} {:>9} {:<8} {:<12} local",
                "id", "size", "quant", "license"
            );
            for m in &cat.models {
                let local =
                    dn_models::download::is_verified(&dir.join(&m.file), m.size, &m.sha256).await;
                println!(
                    "{:<40} {:>9} {:<8} {:<12} {}",
                    m.id,
                    format!("{:.1} GB", m.size as f64 / 1e9),
                    m.quant,
                    m.license,
                    if local { "yes" } else { "" }
                );
            }
            println!("\nmodels dir: {}", dir.display());
            Ok(())
        }
        ModelsCmd::Pull { id, dir } => {
            let e = cat
                .get(&id)
                .ok_or_else(|| format!("unknown model `{id}` (see `dn models list`)"))?;
            let dir = dir.unwrap_or_else(dn_models::download::default_models_dir);
            eprintln!(
                "pulling {} ({:.2} GB, licence {}) → {}",
                e.id,
                e.size as f64 / 1e9,
                e.license,
                dir.display()
            );
            let client = dn_models::download::client().map_err(|e| e.to_string())?;
            let mut last = std::time::Instant::now();
            let mut progress = |done: u64, total: u64| {
                if last.elapsed().as_millis() > 500 || done == total {
                    last = std::time::Instant::now();
                    eprint!(
                        "\r  {:>6.1}%  {:.0}/{:.0} MB",
                        done as f64 * 100.0 / total.max(1) as f64,
                        done as f64 / 1e6,
                        total as f64 / 1e6
                    );
                }
            };
            let p = dn_models::download::pull(&client, e, &dir, &mut progress)
                .await
                .map_err(|e| e.to_string())?;
            eprintln!("\nverified sha256 {}", e.sha256);
            println!("{}", p.display());
            Ok(())
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let r = match cli.cmd {
        Cmd::Run(a) => run(a).await,
        Cmd::Fit(a) => fit_cmd(a),
        Cmd::Bench(a) => bench::run(a).await,
        Cmd::Models { cmd } => models_cmd(cmd).await,
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dn: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moe() -> ModelFootprint {
        ModelFootprint {
            weight_bytes: 18_600_000_000,
            expert_bytes: 17_500_000_000,
            n_params: 30_500_000_000,
            n_layer: 48,
            n_ctx_train: 40_960,
            n_vocab: 151_936,
            n_embd: 2048,
            kv_elems_per_token: 48 * 4 * (128 + 128),
            quant: Some("Q4_K_M".into()),
            swa_kv_elems_per_token: 0,
            sliding_window: 0,
        }
    }

    #[test]
    fn fit_fills_unset_load_options() {
        let report = dn_models::fit(&moe(), &Machine::discrete(32 * GIB, 8 * GIB));
        let r = &report.recommended;
        let a = LoadArgs::default();
        let o = a.apply_fit(a.options().unwrap(), &report, 48);
        assert_eq!(o.n_ctx, Some(r.n_ctx));
        assert_eq!(o.gpu_layers, r.placement.gpu_layers);
        assert_eq!(
            o.moe_offload,
            MoeOffload::FirstLayers(r.placement.moe_cpu_layers)
        );
        assert!(r.placement.moe_cpu_layers > 0);
    }

    #[test]
    fn user_settings_win_over_fit() {
        let report = dn_models::fit(&moe(), &Machine::discrete(32 * GIB, 8 * GIB));
        let a = LoadArgs {
            ctx: Some(1024),
            gpu_layers: Some(3),
            moe_cpu: Some("all".into()),
            kv: Some(KvArg::Q4_0),
            ..Default::default()
        };
        let o = a.apply_fit(a.options().unwrap(), &report, 48);
        assert_eq!(o.n_ctx, Some(1024));
        assert_eq!(o.gpu_layers, Some(3));
        assert_eq!(o.moe_offload, MoeOffload::All);
        assert_eq!(o.kv_cache, KvCacheQuant::Q4_0);
    }
}
