//! Fit estimator: does a GGUF model fit this machine, and with which settings?
//!
//! Designed from first principles (T-INF-03); every constant is named below so it can be
//! tuned against `dn bench` measurements.
//!
//! # Memory needed
//!
//! ```text
//! need(ctx, kv) = W + KV(ctx, kv) + O
//!
//! W            weight bytes: sum of tensor sizes in the GGUF (exact, from the tensor table)
//! KV(ctx, kv)  = bytes(kv) × [ ctx × Σ_full n_head_kv(l) × (key_length + value_length)
//!                           + min(ctx, window + 512) × Σ_swa n_head_kv(l) × (k_swa + v_swa) ]
//!              bytes(f16) = 2, bytes(q8_0) = 34/32, bytes(q4_0) = 18/32
//!              key/value_length default to n_embd / n_head; `_swa` lengths default to them
//!              Σ_full: full-attention layers; Σ_swa: sliding-window layers (from
//!              `attention.sliding_window_pattern`, or the fixed gemma2/gemma3/gpt-oss layouts)
//! O            runtime overhead = RUNTIME_OVERHEAD (300 MiB: backend, Metal/CUDA context)
//!              + compute buffers ≈ 4 bytes × N_UBATCH (512) × (n_vocab + 4 × n_embd)
//! ```
//!
//! Layers of unknown kind count as full attention (recurrent layers need less), so the
//! estimate errs on the safe side.
//!
//! # Memory available
//!
//! - **Unified memory** (Apple Silicon): the GPU may wire at most the Metal working-set
//!   limit. If llama.cpp reports it, that value is used; otherwise
//!   `B = RAM × 2/3` for RAM ≤ 36 GiB, else `RAM × 3/4` (Apple's documented defaults).
//! - **CPU only**: `B = RAM − max(2 GiB, 20% × RAM)` (OS + apps reserve).
//! - **Discrete GPU** with VRAM `V`: `G = 0.9 × V − 300 MiB` on the GPU, plus the CPU budget
//!   above for whatever is offloaded. Attention, shared tensors and the KV cache stay on the
//!   GPU; if they do not all fit, the GPU is filled up to `0.8 × G` (the Ready threshold) and:
//!   - MoE models move expert FFN tensors of the first `k` layers to CPU RAM
//!     (`k = ⌈(need − 0.8G) / (E / n_layer)⌉`, `E` = expert bytes), llama.cpp `--n-cpu-moe k`;
//!   - dense models offload `g = ⌊(0.8G − O) / ((W + KV) / n_layer)⌋` layers to the GPU
//!     and run the rest on the CPU (a layer's KV cache lives where the layer runs).
//!
//! ## MoE presets (T-INF-20)
//!
//! A mixture-of-experts model (`E > 0`) can keep its expert FFN tensors in CPU RAM while
//! attention, shared tensors and the KV cache stay on the GPU. Only a few experts run per
//! token, so the CPU side costs some speed but far less than offloading whole layers.
//!
//! - **Discrete GPU:** the `--n-cpu-moe k` split above.
//! - **Unified memory:** the GPU and CPU share RAM, so moving experts doesn't lower the
//!   total, but CPU-side tensors (llama.cpp repacks them into CPU buffers) don't count
//!   against the Metal working-set limit `B`. With the same `k` formula against `B`, the
//!   ratio becomes `max(GPU part / B, need / C)`, where `C` is the CPU-only budget above
//!   (`RAM − reserve`). This lets a 30B-A3B model at a low quant run on a 16 GB Mac. It is
//!   used only when it improves the status, and the recommendation prefers all on the GPU
//!   at a smaller context over a split at a larger one with the same status.
//!
//! The report names the preset (`MoePreset`), so the UI can explain it.
//!
//! ## Where the numbers come from
//!
//! RAM comes from `sysinfo`. `sysinfo` cannot report VRAM reliably, so the GPU side is
//! either **detected** (the device list llama.cpp reports in `dn-inference`, which `dn fit`
//! uses) or **user-provided** (`dn fit --vram-gb`, and later a setting in the UI). On Apple
//! Silicon, the unified-memory budget is used: the Metal working-set limit when llama.cpp
//! reports it, otherwise the RAM fractions above. If no GPU is detected and none is
//! given, the machine counts as CPU only.
//!
//! # Status
//!
//! With `r = need / budget` (on a split, the larger of the GPU and CPU ratios):
//!
//! | r            | status            | meaning                                   |
//! |--------------|-------------------|-------------------------------------------|
//! | ≤ 0.80       | `Ready`           | fits with headroom                        |
//! | ≤ 0.95       | `Caution`         | fits; little room for other apps          |
//! | ≤ 1.10       | `MemoryPressure`  | runs, but will swap / page weights (slow) |
//! | > 1.10       | `MemoryBlocked`   | do not load                               |
//!
//! A split across GPU and CPU is never better than `Caution` (it works, but slower).
//!
//! # Recommendation
//!
//! Context candidates `32768, 16384, 8192, 4096, 2048` (capped at the training context); for
//! each, f16 KV then q8_0 KV. The recommendation is the largest context that reaches `Ready`
//! (at ≥ 4096 tokens); failing that, the largest that reaches `Caution`; failing that, the
//! smallest config. If the file's quant cannot reach `Ready` at 4096 tokens, the estimator
//! also suggests the highest-quality smaller quant that can, scaling weights by bits per
//! weight: `W_q = W × bpw(q) / bpw(file)`.

use crate::gguf::{Gguf, MetaValue};
use dn_core::model::FitStatus;
use serde::{Deserialize, Serialize};

pub const GIB: u64 = 1 << 30;
pub const MIB: u64 = 1 << 20;
const RUNTIME_OVERHEAD: u64 = 300 * MIB;
const N_UBATCH: u64 = 512;
const GPU_DRIVER_RESERVE: u64 = 300 * MIB;
const READY: f64 = 0.80;
const CAUTION: f64 = 0.95;
const PRESSURE: f64 = 1.10;
const MIN_GOOD_CTX: u32 = 4096;
const CTX_LADDER: [u32; 5] = [32768, 16384, 8192, 4096, 2048];

/// Approximate bits per weight of llama.cpp quant types (incl. block scales and the
/// higher-precision tensors mixed into `_M`/`_S` recipes), best quality first.
pub const QUANT_LADDER: [(&str, f64); 10] = [
    ("Q8_0", 8.50),
    ("Q6_K", 6.56),
    ("Q5_K_M", 5.69),
    ("Q4_K_M", 4.85),
    ("IQ4_XS", 4.25),
    ("Q3_K_M", 3.91),
    ("IQ3_M", 3.66),
    ("IQ3_XXS", 3.06),
    ("Q2_K", 2.96),
    ("IQ2_M", 2.70),
];

fn bpw(quant: &str) -> Option<f64> {
    match quant {
        "F32" => Some(32.0),
        "F16" | "BF16" => Some(16.0),
        "Q4_0" => Some(4.55),
        "Q5_0" => Some(5.54),
        "Q4_K_S" => Some(4.58),
        "Q5_K_S" => Some(5.54),
        "Q3_K_S" => Some(3.50),
        "Q3_K_L" => Some(4.27),
        "IQ4_NL" => Some(4.50),
        _ => QUANT_LADDER
            .iter()
            .find(|(q, _)| *q == quant)
            .map(|(_, b)| *b),
    }
}

/// KV-cache element type the estimator may recommend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KvType {
    F16,
    Q8_0,
}

impl KvType {
    pub fn bytes(self) -> f64 {
        match self {
            Self::F16 => 2.0,
            Self::Q8_0 => 34.0 / 32.0,
        }
    }
}

/// What the estimator needs to know about a model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelFootprint {
    pub weight_bytes: u64,
    /// Bytes of MoE expert FFN tensors (0 for dense models).
    pub expert_bytes: u64,
    pub n_params: u64,
    pub n_layer: u32,
    pub n_ctx_train: u32,
    pub n_vocab: u64,
    pub n_embd: u64,
    /// Σ over full-attention layers of n_head_kv × (key_length + value_length):
    /// KV elements cached per token of context.
    pub kv_elems_per_token: u64,
    /// Same sum over sliding-window layers (they cache at most `sliding_window` tokens).
    #[serde(default)]
    pub swa_kv_elems_per_token: u64,
    #[serde(default)]
    pub sliding_window: u32,
    pub quant: Option<String>,
}

impl ModelFootprint {
    pub fn from_gguf(g: &Gguf) -> Self {
        let n_layer = g.arch_u64("block_count").unwrap_or(0) as u32;
        let n_embd = g.arch_u64("embedding_length").unwrap_or(0);
        let n_head = g.arch_u64("attention.head_count").unwrap_or(1).max(1);
        let head_dim = n_embd / n_head;
        let k_len = g.arch_u64("attention.key_length").unwrap_or(head_dim);
        let v_len = g.arch_u64("attention.value_length").unwrap_or(head_dim);
        let k_swa = g.arch_u64("attention.key_length_swa").unwrap_or(k_len);
        let v_swa = g.arch_u64("attention.value_length_swa").unwrap_or(v_len);
        let window = g.arch_u64("attention.sliding_window").unwrap_or(0) as u32;
        let arr = |suffix: &str| match g.get(&format!("{}.{suffix}", g.arch())) {
            Some(MetaValue::IntArray(v)) => Some(v.clone()),
            _ => None,
        };
        let kv_heads = arr("attention.head_count_kv");
        let kv_scalar = g.arch_u64("attention.head_count_kv").unwrap_or(n_head);
        let pattern = arr("attention.sliding_window_pattern");
        let arch = g.arch().to_string();
        // Which layers use sliding-window attention: explicit per-layer pattern, else the
        // fixed layouts llama.cpp hard-codes; unknown layouts count as full attention.
        let is_swa = |l: usize| -> bool {
            if window == 0 {
                return false;
            }
            if let Some(p) = &pattern {
                return p.get(l).is_some_and(|&x| x != 0);
            }
            match arch.as_str() {
                "gemma3" => !(l + 1).is_multiple_of(6),
                "gemma2" | "gpt-oss" => l.is_multiple_of(2),
                _ => false,
            }
        };
        let (mut full, mut swa) = (0u64, 0u64);
        for l in 0..n_layer as usize {
            let h = kv_heads
                .as_ref()
                .and_then(|v| v.get(l))
                .map(|&x| x.max(0) as u64)
                .unwrap_or(kv_scalar);
            if is_swa(l) {
                swa += h * (k_swa + v_swa);
            } else {
                full += h * (k_len + v_len);
            }
        }
        let n_vocab = match g.get("tokenizer.ggml.tokens") {
            Some(MetaValue::ArrayLen(n)) => *n,
            _ => g.arch_u64("vocab_size").unwrap_or(32000),
        };
        Self {
            weight_bytes: g.tensors.iter().map(|t| t.size).sum(),
            expert_bytes: g
                .tensors
                .iter()
                .filter(|t| t.is_expert())
                .map(|t| t.size)
                .sum(),
            n_params: g.tensors.iter().map(|t| t.n_elements()).sum(),
            n_layer,
            n_ctx_train: g.arch_u64("context_length").unwrap_or(4096) as u32,
            n_vocab,
            n_embd,
            kv_elems_per_token: full,
            swa_kv_elems_per_token: swa,
            sliding_window: if swa > 0 { window } else { 0 },
            quant: g.quant_name().map(str::to_string),
        }
    }

    pub fn is_moe(&self) -> bool {
        self.expert_bytes > 0
    }

    pub fn kv_bytes(&self, ctx: u32, kv: KvType) -> u64 {
        let swa_ctx = ctx.min(self.sliding_window + N_UBATCH as u32);
        ((ctx as f64 * self.kv_elems_per_token as f64
            + swa_ctx as f64 * self.swa_kv_elems_per_token as f64)
            * kv.bytes()) as u64
    }

    pub fn overhead_bytes(&self) -> u64 {
        RUNTIME_OVERHEAD + 4 * N_UBATCH * (self.n_vocab + 4 * self.n_embd)
    }

    /// Same model at another quant (weights scaled by bits per weight).
    pub fn requantized(&self, quant: &str) -> Option<Self> {
        let from = bpw(self.quant.as_deref()?)?;
        let to = bpw(quant)?;
        let scale = to / from;
        Some(Self {
            weight_bytes: (self.weight_bytes as f64 * scale) as u64,
            expert_bytes: (self.expert_bytes as f64 * scale) as u64,
            quant: Some(quant.to_string()),
            ..self.clone()
        })
    }
}

/// The machine's memory layout.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryKind {
    /// CPU and GPU share RAM (Apple Silicon). `gpu_budget`: Metal working-set limit, if known.
    Unified {
        gpu_budget: Option<u64>,
    },
    CpuOnly,
    /// Discrete GPU with its own VRAM.
    Discrete {
        vram: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub total_ram: u64,
    pub memory: MemoryKind,
}

impl Machine {
    pub fn unified(total_ram: u64) -> Self {
        Self {
            total_ram,
            memory: MemoryKind::Unified { gpu_budget: None },
        }
    }

    pub fn cpu_only(total_ram: u64) -> Self {
        Self {
            total_ram,
            memory: MemoryKind::CpuOnly,
        }
    }

    pub fn discrete(total_ram: u64, vram: u64) -> Self {
        Self {
            total_ram,
            memory: MemoryKind::Discrete { vram },
        }
    }

    fn cpu_budget(&self) -> u64 {
        let reserve = (2 * GIB).max(self.total_ram / 5);
        self.total_ram.saturating_sub(reserve)
    }

    fn unified_budget(&self, reported: Option<u64>) -> u64 {
        reported.unwrap_or(if self.total_ram <= 36 * GIB {
            self.total_ram * 2 / 3
        } else {
            self.total_ram * 3 / 4
        })
    }
}

/// Where the weights go for one configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    /// Layers on the GPU (`None` = all, or no GPU).
    pub gpu_layers: Option<u32>,
    /// Layers whose MoE experts stay in CPU RAM (llama.cpp `--n-cpu-moe`).
    pub moe_cpu_layers: u32,
}

impl Placement {
    const FULL: Self = Self {
        gpu_layers: None,
        moe_cpu_layers: 0,
    };

    /// Part of the model runs on the CPU (slower than all on the GPU).
    pub fn is_split(&self) -> bool {
        *self != Self::FULL
    }
}

/// How a mixture-of-experts model is laid out, for the UI to explain (T-INF-20).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoePreset {
    /// Every layer's experts stay in CPU RAM; attention and shared tensors on the GPU.
    ExpertsInRam,
    /// The first `moe_cpu_layers` layers' experts stay in CPU RAM; the rest is on the GPU.
    SomeExpertsInRam,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    pub status: FitStatus,
    pub n_ctx: u32,
    pub kv: KvType,
    pub placement: Placement,
    pub weight_bytes: u64,
    pub kv_bytes: u64,
    pub overhead_bytes: u64,
    pub need_bytes: u64,
    /// Bytes on the GPU / unified budget.
    pub budget_bytes: u64,
    /// Highest of the GPU and CPU need/budget ratios.
    pub ratio: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FitReport {
    /// Status of this file at the recommended settings.
    pub status: FitStatus,
    pub recommended: Estimate,
    /// Set when the file's own quant cannot reach `Ready` at 4096 tokens and a smaller one can.
    pub suggested_quant: Option<String>,
    /// Set when the recommendation keeps MoE experts in CPU RAM.
    #[serde(default)]
    pub moe_preset: Option<MoePreset>,
    pub notes: Vec<String>,
}

fn status_of(ratio: f64) -> FitStatus {
    if ratio <= READY {
        FitStatus::Ready
    } else if ratio <= CAUTION {
        FitStatus::Caution
    } else if ratio <= PRESSURE {
        FitStatus::MemoryPressure
    } else {
        FitStatus::MemoryBlocked
    }
}

fn rank(s: FitStatus) -> u8 {
    match s {
        FitStatus::Ready => 0,
        FitStatus::Caution => 1,
        FitStatus::MemoryPressure => 2,
        FitStatus::MemoryBlocked => 3,
    }
}

fn worse(a: FitStatus, b: FitStatus) -> FitStatus {
    if rank(a) >= rank(b) { a } else { b }
}

/// Layers whose experts must leave a GPU budget filled up to `g_fill` (`--n-cpu-moe k`),
/// and the bytes that leaves on the GPU and moves to the CPU.
fn experts_off(m: &ModelFootprint, need: u64, g_fill: u64) -> (u64, u64, Placement) {
    let layers = m.n_layer.max(1) as u64;
    let per = (m.expert_bytes / layers).max(1);
    let k = need.saturating_sub(g_fill).div_ceil(per).min(layers);
    (
        need.saturating_sub(k * per),
        k * per,
        Placement {
            gpu_layers: None,
            moe_cpu_layers: k as u32,
        },
    )
}

/// Estimate one configuration.
pub fn estimate(m: &ModelFootprint, mach: &Machine, n_ctx: u32, kv: KvType) -> Estimate {
    let w = m.weight_bytes;
    let kvb = m.kv_bytes(n_ctx, kv);
    let o = m.overhead_bytes();
    let need = w + kvb + o;
    let full = Placement::FULL;
    let (budget, ratio, status, placement) = match mach.memory {
        MemoryKind::Unified { gpu_budget } => {
            let b = mach.unified_budget(gpu_budget);
            let r = need as f64 / b as f64;
            let all_gpu = (b, r, status_of(r), full);
            if !m.is_moe() || status_of(r) == FitStatus::Ready {
                all_gpu
            } else {
                // Experts in CPU buffers: outside the Metal limit, inside total RAM.
                let (gpu_bytes, _, placement) = experts_off(m, need, (b as f64 * READY) as u64);
                let r2 = (gpu_bytes as f64 / b as f64).max(need as f64 / mach.cpu_budget() as f64);
                let s2 = worse(status_of(r2), FitStatus::Caution);
                if rank(s2) < rank(status_of(r)) {
                    (b, r2, s2, placement)
                } else {
                    all_gpu
                }
            }
        }
        MemoryKind::CpuOnly => {
            let b = mach.cpu_budget();
            let r = need as f64 / b as f64;
            (b, r, status_of(r), full)
        }
        MemoryKind::Discrete { vram } => {
            let g = ((vram as f64 * 0.9) as u64).saturating_sub(GPU_DRIVER_RESERVE);
            let c = mach.cpu_budget();
            if need <= g {
                let r = need as f64 / g as f64;
                (g, r, status_of(r), full)
            } else {
                let layers = m.n_layer.max(1) as u64;
                // Fill the GPU only up to the Ready threshold, so it keeps headroom.
                let g_fill = (g as f64 * READY) as u64;
                let (gpu_bytes, cpu_bytes, placement) = if m.is_moe() {
                    experts_off(m, need, g_fill)
                } else {
                    // A layer's KV cache lives with the layer (GPU or CPU).
                    let per = ((w + kvb) / layers).max(1);
                    let room = g_fill.saturating_sub(o);
                    let on_gpu = (room / per).min(layers);
                    (
                        o + on_gpu * per,
                        (layers - on_gpu) * per,
                        Placement {
                            gpu_layers: Some(on_gpu as u32),
                            moe_cpu_layers: 0,
                        },
                    )
                };
                let r = (gpu_bytes as f64 / g as f64).max(cpu_bytes as f64 / c as f64);
                (g, r, worse(status_of(r), FitStatus::Caution), placement)
            }
        }
    };
    Estimate {
        status,
        n_ctx,
        kv,
        placement,
        weight_bytes: w,
        kv_bytes: kvb,
        overhead_bytes: o,
        need_bytes: need,
        budget_bytes: budget,
        ratio,
    }
}

fn candidates(m: &ModelFootprint) -> Vec<(u32, KvType)> {
    let mut ctxs: Vec<u32> = CTX_LADDER
        .iter()
        .copied()
        .filter(|&c| c <= m.n_ctx_train)
        .collect();
    if ctxs.is_empty() {
        ctxs.push(m.n_ctx_train.max(512));
    }
    ctxs.into_iter()
        .flat_map(|c| [(c, KvType::F16), (c, KvType::Q8_0)])
        .collect()
}

fn best(m: &ModelFootprint, mach: &Machine) -> Estimate {
    let all: Vec<Estimate> = candidates(m)
        .into_iter()
        .map(|(c, kv)| estimate(m, mach, c, kv))
        .collect();
    let good_ctx = |e: &&Estimate| e.n_ctx >= MIN_GOOD_CTX.min(m.n_ctx_train);
    all.iter()
        .filter(good_ctx)
        .find(|e| e.status == FitStatus::Ready)
        // Among Caution configs, all on the GPU (faster) beats a bigger context with a split.
        .or_else(|| {
            all.iter()
                .filter(good_ctx)
                .find(|e| e.status == FitStatus::Caution && !e.placement.is_split())
        })
        .or_else(|| {
            all.iter()
                .filter(good_ctx)
                .find(|e| e.status == FitStatus::Caution)
        })
        .or_else(|| all.iter().find(|e| e.status == FitStatus::Ready))
        .or_else(|| all.iter().find(|e| e.status == FitStatus::Caution))
        .unwrap_or_else(|| all.last().expect("at least one candidate"))
        .clone()
}

/// Recommend settings for `model` on `machine`.
pub fn fit(model: &ModelFootprint, machine: &Machine) -> FitReport {
    let rec = best(model, machine);
    let mut notes = vec![];
    let mut suggested_quant = None;
    // A model trained on a shorter context counts as ready at its full training context.
    let ready_at_min =
        rec.status == FitStatus::Ready && rec.n_ctx >= MIN_GOOD_CTX.min(model.n_ctx_train);
    if !ready_at_min {
        let current = model.quant.as_deref().and_then(bpw).unwrap_or(f64::MAX);
        suggested_quant = QUANT_LADDER
            .iter()
            .filter(|(_, b)| *b < current - 0.05)
            .filter_map(|(q, _)| model.requantized(q))
            .find(|alt| {
                let e = best(alt, machine);
                e.status == FitStatus::Ready && e.n_ctx >= MIN_GOOD_CTX.min(alt.n_ctx_train)
            })
            .and_then(|alt| alt.quant);
        if let Some(q) = &suggested_quant {
            notes.push(format!(
                "Re-quantised to {q}, this model would run comfortably."
            ));
        } else if rec.status == FitStatus::MemoryBlocked {
            notes.push(
                "Too large for this machine at any common quant; pick a smaller model.".into(),
            );
        }
    }
    if rec.kv == KvType::Q8_0 {
        notes.push("Quantised (q8_0) KV cache keeps the context window usable.".into());
    }
    if rec.placement.moe_cpu_layers > 0 {
        notes.push(format!(
            "Experts of {} layers stay in CPU RAM; attention runs on the GPU.",
            rec.placement.moe_cpu_layers
        ));
    }
    if let Some(g) = rec.placement.gpu_layers {
        notes.push(format!(
            "{g} of {} layers fit on the GPU; the rest run on the CPU (slower).",
            model.n_layer
        ));
    }
    let k = rec.placement.moe_cpu_layers;
    let moe_preset = match k {
        0 => None,
        k if k >= model.n_layer => Some(MoePreset::ExpertsInRam),
        _ => Some(MoePreset::SomeExpertsInRam),
    };
    FitReport {
        status: rec.status,
        recommended: rec,
        suggested_quant,
        moe_preset,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Llama-3.1-8B-Instruct Q4_K_M: 4.92 GB file, 32 layers, GQA 8 KV heads × 128 dims.
    fn llama8b() -> ModelFootprint {
        ModelFootprint {
            weight_bytes: 4_920_000_000,
            expert_bytes: 0,
            n_params: 8_030_000_000,
            n_layer: 32,
            n_ctx_train: 131_072,
            n_vocab: 128_256,
            n_embd: 4096,
            kv_elems_per_token: 32 * 8 * (128 + 128),
            quant: Some("Q4_K_M".into()),
            swa_kv_elems_per_token: 0,
            sliding_window: 0,
        }
    }

    /// Qwen2.5-0.5B-Instruct Q4_K_M: 491 MB, 24 layers, 2 KV heads × 64 dims.
    fn qwen05b() -> ModelFootprint {
        ModelFootprint {
            weight_bytes: 491_000_000,
            expert_bytes: 0,
            n_params: 494_000_000,
            n_layer: 24,
            n_ctx_train: 32_768,
            n_vocab: 151_936,
            n_embd: 896,
            kv_elems_per_token: 24 * 2 * (64 + 64),
            quant: Some("Q4_K_M".into()),
            swa_kv_elems_per_token: 0,
            sliding_window: 0,
        }
    }

    /// Qwen3-30B-A3B Q4_K_M: 18.6 GB, 48 layers, 4 KV heads × 128 dims, ~94% experts.
    fn qwen30b_moe() -> ModelFootprint {
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
    fn kv_and_overhead_formula() {
        let m = llama8b();
        // 32 layers × 8 heads × 256 × 2 bytes = 128 KiB per token → 1 GiB at 8192.
        assert_eq!(m.kv_bytes(8192, KvType::F16), GIB);
        assert_eq!(m.kv_bytes(8192, KvType::Q8_0), GIB * 34 / 64);
        assert_eq!(
            m.overhead_bytes(),
            300 * MIB + 4 * 512 * (128_256 + 4 * 4096)
        );
    }

    #[test]
    fn sliding_window_layers_cache_only_the_window() {
        // Gemma-4-26B-A4B layout: 25 SWA layers (8 KV heads × 256+256, window 1024) and
        // 5 full layers (2 KV heads × 512+512).
        let m = ModelFootprint {
            kv_elems_per_token: 5 * 2 * 1024,
            swa_kv_elems_per_token: 25 * 8 * 512,
            sliding_window: 1024,
            ..llama8b()
        };
        let at = |ctx| m.kv_bytes(ctx, KvType::F16);
        assert_eq!(at(1024), 1024 * (10_240 + 102_400) * 2);
        // Past the window (+ ubatch) only the full layers grow.
        assert_eq!(at(32768) - at(16384), 16384 * 10_240 * 2);
    }

    #[test]
    fn reads_swa_layout_from_gguf() {
        use crate::gguf::write::{V, gguf};
        let bytes = gguf(
            &[
                ("general.architecture", V::Str("gemma4")),
                ("gemma4.block_count", V::U32(3)),
                ("gemma4.embedding_length", V::U32(64)),
                ("gemma4.attention.head_count", V::U32(4)),
                ("gemma4.attention.head_count_kv", V::I32Arr(&[8, 8, 2])),
                ("gemma4.attention.key_length", V::U32(512)),
                ("gemma4.attention.value_length", V::U32(512)),
                ("gemma4.attention.key_length_swa", V::U32(256)),
                ("gemma4.attention.value_length_swa", V::U32(256)),
                ("gemma4.attention.sliding_window", V::U32(1024)),
                (
                    "gemma4.attention.sliding_window_pattern",
                    V::BoolArr(&[true, true, false]),
                ),
            ],
            &[("blk.0.attn_q.weight", &[8, 8], 0, 256)],
        );
        let len = bytes.len() as u64;
        let g = Gguf::read(std::io::Cursor::new(bytes), len).unwrap();
        let f = ModelFootprint::from_gguf(&g);
        assert_eq!(f.kv_elems_per_token, 2 * 1024);
        assert_eq!(f.swa_kv_elems_per_token, 2 * 8 * 512);
        assert_eq!(f.sliding_window, 1024);
    }

    #[test]
    fn tiny_model_ready_everywhere() {
        for gb in [8, 16, 32] {
            let r = fit(&qwen05b(), &Machine::unified(gb * GIB));
            assert_eq!(r.status, FitStatus::Ready, "{gb} GB");
            assert_eq!(r.recommended.n_ctx, 32768, "{gb} GB");
            assert!(r.suggested_quant.is_none());
        }
    }

    #[test]
    fn llama8b_on_8gb_mac_needs_smaller_quant() {
        let r = fit(&llama8b(), &Machine::unified(8 * GIB));
        // 5.33 GiB budget; 4.58 GiB weights + KV + overhead cannot be Ready at 4096.
        assert_ne!(r.status, FitStatus::Ready);
        // Ready (≤ 4.27 GiB) needs weights ≤ ~3.4 GiB: Q3_K_M (3.70) and IQ3_M (3.47) miss.
        assert_eq!(r.suggested_quant.as_deref(), Some("IQ3_XXS"));
    }

    #[test]
    fn llama8b_on_16gb_mac_ready() {
        let r = fit(&llama8b(), &Machine::unified(16 * GIB));
        assert_eq!(r.status, FitStatus::Ready);
        // 32k f16 = 4 GiB KV → too tight; 32k q8_0 (2.1 GiB) fits under 80% of 10.67 GiB.
        assert_eq!(r.recommended.n_ctx, 32768);
        assert_eq!(r.recommended.kv, KvType::Q8_0);
        assert!(r.suggested_quant.is_none());
    }

    #[test]
    fn llama8b_on_32gb_mac_full_context_f16() {
        let r = fit(&llama8b(), &Machine::unified(32 * GIB));
        assert_eq!(r.status, FitStatus::Ready);
        assert_eq!(r.recommended.n_ctx, 32768);
        assert_eq!(r.recommended.kv, KvType::F16);
    }

    #[test]
    fn moe_30b_by_machine_size() {
        let m = qwen30b_moe();
        assert_eq!(
            fit(&m, &Machine::unified(8 * GIB)).status,
            FitStatus::MemoryBlocked
        );
        assert_eq!(
            fit(&m, &Machine::unified(16 * GIB)).status,
            FitStatus::MemoryBlocked
        );
        let r32 = fit(&m, &Machine::unified(32 * GIB));
        // 17.3 GiB weights in a 21.3 GiB budget: runs, without much headroom.
        assert_eq!(r32.status, FitStatus::Caution);
        assert_eq!(r32.suggested_quant.as_deref(), Some("IQ4_XS"));
    }

    #[test]
    fn moe_experts_in_ram_on_a_16gb_mac() {
        // Qwen3-30B-A3B at Q2_K (~10.6 GiB) is over the 10.7 GiB Metal limit with its KV
        // cache and overhead, but fits in 16 GB once the experts live outside it.
        let q2 = qwen30b_moe().requantized("Q2_K").unwrap();
        let mac16 = Machine::unified(16 * GIB);
        let all_gpu = estimate(&q2, &Machine::unified(64 * GIB), 4096, KvType::F16);
        assert!(all_gpu.need_bytes as f64 / mac16.unified_budget(None) as f64 > CAUTION);
        let r = fit(&q2, &mac16);
        assert_eq!(r.status, FitStatus::Caution);
        assert!(r.recommended.placement.moe_cpu_layers > 0);
        assert!(r.moe_preset.is_some());
        assert!(
            r.notes.iter().any(|n| n.contains("CPU RAM")),
            "{:?}",
            r.notes
        );
        // Q4_K_M is still too big for 16 GB, however the experts are placed.
        assert_eq!(fit(&qwen30b_moe(), &mac16).status, FitStatus::MemoryBlocked);
    }

    #[test]
    fn moe_on_a_32gb_mac_prefers_all_on_gpu() {
        // All on the GPU already reaches Caution (q8_0 KV), so the slower split isn't used
        // just for a bigger context.
        let r = fit(&qwen30b_moe(), &Machine::unified(32 * GIB));
        assert_eq!(r.status, FitStatus::Caution);
        assert!(!r.recommended.placement.is_split());
        assert_eq!(r.moe_preset, None);
        // At 32k f16 KV, all on the GPU is past the Metal limit; experts in RAM fit.
        let e = estimate(
            &qwen30b_moe(),
            &Machine::unified(32 * GIB),
            32768,
            KvType::F16,
        );
        assert_eq!(e.status, FitStatus::Caution);
        assert!(e.placement.moe_cpu_layers > 0);
    }

    #[test]
    fn moe_preset_only_when_needed() {
        let r = fit(&qwen30b_moe(), &Machine::unified(64 * GIB));
        assert_eq!(r.status, FitStatus::Ready);
        assert_eq!(r.moe_preset, None);
        // Dense models never get a MoE preset.
        let d = fit(&llama8b(), &Machine::unified(8 * GIB));
        assert_eq!(d.moe_preset, None);
        assert!(!d.recommended.placement.is_split());
    }

    #[test]
    fn moe_all_experts_preset_on_small_vram() {
        // 2 GB GPU: every layer's experts go to RAM.
        let r = fit(&qwen30b_moe(), &Machine::discrete(32 * GIB, 2 * GIB));
        assert_eq!(r.recommended.placement.moe_cpu_layers, 48);
        assert_eq!(r.moe_preset, Some(MoePreset::ExpertsInRam));
        let r8 = fit(&qwen30b_moe(), &Machine::discrete(32 * GIB, 8 * GIB));
        assert_eq!(r8.moe_preset, Some(MoePreset::SomeExpertsInRam));
    }

    #[test]
    fn moe_expert_offload_on_8gb_vram() {
        // 32 GB RAM + 8 GB GPU: attention on GPU, most experts in RAM.
        let r = fit(&qwen30b_moe(), &Machine::discrete(32 * GIB, 8 * GIB));
        assert_eq!(r.status, FitStatus::Caution);
        let k = r.recommended.placement.moe_cpu_layers;
        assert!(k > 24 && k <= 48, "moe_cpu_layers = {k}");
        assert_eq!(r.recommended.placement.gpu_layers, None);
        // Same GPU with only 16 GB RAM: ~12 GiB of experts in a 12.8 GiB CPU budget.
        let r16 = fit(&qwen30b_moe(), &Machine::discrete(16 * GIB, 8 * GIB));
        assert_eq!(r16.status, FitStatus::MemoryPressure);
    }

    #[test]
    fn dense_partial_gpu_offload() {
        let r = fit(&llama8b(), &Machine::discrete(16 * GIB, 4 * GIB));
        let g = r.recommended.placement.gpu_layers.expect("partial offload");
        assert!(g > 0 && g < 32, "gpu_layers = {g}");
        assert_eq!(r.status, FitStatus::Caution);
    }

    #[test]
    fn cpu_only_budget_reserves_os() {
        let m = Machine::cpu_only(8 * GIB);
        assert_eq!(m.cpu_budget(), 8 * GIB - 2 * GIB);
        let m = Machine::cpu_only(32 * GIB);
        assert_eq!(m.cpu_budget(), 32 * GIB - 32 * GIB / 5);
    }

    #[test]
    fn requantize_scales_by_bpw() {
        let q3 = llama8b().requantized("Q3_K_M").unwrap();
        let expect = 4_920_000_000.0 * 3.91 / 4.85;
        assert!((q3.weight_bytes as f64 - expect).abs() < 1.0);
    }

    #[test]
    fn reported_metal_budget_wins() {
        let mach = Machine {
            total_ram: 16 * GIB,
            memory: MemoryKind::Unified {
                gpu_budget: Some(12 * GIB),
            },
        };
        assert_eq!(
            estimate(&llama8b(), &mach, 8192, KvType::F16).budget_bytes,
            12 * GIB
        );
    }
}
