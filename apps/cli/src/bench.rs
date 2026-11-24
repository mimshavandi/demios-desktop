//! `dn bench`: llama-bench-style throughput (fixed token counts, EOS ignored) plus peak
//! process memory, written as a JSON report (schema `dn.bench.v1`).

use crate::{LoadArgs, machine, resolve_model};
use clap::Args;
use dn_inference::llamacpp::BenchRun;
use dn_inference::{InferenceBackend, LlamaCppBackend, ModelSpec};
use dn_models::{Gguf, ModelFootprint};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct BenchArgs {
    /// GGUF path or downloaded catalogue id.
    model: String,
    /// Prompt tokens per run.
    #[arg(long, default_value_t = 512)]
    n_prompt: u32,
    /// Generated tokens per run.
    #[arg(long, default_value_t = 128)]
    n_gen: u32,
    /// Measured runs (after one warm-up run).
    #[arg(long, default_value_t = 3)]
    reps: u32,
    /// Report path (default: bench-<model>.json).
    #[arg(long, short)]
    out: Option<PathBuf>,
    #[command(flatten)]
    load: LoadArgs,
}

#[derive(Serialize)]
struct Stats {
    mean: f64,
    stddev: f64,
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

#[derive(Serialize)]
struct Report {
    schema: &'static str,
    dn_version: &'static str,
    unix_time: u64,
    backend: &'static str,
    model: serde_json::Value,
    machine: serde_json::Value,
    options: dn_inference::LoadOptions,
    load_ms: f64,
    runs: Vec<BenchRun>,
    prompt_tokens_per_s: Stats,
    gen_tokens_per_s: Stats,
    /// Highest resident set size of this process, sampled every 20 ms. On unified memory,
    /// GPU buffers backed by the mmapped model count here too.
    peak_rss_bytes: u64,
}

/// Samples this process's RSS on a background thread until dropped.
struct PeakMem {
    peak: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl PeakMem {
    fn start() -> Self {
        let peak = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (peak.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            let Ok(pid) = sysinfo::get_current_pid() else {
                return;
            };
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

    fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.peak.load(Ordering::Relaxed)
    }
}

pub async fn run(a: BenchArgs) -> Result<(), String> {
    let path = resolve_model(&a.model)?;
    let g = Gguf::open(&path).map_err(|e| e.to_string())?;
    let fp = ModelFootprint::from_gguf(&g);
    let mut opts = a.load.options()?;
    // Context must hold one run.
    opts.n_ctx = Some(opts.n_ctx.unwrap_or(0).max(a.n_prompt + a.n_gen + 16));
    let mem = PeakMem::start();
    let backend = LlamaCppBackend::new();
    let t0 = Instant::now();
    let h = backend
        .load(&ModelSpec::gguf(&path), &opts)
        .await
        .map_err(|e| e.to_string())?;
    let load_ms = t0.elapsed().as_secs_f64() * 1e3;
    eprintln!("[bench] loaded in {load_ms:.0} ms; warm-up…");
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
    let info = backend.model_info(&h).map_err(|e| e.to_string())?;
    backend.unload(h).await.map_err(|e| e.to_string())?;
    let peak_rss_bytes = mem.finish();

    let mach = machine(None, None, false);
    let mut sys = sysinfo::System::new();
    sys.refresh_cpu_all();
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_default();
    let report = Report {
        schema: "dn.bench.v1",
        dn_version: env!("CARGO_PKG_VERSION"),
        unix_time: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        backend: "llamacpp",
        model: serde_json::json!({
            "path": path,
            "file_size": g.file_size,
            "arch": g.arch(),
            "quant": fp.quant,
            "n_params": fp.n_params,
            "n_layer": info.n_layer,
            "n_ctx": info.n_ctx,
            "moe": fp.is_moe(),
        }),
        machine: serde_json::json!({
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "cpu": cpu,
            "cpu_threads": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
            "memory": mach,
            "gpu_offload": LlamaCppBackend::supports_gpu(),
            "devices": LlamaCppBackend::devices().unwrap_or_default(),
        }),
        options: opts,
        load_ms,
        prompt_tokens_per_s: stats(
            &runs
                .iter()
                .map(|r| r.prompt_tokens_per_s)
                .collect::<Vec<_>>(),
        ),
        gen_tokens_per_s: stats(&runs.iter().map(|r| r.gen_tokens_per_s).collect::<Vec<_>>()),
        runs,
        peak_rss_bytes,
    };
    let out = a.out.unwrap_or_else(|| {
        PathBuf::from(format!(
            "bench-{}.json",
            path.file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        ))
    });
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&out, json + "\n").map_err(|e| format!("{}: {e}", out.display()))?;
    println!(
        "pp{} {:.1} ± {:.1} tok/s · tg{} {:.1} ± {:.1} tok/s · peak RSS {:.2} GiB",
        a.n_prompt,
        report.prompt_tokens_per_s.mean,
        report.prompt_tokens_per_s.stddev,
        a.n_gen,
        report.gen_tokens_per_s.mean,
        report.gen_tokens_per_s.stddev,
        peak_rss_bytes as f64 / (1u64 << 30) as f64
    );
    println!("report: {}", out.display());
    Ok(())
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
}
