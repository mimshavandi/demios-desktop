//! In-process llama.cpp backend (`llama-cpp-2` bindings). Metal on macOS, CPU elsewhere;
//! CUDA / Vulkan behind the `cuda` / `vulkan` cargo features.
//!
//! Each `chat` call runs on its own OS thread with a fresh context (KV cache) over the shared,
//! mmapped weights, and streams through a bounded channel. Dropping the stream closes the
//! channel; the thread notices on its next token and stops.
//!
//! No `unsafe` here: all FFI stays inside `llama-cpp-2`.

mod utf8;

use crate::backend::{TokenStream, normalize};
use crate::prompt::{self, Turn};
use crate::stream::{ChannelStream, Item};
use crate::tags::TagParser;
use crate::{
    BackendCaps, InferenceBackend, InferenceError, KvCacheQuant, LoadOptions, ModelHandle,
    ModelLocation, ModelSpec, MoeOffload, Result,
};
use dn_core::chat::{ChatRequest, FinishReason, StreamEvent, Usage};
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::{KvCacheType, LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaChatTemplate, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::CString;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tokio::sync::mpsc;
use utf8::Utf8Buf;

/// llama.cpp can only be initialised once per process.
fn global_backend() -> Result<&'static LlamaBackend> {
    static BACKEND: OnceLock<std::result::Result<LlamaBackend, String>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            // Silence llama.cpp/ggml (incl. Metal device init) unless DN_LLAMA_LOG is set.
            // Must happen before init, which already logs.
            let quiet = std::env::var_os("DN_LLAMA_LOG").is_none();
            if quiet {
                llama_cpp_2::send_logs_to_tracing(
                    llama_cpp_2::LogOptions::default().with_logs_enabled(false),
                );
            }
            LlamaBackend::init().map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| InferenceError::Load(format!("llama.cpp init: {e}")))
}

static ACTIVE: AtomicU64 = AtomicU64::new(0);

/// Generation threads currently running (drops to 0 shortly after streams are dropped).
pub fn active_generations() -> u64 {
    ACTIVE.load(Ordering::SeqCst)
}

struct ActiveGuard;

impl ActiveGuard {
    fn new() -> Self {
        ACTIVE.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Loaded {
    model: LlamaModel,
    template: Option<LlamaChatTemplate>,
    n_ctx: u32,
    opts: LoadOptions,
    path: std::path::PathBuf,
}

/// The default local backend.
#[derive(Default)]
pub struct LlamaCppBackend {
    models: Mutex<HashMap<u64, Arc<Loaded>>>,
    next_id: AtomicU64,
}

/// A compute device llama.cpp can see (used by the fit estimator for VRAM).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub description: String,
    pub backend: String,
    /// `cpu`, `gpu`, `igpu`, `accel` or `unknown`.
    pub kind: String,
    pub memory_total: u64,
    pub memory_free: u64,
}

/// Facts about a loaded model, for `dn run -v` and the bench report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelInfo {
    pub path: String,
    pub size_bytes: u64,
    pub n_params: u64,
    pub n_layer: u32,
    pub n_ctx_train: u32,
    pub n_ctx: u32,
    pub has_chat_template: bool,
}

/// One `dn bench` measurement (llama-bench style: fixed token counts, EOS ignored).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchRun {
    pub n_prompt: u32,
    pub n_gen: u32,
    pub prompt_ms: f64,
    pub gen_ms: f64,
    pub prompt_tokens_per_s: f64,
    pub gen_tokens_per_s: f64,
}

impl LlamaCppBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Devices visible to llama.cpp in this build (Metal GPU on macOS, CUDA/Vulkan if enabled).
    pub fn devices() -> Result<Vec<DeviceInfo>> {
        global_backend()?;
        use llama_cpp_2::LlamaBackendDeviceType as T;
        Ok(llama_cpp_2::list_llama_ggml_backend_devices()
            .into_iter()
            .map(|d| DeviceInfo {
                name: d.name,
                description: d.description,
                backend: d.backend,
                kind: match d.device_type {
                    T::Cpu => "cpu",
                    T::Gpu => "gpu",
                    T::IntegratedGpu => "igpu",
                    T::Accelerator => "accel",
                    T::Unknown => "unknown",
                }
                .into(),
                memory_total: d.memory_total as u64,
                memory_free: d.memory_free as u64,
            })
            .collect())
    }

    /// True when this build can offload layers to a GPU.
    pub fn supports_gpu() -> bool {
        global_backend()
            .map(|b| b.supports_gpu_offload())
            .unwrap_or(false)
    }

    fn get(&self, h: &ModelHandle) -> Result<Arc<Loaded>> {
        self.models
            .lock()
            .map_err(|_| InferenceError::Runtime("model table poisoned".into()))?
            .get(&h.id)
            .cloned()
            .ok_or(InferenceError::UnknownHandle(h.id))
    }

    pub fn model_info(&self, h: &ModelHandle) -> Result<ModelInfo> {
        let l = self.get(h)?;
        Ok(ModelInfo {
            path: l.path.display().to_string(),
            size_bytes: l.model.size(),
            n_params: l.model.n_params(),
            n_layer: l.model.n_layer(),
            n_ctx_train: l.model.n_ctx_train(),
            n_ctx: l.n_ctx,
            has_chat_template: l.template.is_some(),
        })
    }

    /// Prompt-processing and generation throughput with exactly `n_prompt` / `n_gen` tokens.
    pub async fn bench(&self, h: &ModelHandle, n_prompt: u32, n_gen: u32) -> Result<BenchRun> {
        let l = self.get(h)?;
        tokio::task::spawn_blocking(move || bench_blocking(&l, n_prompt, n_gen))
            .await
            .map_err(|e| InferenceError::Runtime(e.to_string()))?
    }
}

fn kv_type(q: KvCacheQuant) -> KvCacheType {
    match q {
        KvCacheQuant::F16 => KvCacheType::F16,
        KvCacheQuant::Q8_0 => KvCacheType::Q8_0,
        KvCacheQuant::Q4_0 => KvCacheType::Q4_0,
    }
}

fn threads(opts: &LoadOptions) -> i32 {
    opts.threads.map(|t| t as i32).unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4)
    })
}

fn context<'m>(l: &'m Loaded, embeddings: bool) -> Result<LlamaContext<'m>> {
    let t = threads(&l.opts);
    let mut p = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(l.n_ctx))
        .with_n_batch(l.opts.n_batch)
        .with_n_ubatch(l.opts.n_batch)
        .with_n_threads(t)
        .with_n_threads_batch(t)
        .with_type_k(kv_type(l.opts.kv_cache))
        .with_type_v(kv_type(l.opts.kv_cache));
    if embeddings {
        let arch = l
            .model
            .meta_val_str("general.architecture")
            .unwrap_or_default();
        let has_pooling = l
            .model
            .meta_val_str(&format!("{arch}.pooling_type"))
            .is_ok();
        p = p.with_embeddings(true).with_pooling_type(if has_pooling {
            LlamaPoolingType::Unspecified
        } else {
            LlamaPoolingType::Mean
        });
    }
    l.model
        .new_context(global_backend()?, p)
        .map_err(|e| InferenceError::Runtime(format!("context: {e}")))
}

/// Regex llama.cpp matches against tensor names to keep expert FFNs on the CPU.
fn moe_pattern(first_layers: u32) -> String {
    let layers: Vec<String> = (0..first_layers).map(|i| i.to_string()).collect();
    format!(
        "blk\\.({})\\.ffn_(up|down|gate)_(ch|)exps",
        layers.join("|")
    )
}

fn load_blocking(spec: &ModelSpec, opts: &LoadOptions) -> Result<Loaded> {
    let ModelLocation::Gguf { path } = &spec.location else {
        return Err(InferenceError::Unsupported {
            backend: "llamacpp",
            what: "endpoint models (use openai_compat)".into(),
        });
    };
    if !path.is_file() {
        return Err(InferenceError::NotFound(path.display().to_string()));
    }
    let backend = global_backend()?;
    let gpu_layers = opts.gpu_layers.unwrap_or(u32::MAX >> 1);
    let params = LlamaModelParams::default()
        .with_n_gpu_layers(gpu_layers)
        .with_use_mmap(opts.mmap);
    let mut params = std::pin::pin!(params);
    // Must outlive `load_from_file`: llama.cpp reads the pattern during load.
    let partial: CString;
    match opts.moe_offload {
        MoeOffload::None => {}
        MoeOffload::All => params.as_mut().add_cpu_moe_override(),
        MoeOffload::FirstLayers(0) => {}
        MoeOffload::FirstLayers(n) => {
            partial =
                CString::new(moe_pattern(n)).map_err(|e| InferenceError::Load(e.to_string()))?;
            params.as_mut().add_cpu_buft_override(&partial);
        }
    }
    let model = LlamaModel::load_from_file(backend, path, &params)
        .map_err(|e| InferenceError::Load(format!("{}: {e}", path.display())))?;
    let n_ctx = opts
        .n_ctx
        .unwrap_or_else(|| model.n_ctx_train().clamp(512, 8192));
    let template = model.chat_template(None).ok();
    Ok(Loaded {
        model,
        template,
        n_ctx,
        opts: opts.clone(),
        path: path.clone(),
    })
}

fn render(l: &Loaded, turns: &[Turn]) -> Result<String> {
    let msgs = turns
        .iter()
        .map(|t| LlamaChatMessage::new(t.role.into(), t.content.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| InferenceError::Runtime(e.to_string()))?;
    let fallback;
    let tmpl = match &l.template {
        Some(t) => t,
        None => {
            fallback = LlamaChatTemplate::new("chatml")
                .map_err(|e| InferenceError::Runtime(e.to_string()))?;
            &fallback
        }
    };
    l.model
        .apply_chat_template(tmpl, &msgs, true)
        .map_err(|e| InferenceError::Runtime(format!("chat template: {e}")))
}

fn sampler(l: &Loaded, req: &ChatRequest) -> LlamaSampler {
    let mut chain = vec![];
    if !req.tools.is_empty() {
        let schema = prompt::tool_call_schema(&req.tools).to_string();
        match llama_cpp_2::json_schema_to_grammar(&schema) {
            Ok(g) => {
                let g = prompt::wrap_tool_grammar(&g);
                if let Ok(s) =
                    LlamaSampler::grammar_lazy(&l.model, &g, "root", ["<tool_call>"], &[])
                {
                    chain.push(s);
                }
            }
            Err(e) => eprintln!("dn-inference: tool grammar unavailable: {e}"),
        }
    } else if let Some(schema) = &req.response_schema {
        if let Ok(g) = llama_cpp_2::json_schema_to_grammar(&schema.to_string()) {
            if let Ok(s) = LlamaSampler::grammar(&l.model, &g, "root") {
                chain.push(s);
            }
        }
    }
    let temp = req.temperature.unwrap_or(0.7);
    if temp <= 0.0 {
        chain.push(LlamaSampler::greedy());
    } else {
        chain.extend([
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.95, 1),
            LlamaSampler::min_p(0.05, 1),
            LlamaSampler::temp(temp),
            LlamaSampler::dist(seed()),
        ]);
    }
    LlamaSampler::chain_simple(chain)
}

fn seed() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(42)
}

/// Decode `tokens` starting at `pos`, in `n_batch` chunks; logits only for the last token.
fn decode_prompt(
    ctx: &mut LlamaContext<'_>,
    tokens: &[LlamaToken],
    n_batch: usize,
    batch: &mut LlamaBatch<'_>,
) -> Result<()> {
    let last = tokens.len().saturating_sub(1);
    for (ci, chunk) in tokens.chunks(n_batch).enumerate() {
        batch.clear();
        for (j, &t) in chunk.iter().enumerate() {
            let i = ci * n_batch + j;
            batch
                .add(t, i as i32, &[0], i == last)
                .map_err(|e| InferenceError::Runtime(e.to_string()))?;
        }
        ctx.decode(batch)
            .map_err(|e| InferenceError::Runtime(format!("decode: {e}")))?;
    }
    Ok(())
}

fn send(tx: &mpsc::Sender<Item>, ev: StreamEvent) -> bool {
    tx.blocking_send(Ok(ev)).is_ok()
}

fn generate(l: &Loaded, req: &ChatRequest, tx: &mpsc::Sender<Item>) -> Result<()> {
    let text = render(l, &prompt::turns(req))?;
    let vocab = l.model.vocab();
    let tokens = vocab.tokenize(text.as_bytes(), true, true);
    let n_prompt = tokens.len();
    if n_prompt as u32 >= l.n_ctx {
        return Err(InferenceError::ContextOverflow {
            prompt: n_prompt,
            n_ctx: l.n_ctx,
        });
    }
    let room = l.n_ctx as usize - n_prompt;
    let max_new = req.max_tokens.map_or(room, |m| (m as usize).min(room));

    let mut ctx = context(l, false)?;
    let n_batch = l.opts.n_batch.max(1) as usize;
    let mut batch = LlamaBatch::new(n_batch, 1);
    decode_prompt(&mut ctx, &tokens, n_batch, &mut batch)?;

    let mut sampler = sampler(l, req);
    let mut parser = TagParser::new();
    let mut utf8 = Utf8Buf::default();
    let mut pos = n_prompt as i32;
    let mut generated = 0usize;
    let mut finish = FinishReason::Length;
    while generated < max_new {
        if tx.is_closed() {
            return Ok(());
        }
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        generated += 1;
        if vocab.is_eog(token) {
            finish = FinishReason::Stop;
            break;
        }
        let piece = vocab.token_to_piece(token, false, None);
        if let Some(s) = utf8.push(&piece) {
            for ev in parser.push(&s) {
                if !send(tx, ev) {
                    return Ok(());
                }
            }
        }
        batch.clear();
        batch
            .add(token, pos, &[0], true)
            .map_err(|e| InferenceError::Runtime(e.to_string()))?;
        pos += 1;
        ctx.decode(&mut batch)
            .map_err(|e| InferenceError::Runtime(format!("decode: {e}")))?;
    }
    let tail = utf8.finish();
    let mut evs = parser.push(&tail);
    evs.extend(parser.finish());
    for ev in evs {
        if !send(tx, ev) {
            return Ok(());
        }
    }
    if parser.tool_calls() > 0 {
        finish = FinishReason::ToolCalls;
    }
    send(
        tx,
        StreamEvent::Done {
            finish_reason: finish,
            usage: Usage {
                input_tokens: n_prompt as u32,
                output_tokens: generated as u32,
            },
        },
    );
    Ok(())
}

fn embed_blocking(l: &Loaded, input: &[String]) -> Result<Vec<Vec<f32>>> {
    let mut ctx = context(l, true)?;
    let vocab = l.model.vocab();
    let mut out = Vec::with_capacity(input.len());
    for text in input {
        let tokens = vocab.tokenize(text.as_bytes(), true, false);
        let n = tokens
            .len()
            .min(l.n_ctx as usize)
            .min(l.opts.n_batch as usize);
        let mut batch = LlamaBatch::new(n.max(1), 1);
        for (i, &t) in tokens[..n].iter().enumerate() {
            batch
                .add(t, i as i32, &[0], true)
                .map_err(|e| InferenceError::Runtime(e.to_string()))?;
        }
        ctx.clear_kv_cache();
        ctx.decode(&mut batch)
            .map_err(|e| InferenceError::Runtime(format!("decode: {e}")))?;
        let mut v = ctx
            .embeddings_seq_ith(0)
            .map_err(|e| InferenceError::Runtime(format!("embeddings: {e}")))?
            .to_vec();
        normalize(&mut v);
        out.push(v);
    }
    Ok(out)
}

fn bench_blocking(l: &Loaded, n_prompt: u32, n_gen: u32) -> Result<BenchRun> {
    if n_prompt + n_gen >= l.n_ctx {
        return Err(InferenceError::ContextOverflow {
            prompt: (n_prompt + n_gen) as usize,
            n_ctx: l.n_ctx,
        });
    }
    let vocab = l.model.vocab();
    let seed_text = "The quick brown fox jumps over the lazy dog. ".repeat(64);
    let base = vocab.tokenize(seed_text.as_bytes(), false, false);
    let tokens: Vec<LlamaToken> = base
        .iter()
        .cycle()
        .take(n_prompt as usize)
        .copied()
        .collect();
    let mut ctx = context(l, false)?;
    let n_batch = l.opts.n_batch.max(1) as usize;
    let mut batch = LlamaBatch::new(n_batch, 1);

    let t0 = Instant::now();
    decode_prompt(&mut ctx, &tokens, n_batch, &mut batch)?;
    // decode() may return before the GPU finishes; wait so the timing is real.
    ctx.synchronize();
    let prompt_ms = t0.elapsed().as_secs_f64() * 1e3;

    let mut sampler = LlamaSampler::greedy();
    let t1 = Instant::now();
    for pos in (n_prompt as i32..).take(n_gen as usize) {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        batch.clear();
        batch
            .add(token, pos, &[0], true)
            .map_err(|e| InferenceError::Runtime(e.to_string()))?;
        ctx.decode(&mut batch)
            .map_err(|e| InferenceError::Runtime(format!("decode: {e}")))?;
    }
    ctx.synchronize();
    let gen_ms = t1.elapsed().as_secs_f64() * 1e3;
    let rate = |n: u32, ms: f64| if ms > 0.0 { n as f64 * 1e3 / ms } else { 0.0 };
    Ok(BenchRun {
        n_prompt,
        n_gen,
        prompt_ms,
        gen_ms,
        prompt_tokens_per_s: rate(n_prompt, prompt_ms),
        gen_tokens_per_s: rate(n_gen, gen_ms),
    })
}

impl InferenceBackend for LlamaCppBackend {
    fn name(&self) -> &'static str {
        "llamacpp"
    }

    fn capabilities(&self) -> BackendCaps {
        BackendCaps {
            chat: true,
            embed: true,
            tool_calls: true,
            grammar: true,
            ..Default::default()
        }
    }

    async fn load(&self, spec: &ModelSpec, opts: &LoadOptions) -> Result<ModelHandle> {
        let (spec, opts) = (spec.clone(), opts.clone());
        let model_id = spec.model_id.clone();
        let loaded = tokio::task::spawn_blocking(move || load_blocking(&spec, &opts))
            .await
            .map_err(|e| InferenceError::Load(e.to_string()))??;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.models
            .lock()
            .map_err(|_| InferenceError::Runtime("model table poisoned".into()))?
            .insert(id, Arc::new(loaded));
        Ok(ModelHandle::new(id, model_id))
    }

    async fn chat(&self, h: &ModelHandle, req: ChatRequest) -> Result<TokenStream> {
        let l = self.get(h)?;
        let (tx, rx) = mpsc::channel::<Item>(64);
        std::thread::Builder::new()
            .name("dn-llamacpp-chat".into())
            .spawn(move || {
                let _active = ActiveGuard::new();
                if let Err(e) = generate(&l, &req, &tx) {
                    let _ = tx.blocking_send(Err(e.into()));
                }
            })
            .map_err(|e| InferenceError::Runtime(e.to_string()))?;
        Ok(Box::pin(ChannelStream::new(rx)))
    }

    async fn embed(&self, h: &ModelHandle, input: &[String]) -> Result<Vec<Vec<f32>>> {
        let l = self.get(h)?;
        let input = input.to_vec();
        tokio::task::spawn_blocking(move || embed_blocking(&l, &input))
            .await
            .map_err(|e| InferenceError::Runtime(e.to_string()))?
    }

    async fn unload(&self, h: ModelHandle) -> Result<()> {
        self.models
            .lock()
            .map_err(|_| InferenceError::Runtime("model table poisoned".into()))?
            .remove(&h.id)
            .map(|_| ())
            .ok_or(InferenceError::UnknownHandle(h.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moe_pattern_lists_layers() {
        assert_eq!(
            moe_pattern(3),
            "blk\\.(0|1|2)\\.ffn_(up|down|gate)_(ch|)exps"
        );
    }

    #[tokio::test]
    async fn missing_file_is_not_found() {
        let b = LlamaCppBackend::new();
        let e = b
            .load(
                &ModelSpec::gguf("/nonexistent/x.gguf"),
                &LoadOptions::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(e, InferenceError::NotFound(_)));
    }

    #[tokio::test]
    async fn unknown_handle() {
        let b = LlamaCppBackend::new();
        assert!(matches!(
            b.unload(ModelHandle::new(7, "x")).await,
            Err(InferenceError::UnknownHandle(7))
        ));
    }
}
