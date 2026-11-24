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
use dn_models::hub::{
    GgufVariant, HubClient, HubConfig, LicenseInfo, LicenseWarning, PullProgress, SearchQuery,
    SearchSort, VariantKind, gguf_variants,
};
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
    /// Search Hugging Face for GGUF repos (HF_ENDPOINT selects a mirror).
    Search {
        text: String,
        #[arg(long, value_enum, default_value_t = SortArg::Downloads)]
        sort: SortArg,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// List a Hugging Face repo's GGUF quantisations at a pinned commit, with its licence.
    Files {
        /// `owner/name`.
        repo: String,
        #[arg(long)]
        revision: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Download a catalogue model, or a Hugging Face repo's quant (`owner/name --quant Q4_K_M`).
    /// Resumable; every file is checked against its SHA-256.
    Pull {
        /// Catalogue id or Hugging Face `owner/name`.
        id: String,
        /// Quantisation (Hugging Face only), e.g. Q4_K_M.
        #[arg(long)]
        quant: Option<String>,
        /// Exact variant name from `dn models files` (Hugging Face only).
        #[arg(long, conflicts_with = "quant")]
        variant: Option<String>,
        #[arg(long)]
        revision: Option<String>,
        /// Target directory (default: $DN_MODELS_DIR or the platform data dir).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum SortArg {
    Downloads,
    Likes,
    Trending,
    Updated,
}

#[derive(Clone, Copy, ValueEnum)]
enum KvArg {
    F16,
    Q8_0,
    Q4_0,
}

/// Options shared by `run` and `bench` for loading a GGUF.
#[derive(Args, Clone)]
pub struct LoadArgs {
    /// Context window (default: min(training context, 8192)).
    #[arg(long)]
    ctx: Option<u32>,
    /// Layers on the GPU (default: all).
    #[arg(long)]
    gpu_layers: Option<u32>,
    /// Keep MoE experts in CPU RAM: `all` or the number of leading layers.
    #[arg(long)]
    moe_cpu: Option<String>,
    #[arg(long, value_enum, default_value = "f16")]
    kv: KvArg,
    #[arg(long)]
    threads: Option<u32>,
}

impl LoadArgs {
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
                KvArg::F16 => KvCacheQuant::F16,
                KvArg::Q8_0 => KvCacheQuant::Q8_0,
                KvArg::Q4_0 => KvCacheQuant::Q4_0,
            },
            threads: self.threads,
            ..Default::default()
        })
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
    /// GGUF path, downloaded catalogue id, or Hugging Face `owner/name` (read remotely: only
    /// the GGUF header is fetched).
    model: String,
    /// Hugging Face: quant to check (default: every quant, estimated from one header).
    #[arg(long)]
    quant: Option<String>,
    /// Hugging Face: exact variant name from `dn models files`.
    #[arg(long, conflicts_with = "quant")]
    variant: Option<String>,
    #[arg(long)]
    revision: Option<String>,
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
        let m = LoadedModel::load(backend.clone(), &spec, &a.load.options()?)
            .await
            .map_err(|e| e.to_string())?;
        if a.verbose {
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

fn is_local_model(s: &str) -> bool {
    PathBuf::from(s).exists()
        || Catalog::builtin().get(s).is_some()
        || dn_models::hub::validate_repo_id(s).is_err()
}

async fn fit_cmd(a: FitArgs) -> Result<(), String> {
    let mach = machine(a.ram_gb, a.vram_gb, a.unified);
    if !is_local_model(&a.model) {
        return remote_fit(&a, &mach).await;
    }
    let path = resolve_model(&a.model)?;
    let g = Gguf::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fp = ModelFootprint::from_gguf(&g);
    print_fit(&path.display().to_string(), g.arch(), &fp, &mach, a.json)
}

/// Fit for a Hugging Face repo without downloading it: GGUF header via range reads.
async fn remote_fit(a: &FitArgs, mach: &Machine) -> Result<(), String> {
    let hub = hub_client().await?;
    let info = hub
        .repo(&a.model, a.revision.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    let variants = hub
        .variants(&a.model, &info.sha)
        .await
        .map_err(|e| e.to_string())?;
    let models: Vec<&GgufVariant> = variants
        .iter()
        .filter(|v| v.kind == VariantKind::Model)
        .collect();
    let chosen = models.iter().find(|v| match (&a.quant, &a.variant) {
        (_, Some(n)) => &v.name == n,
        (Some(q), _) => v
            .quant
            .as_deref()
            .is_some_and(|x| x.eq_ignore_ascii_case(q)),
        _ => false,
    });
    if a.quant.is_some() || a.variant.is_some() {
        let v = chosen.ok_or("no such variant (see `dn models files`)")?;
        let fp = hub.footprint(&info, v).await.map_err(|e| e.to_string())?;
        let label = format!("{}@{} {}", info.id, &info.sha[..12], v.name);
        return print_fit(&label, "", &fp, mach, a.json);
    }
    // Every quant: one header (the smallest variant), scaled to the others' sizes.
    let base = models.first().ok_or("no GGUF model variants")?;
    let fp = hub
        .footprint(&info, base)
        .await
        .map_err(|e| e.to_string())?;
    let rows: Vec<_> = models
        .iter()
        .map(|v| {
            let est = dn_models::hub::estimate_variant(&fp, base.total_size, v);
            (*v, dn_models::fit(&est, mach))
        })
        .collect();
    if a.json {
        let v: Vec<_> = rows
            .iter()
            .map(|(v, r)| serde_json::json!({"variant": v.name, "quant": v.quant, "size": v.total_size, "report": r}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    println!(
        "{}@{} ({} header read; others estimated from file size)",
        info.id,
        &info.sha[..12],
        base.name
    );
    println!("machine   {} RAM\n", gib(mach.total_ram));
    println!(
        "{:<48} {:<9} {:>9}  {:<34} ctx",
        "variant", "quant", "size", "status"
    );
    for (v, r) in rows {
        println!(
            "{:<48} {:<9} {:>9}  {:<34} {}",
            v.name,
            v.quant.as_deref().unwrap_or("-"),
            format!("{:.2} GB", v.total_size as f64 / 1e9),
            status_label(r.status),
            r.recommended.n_ctx
        );
    }
    Ok(())
}

fn print_fit(
    label: &str,
    arch: &str,
    fp: &ModelFootprint,
    mach: &Machine,
    json: bool,
) -> Result<(), String> {
    let mach = *mach;
    let report = dn_models::fit(fp, &mach);
    if json {
        let v =
            serde_json::json!({"model": label, "footprint": fp, "machine": mach, "report": report});
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
    println!("model     {label}");
    println!(
        "          {} {} · {:.2}B params · {} layers · train ctx {}{}",
        arch,
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

/// Hub client from `HF_ENDPOINT`, with `HF_TOKEN` or the keychain token if present.
async fn hub_client() -> Result<HubClient, String> {
    let mut cfg = HubConfig::from_env();
    match std::env::var("HF_TOKEN").ok().filter(|t| !t.is_empty()) {
        Some(t) => cfg = cfg.with_token(t.into()),
        None => {
            let store =
                dn_secrets::AsyncSecrets::new(Arc::new(dn_secrets::KeyringStore::default()));
            cfg = cfg.with_token_from(&store).await;
        }
    }
    HubClient::new(cfg).map_err(|e| e.to_string())
}

fn license_line(l: &LicenseInfo, card: &str) -> String {
    let id = l.name.as_deref().or(l.id.as_deref()).unwrap_or("none");
    let warn = match l.warning {
        None => String::new(),
        Some(LicenseWarning::Missing) => "  ⚠ no licence in the model card".into(),
        Some(LicenseWarning::NonCommercial) => "  ⚠ non-commercial / research licence".into(),
        Some(LicenseWarning::Custom) => "  ⚠ custom licence: read the model card".into(),
        Some(LicenseWarning::UsePolicy) => "  (acceptable-use policy applies: see the card)".into(),
    };
    format!("licence: {id}{warn}\nmodel card: {card}")
}

fn progress_printer() -> impl FnMut(u64, u64) + Send {
    let mut last = std::time::Instant::now();
    move |done: u64, total: u64| {
        if last.elapsed().as_millis() > 500 || done == total {
            last = std::time::Instant::now();
            eprint!(
                "\r  {:>6.1}%  {:.0}/{:.0} MB",
                done as f64 * 100.0 / total.max(1) as f64,
                done as f64 / 1e6,
                total as f64 / 1e6
            );
        }
    }
}

async fn hub_pull(
    repo: &str,
    quant: Option<String>,
    variant: Option<String>,
    revision: Option<String>,
    dir: PathBuf,
) -> Result<(), String> {
    let hub = hub_client().await?;
    let info = hub
        .repo(repo, revision.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    let files = hub
        .files(repo, &info.sha)
        .await
        .map_err(|e| e.to_string())?;
    let variants = gguf_variants(&files);
    let pick = |v: &&GgufVariant| match (&quant, &variant) {
        (_, Some(n)) => &v.name == n,
        (Some(q), _) => {
            v.kind == VariantKind::Model
                && v.quant
                    .as_deref()
                    .is_some_and(|x| x.eq_ignore_ascii_case(q))
        }
        _ => false,
    };
    let matches: Vec<&GgufVariant> = variants.iter().filter(pick).collect();
    let v = match matches.as_slice() {
        [v] => *v,
        [] => {
            return Err(format!(
                "pick one with --quant or --variant; available: {}",
                variants
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        _ => {
            return Err(format!(
                "--quant matches several variants, use --variant: {}",
                matches
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    };
    eprintln!(
        "pulling {repo}@{} {} ({:.2} GB, {} file(s)) → {}\n{}",
        &info.sha[..12],
        v.name,
        v.total_size as f64 / 1e9,
        v.files.len(),
        dir.display(),
        license_line(&info.license, &info.card_url)
    );
    let mut print = progress_printer();
    let p = hub
        .pull(&info, v, &dir, &mut |pr: PullProgress| {
            print(pr.done_bytes, pr.total_bytes)
        })
        .await
        .map_err(|e| e.to_string())?;
    eprintln!("\nverified sha256 of {} file(s)", v.files.len());
    println!("{}", p.display());
    Ok(())
}

async fn models_cmd(cmd: ModelsCmd) -> Result<(), String> {
    let cat = Catalog::builtin();
    match cmd {
        ModelsCmd::Search {
            text,
            sort,
            limit,
            json,
        } => {
            let hub = hub_client().await?;
            let page = hub
                .search(&SearchQuery {
                    text,
                    sort: match sort {
                        SortArg::Downloads => SearchSort::Downloads,
                        SortArg::Likes => SearchSort::Likes,
                        SortArg::Trending => SearchSort::Trending,
                        SortArg::Updated => SearchSort::Updated,
                    },
                    limit: Some(limit),
                })
                .await
                .map_err(|e| e.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&page).map_err(|e| e.to_string())?
                );
                return Ok(());
            }
            println!(
                "{:<56} {:>10} {:>6} {:<14}",
                "repo", "downloads", "likes", "license"
            );
            for r in &page.items {
                println!(
                    "{:<56} {:>10} {:>6} {:<14}{}",
                    r.id,
                    r.downloads,
                    r.likes,
                    r.license.as_deref().unwrap_or("-"),
                    if r.gated { " gated" } else { "" }
                );
            }
            Ok(())
        }
        ModelsCmd::Files {
            repo,
            revision,
            json,
        } => {
            let hub = hub_client().await?;
            let info = hub
                .repo(&repo, revision.as_deref())
                .await
                .map_err(|e| e.to_string())?;
            let variants = gguf_variants(
                &hub.files(&repo, &info.sha)
                    .await
                    .map_err(|e| e.to_string())?,
            );
            if json {
                let v = serde_json::json!({"repo": info, "variants": variants});
                println!(
                    "{}",
                    serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?
                );
                return Ok(());
            }
            println!(
                "{repo} @ {}{}",
                info.sha,
                if info.gated { " (gated)" } else { "" }
            );
            println!("{}\n", license_line(&info.license, &info.card_url));
            println!(
                "{:<56} {:<10} {:>9} {:>5}",
                "variant", "quant", "size", "parts"
            );
            for v in &variants {
                println!(
                    "{:<56} {:<10} {:>9} {:>5}{}",
                    v.name,
                    v.quant.as_deref().unwrap_or("-"),
                    format!("{:.2} GB", v.total_size as f64 / 1e9),
                    v.files.len(),
                    if v.kind == VariantKind::Projector {
                        " (vision projector)"
                    } else {
                        ""
                    }
                );
            }
            Ok(())
        }
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
        ModelsCmd::Pull {
            id,
            quant,
            variant,
            revision,
            dir,
        } => {
            let dir = dir.unwrap_or_else(dn_models::download::default_models_dir);
            if id.contains('/') {
                return hub_pull(&id, quant, variant, revision, dir).await;
            }
            if quant.is_some() || variant.is_some() || revision.is_some() {
                return Err(
                    "--quant/--variant/--revision apply to Hugging Face repos (owner/name)".into(),
                );
            }
            let e = cat
                .get(&id)
                .ok_or_else(|| format!("unknown model `{id}` (see `dn models list`)"))?;
            eprintln!(
                "pulling {} ({:.2} GB, licence {}) → {}",
                e.id,
                e.size as f64 / 1e9,
                e.license,
                dir.display()
            );
            let client = dn_models::download::client().map_err(|e| e.to_string())?;
            let mut progress = progress_printer();
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
        Cmd::Fit(a) => fit_cmd(a).await,
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
