//! `dn bench-embed`: retrieval quality, speed and memory of a local embedding model on a
//! small bilingual (English and German) set (#128). Each question has one right document;
//! the model embeds documents and questions with its own prefixes, and the report gives
//! recall@1, recall@3 and MRR per language pair, plus embedding speed and peak memory.

use crate::bench::{PeakMem, gguf_json, machine_json, now};
use crate::resolve_model;
use clap::Args;
use dn_inference::{InferenceBackend, LlamaCppBackend, LoadOptions, ModelSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

const SET: &str = include_str!("embed_set.json");

#[derive(Args)]
pub struct EmbedArgs {
    /// GGUF path or downloaded catalogue id of an embedding model.
    model: String,
    /// Text put before each question (default: the model family's documented prefix).
    #[arg(long)]
    query_prefix: Option<String>,
    /// Text put before each document (default: the model family's documented prefix).
    #[arg(long)]
    doc_prefix: Option<String>,
    /// Report path (default: embed-<model>.json).
    #[arg(long, short)]
    out: Option<PathBuf>,
    /// Load a model whose check found Warnings. Blocked models are never loaded.
    #[arg(long)]
    accept_model_warnings: bool,
}

#[derive(Deserialize)]
struct Set {
    docs: Vec<Doc>,
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Doc {
    id: String,
    text: String,
}

#[derive(Deserialize)]
struct Query {
    lang: String,
    text: String,
    doc: String,
}

/// Query and document prefixes each model family documents for retrieval.
pub fn prefixes(file: &str) -> (&'static str, &'static str) {
    let f = file.to_ascii_lowercase();
    if f.contains("nomic-embed") {
        ("search_query: ", "search_document: ")
    } else if f.contains("qwen3-embedding") {
        (
            "Instruct: Given a question, retrieve the passage that answers it\nQuery:",
            "",
        )
    } else if f.contains("embeddinggemma") {
        ("task: search result | query: ", "title: none | text: ")
    } else if f.contains("e5-") {
        ("query: ", "passage: ")
    } else {
        // bge-m3, granite-embedding and others: no prefixes.
        ("", "")
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Scores {
    pub n: usize,
    pub recall_at_1: f64,
    pub recall_at_3: f64,
    pub mrr: f64,
}

/// Scores from the 1-based rank of the right document for each question.
pub fn scores(ranks: &[usize]) -> Scores {
    let n = ranks.len().max(1) as f64;
    Scores {
        n: ranks.len(),
        recall_at_1: ranks.iter().filter(|&&r| r == 1).count() as f64 / n,
        recall_at_3: ranks.iter().filter(|&&r| r <= 3).count() as f64 / n,
        mrr: ranks.iter().map(|&r| 1.0 / r as f64).sum::<f64>() / n,
    }
}

/// 1-based rank of `right` among `docs` by dot product with `q` (vectors are normalised).
pub fn rank(q: &[f32], docs: &[Vec<f32>], right: usize) -> usize {
    let dot = |d: &Vec<f32>| q.iter().zip(d).map(|(a, b)| a * b).sum::<f32>();
    let target = dot(&docs[right]);
    1 + docs
        .iter()
        .enumerate()
        .filter(|&(i, d)| i != right && dot(d) > target)
        .count()
}

#[derive(Serialize)]
struct Report {
    schema: &'static str,
    unix_time: u64,
    model: serde_json::Value,
    machine: serde_json::Value,
    query_prefix: String,
    doc_prefix: String,
    dimension: usize,
    load_ms: f64,
    /// Documents embedded per second (one at a time, as the index does).
    docs_per_s: f64,
    /// Mean time to embed one question.
    query_ms: f64,
    peak_rss_bytes: u64,
    overall: Scores,
    by_lang: BTreeMap<String, Scores>,
    misses: Vec<serde_json::Value>,
}

pub async fn run(a: EmbedArgs) -> Result<(), String> {
    let set: Set = serde_json::from_str(SET).map_err(|e| format!("embed_set.json: {e}"))?;
    let path = resolve_model(&a.model)?;
    let checked = crate::check_model(&path, a.accept_model_warnings).await?;
    let file = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (qp, dp) = prefixes(&file);
    let qp = a.query_prefix.clone().unwrap_or_else(|| qp.to_string());
    let dp = a.doc_prefix.clone().unwrap_or_else(|| dp.to_string());

    let mem = PeakMem::start(sysinfo::get_current_pid().map_err(|e| e.to_string())?);
    let backend = LlamaCppBackend::new();
    checked.confirm()?;
    let t0 = Instant::now();
    let h = backend
        .load(
            &ModelSpec::gguf(&path),
            &LoadOptions {
                n_ctx: Some(2048),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    let load_ms = t0.elapsed().as_secs_f64() * 1e3;
    // Warm-up, so the first document doesn't pay for kernel compilation.
    backend
        .embed(&h, &[format!("{dp}warm-up")])
        .await
        .map_err(|e| e.to_string())?;

    let doc_texts: Vec<String> = set.docs.iter().map(|d| format!("{dp}{}", d.text)).collect();
    let t = Instant::now();
    let docs = backend
        .embed(&h, &doc_texts)
        .await
        .map_err(|e| e.to_string())?;
    let docs_per_s = docs.len() as f64 / t.elapsed().as_secs_f64();
    let q_texts: Vec<String> = set
        .queries
        .iter()
        .map(|q| format!("{qp}{}", q.text))
        .collect();
    let t = Instant::now();
    let queries = backend
        .embed(&h, &q_texts)
        .await
        .map_err(|e| e.to_string())?;
    let query_ms = t.elapsed().as_secs_f64() * 1e3 / queries.len().max(1) as f64;
    backend.unload(h).await.map_err(|e| e.to_string())?;
    let peak_rss_bytes = mem.finish();

    let index: BTreeMap<&str, usize> = set
        .docs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.id.as_str(), i))
        .collect();
    let mut all = vec![];
    let mut by: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut misses = vec![];
    for (q, v) in set.queries.iter().zip(&queries) {
        let right = *index
            .get(q.doc.as_str())
            .ok_or_else(|| format!("embed_set.json: unknown doc {}", q.doc))?;
        let r = rank(v, &docs, right);
        if r > 1 {
            misses.push(serde_json::json!({"query": q.text, "lang": q.lang, "rank": r}));
        }
        all.push(r);
        by.entry(q.lang.clone()).or_default().push(r);
    }
    let report = Report {
        schema: "dn.bench-embed.v1",
        unix_time: now(),
        model: gguf_json(&path)?,
        machine: machine_json(),
        query_prefix: qp,
        doc_prefix: dp,
        dimension: docs.first().map(Vec::len).unwrap_or(0),
        load_ms,
        docs_per_s,
        query_ms,
        peak_rss_bytes,
        overall: scores(&all),
        by_lang: by.into_iter().map(|(k, v)| (k, scores(&v))).collect(),
        misses,
    };
    println!(
        "{file}: dim {} · overall R@1 {:.2} R@3 {:.2} MRR {:.3} · {:.0} docs/s · {:.1} ms/query · peak RSS {:.2} GiB",
        report.dimension,
        report.overall.recall_at_1,
        report.overall.recall_at_3,
        report.overall.mrr,
        report.docs_per_s,
        report.query_ms,
        report.peak_rss_bytes as f64 / (1u64 << 30) as f64
    );
    for (lang, s) in &report.by_lang {
        println!(
            "  {lang:6} n={:2} R@1 {:.2} R@3 {:.2} MRR {:.3}",
            s.n, s.recall_at_1, s.recall_at_3, s.mrr
        );
    }
    let out = a.out.unwrap_or_else(|| {
        PathBuf::from(format!(
            "embed-{}.json",
            path.file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        ))
    });
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&out, json + "\n").map_err(|e| format!("{}: {e}", out.display()))?;
    println!("report: {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_is_consistent() {
        let set: Set = serde_json::from_str(SET).unwrap();
        let ids: std::collections::HashSet<_> = set.docs.iter().map(|d| d.id.as_str()).collect();
        // Near-miss documents are never the answer.
        assert!(set.queries.iter().all(|q| !q.doc.starts_with('x')));
        assert_eq!(ids.len(), set.docs.len(), "unique doc ids");
        assert!(set.queries.iter().all(|q| ids.contains(q.doc.as_str())));
        for lang in ["en", "de", "de-en", "en-de"] {
            assert!(set.queries.iter().any(|q| q.lang == lang), "{lang}");
        }
        // Cross-language questions point at documents in the other language.
        for q in &set.queries {
            match q.lang.as_str() {
                "en" | "de-en" => assert!(q.doc.starts_with('e')),
                "de" | "en-de" => assert!(q.doc.starts_with('g')),
                other => panic!("unknown lang {other}"),
            }
        }
    }

    #[test]
    fn ranks_and_scores() {
        let docs = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![0.6, 0.8]];
        assert_eq!(rank(&[1.0, 0.0], &docs, 0), 1);
        assert_eq!(rank(&[1.0, 0.0], &docs, 1), 3);
        let s = scores(&[1, 2, 4, 1]);
        assert_eq!((s.recall_at_1, s.recall_at_3), (0.5, 0.75));
        assert!((s.mrr - (1.0 + 0.5 + 0.25 + 1.0) / 4.0).abs() < 1e-12);
    }

    #[test]
    fn family_prefixes() {
        assert_eq!(
            prefixes("nomic-embed-text-v1.5.Q4_K_M.gguf").0,
            "search_query: "
        );
        assert_eq!(
            prefixes("multilingual-e5-small-q8_0.gguf"),
            ("query: ", "passage: ")
        );
        assert_eq!(prefixes("bge-m3-q8_0.gguf"), ("", ""));
        assert!(
            prefixes("Qwen3-Embedding-0.6B-Q8_0.gguf")
                .0
                .starts_with("Instruct: ")
        );
    }
}
