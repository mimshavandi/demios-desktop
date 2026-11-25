//! `dn bench-report`: a markdown table from `dn bench` JSON reports (T-INF-21). Rows are
//! grouped by machine, then model, then backend, so in-process llama.cpp and a server
//! running the same GGUF sit next to each other.

use clap::Args;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct ReportArgs {
    /// Report files, or directories to search (non-recursively) for `*.json` reports.
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    /// Write the table here instead of printing it.
    #[arg(long, short)]
    out: Option<PathBuf>,
}

fn collect(paths: &[PathBuf]) -> Result<Vec<(PathBuf, Value)>, String> {
    let mut files = vec![];
    for p in paths {
        if p.is_dir() {
            let rd = std::fs::read_dir(p).map_err(|e| format!("{}: {e}", p.display()))?;
            let mut v: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| f.extension().is_some_and(|x| x == "json"))
                .collect();
            v.sort();
            files.extend(v);
        } else {
            files.push(p.clone());
        }
    }
    let mut out = vec![];
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if v["schema"]
            .as_str()
            .is_some_and(|s| s.starts_with("dn.bench."))
        {
            out.push((f, v));
        }
    }
    Ok(out)
}

fn gib(b: f64) -> String {
    format!("{:.1}", b / (1u64 << 30) as f64)
}

fn num(v: &Value, digits: usize) -> String {
    v.as_f64()
        .map(|x| format!("{x:.digits$}"))
        .unwrap_or_else(|| "–".into())
}

/// "macos/aarch64 · 16 GB unified".
fn machine_label(r: &Value) -> String {
    let m = &r["machine"];
    let mem = &m["memory"];
    // RAM is sold in GiB ("16 GB" is 16 GiB).
    let ram = mem["total_ram"].as_f64().unwrap_or(0.0) / (1u64 << 30) as f64;
    let kind = match mem["memory"]["kind"].as_str() {
        Some("unified") => "unified".to_string(),
        Some("discrete") => format!(
            "GPU {:.0} GB",
            mem["memory"]["vram"].as_f64().unwrap_or(0.0) / (1u64 << 30) as f64
        ),
        Some("cpu_only") => "CPU only".to_string(),
        _ => "?".to_string(),
    };
    format!(
        "{}/{} · {:.0} GB {kind}",
        m["os"].as_str().unwrap_or("?"),
        m["arch"].as_str().unwrap_or("?"),
        ram
    )
}

fn placement(r: &Value) -> String {
    let o = &r["options"];
    if o.is_null() {
        return "server".into();
    }
    let mut parts = vec![];
    match o["moe_offload"]["mode"].as_str() {
        Some("all") => parts.push("experts in RAM".to_string()),
        Some("first_layers") => parts.push(format!(
            "experts of {} layers in RAM",
            o["moe_offload"]["layers"]
        )),
        _ => {}
    }
    if let Some(g) = o["gpu_layers"].as_u64() {
        parts.push(format!("{g} layers on GPU"));
    }
    if parts.is_empty() {
        "all on GPU".into()
    } else {
        parts.join(", ")
    }
}

fn model_label(r: &Value) -> String {
    let m = &r["model"];
    m["file"]
        .as_str()
        .or_else(|| {
            m["path"]
                .as_str()
                .and_then(|p| Path::new(p).file_name()?.to_str())
        })
        .or_else(|| m["server_model"].as_str())
        .unwrap_or("?")
        .trim_end_matches(".gguf")
        .to_string()
}

pub fn table(reports: &[Value]) -> String {
    let mut rows: Vec<(String, String, String, String)> = reports
        .iter()
        .map(|r| {
            let quality = match (
                r["quality"]["passed"].as_u64(),
                r["quality"]["total"].as_u64(),
            ) {
                (Some(p), Some(t)) => format!("{p}/{t}"),
                _ => "–".into(),
            };
            let line = format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                model_label(r),
                r["model"]["quant"].as_str().unwrap_or("–"),
                r["backend"].as_str().unwrap_or("?"),
                placement(r),
                r["model"]["n_ctx"]
                    .as_u64()
                    .or_else(|| r["options"]["n_ctx"].as_u64())
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "–".into()),
                num(&r["prompt_tokens_per_s"]["mean"], 1),
                num(&r["gen_tokens_per_s"]["mean"], 1),
                num(&r["chat"]["ttft_ms"]["mean"], 0),
                num(&r["chat"]["gen_tokens_per_s"]["mean"], 1),
                r["peak_rss_bytes"]
                    .as_f64()
                    .map(gib)
                    .unwrap_or_else(|| "–".into()),
                quality
            );
            (
                machine_label(r),
                model_label(r),
                r["backend"].as_str().unwrap_or("").to_string(),
                line,
            )
        })
        .collect();
    rows.sort();
    let mut out = String::new();
    let mut current = None;
    for (machine, _, _, line) in rows {
        if current.as_ref() != Some(&machine) {
            out.push_str(&format!(
                "\n### {machine}\n\n\
                 | model | quant | backend | placement | ctx | pp t/s | tg t/s | first token ms | chat t/s | peak RSS GiB | quality |\n\
                 |---|---|---|---|---:|---:|---:|---:|---:|---:|---:|\n"
            ));
            current = Some(machine);
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.trim_start().to_string()
}

pub fn run(a: ReportArgs) -> Result<(), String> {
    let found = collect(&a.paths)?;
    if found.is_empty() {
        return Err("no dn bench reports found".into());
    }
    let reports: Vec<Value> = found.into_iter().map(|(_, v)| v).collect();
    let md = format!(
        "{}\n\npp/tg: llama-bench-style prompt and generation tokens/s (in-process llama.cpp only). \
         First token and chat t/s: the same long prompt streamed through the chat path, so \
         backends compare on equal terms. Peak RSS: of `dn`, or of the server with `--server-pid`. \
         Quality: the 14-prompt smoke set (`dn bench --quality`).\n",
        table(&reports)
    );
    match a.out {
        Some(p) => std::fs::write(&p, md).map_err(|e| format!("{}: {e}", p.display()))?,
        None => print!("{md}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn local() -> Value {
        json!({
            "schema": "dn.bench.v2",
            "backend": "llamacpp",
            "model": {"file": "Qwen3-30B-A3B-Q4_K_M.gguf", "quant": "Q4_K_M", "n_ctx": 720},
            "machine": {"os": "macos", "arch": "aarch64",
                "memory": {"total_ram": 34359738368u64, "memory": {"kind": "unified", "gpu_budget": null}}},
            "options": {"n_ctx": 720, "gpu_layers": null, "moe_offload": {"mode": "first_layers", "layers": 20}},
            "prompt_tokens_per_s": {"mean": 812.34, "stddev": 3.0},
            "gen_tokens_per_s": {"mean": 41.26, "stddev": 0.5},
            "chat": {"ttft_ms": {"mean": 640.2, "stddev": 1.0}, "gen_tokens_per_s": {"mean": 39.9, "stddev": 0.2}, "runs": []},
            "quality": {"passed": 13, "total": 14, "items": []},
            "peak_rss_bytes": 19327352832u64
        })
    }

    #[test]
    fn rows_group_by_machine_and_show_placement() {
        let mut server = local();
        server["backend"] = "mistralrs".into();
        server["options"] = Value::Null;
        server["prompt_tokens_per_s"] = Value::Null;
        server["gen_tokens_per_s"] = Value::Null;
        server["quality"] = Value::Null;
        server["peak_rss_bytes"] = Value::Null;
        let t = table(&[server, local()]);
        assert_eq!(
            t.matches("### macos/aarch64 · 32 GB unified").count(),
            1,
            "{t}"
        );
        assert!(t.contains(
            "| Qwen3-30B-A3B-Q4_K_M | Q4_K_M | llamacpp | experts of 20 layers in RAM | 720 | 812.3 | 41.3 | 640 | 39.9 | 18.0 | 13/14 |"
        ), "{t}");
        assert!(
            t.contains("| mistralrs | server | 720 | – | – | 640 | 39.9 | – | – |"),
            "{t}"
        );
        // llamacpp sorts before mistralrs within the same model.
        assert!(t.find("llamacpp").unwrap() < t.find("mistralrs").unwrap());
    }

    #[test]
    fn v1_reports_without_chat_still_render() {
        let mut v1 = local();
        v1["schema"] = "dn.bench.v1".into();
        v1.as_object_mut().unwrap().remove("chat");
        v1.as_object_mut().unwrap().remove("quality");
        let t = table(&[v1]);
        assert!(t.contains("| 812.3 | 41.3 | – | – | 18.0 | – |"), "{t}");
    }
}
