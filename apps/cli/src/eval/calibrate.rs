//! `dn eval calibrate` (#355, ADR-0020 §8): how far the pairwise judge agrees with people.
//!
//! A labels file (`dn.labels.v1`) lists pairs of answers to a case of the set, each with a
//! person's verdict (`A`, `B` or `tie`). The judge compares every pair exactly as
//! `dn eval judge --references` does (both orders, a preference only when they agree), and
//! the report (`dn.calibration.v1`) gives the exact agreement, Cohen's κ over the three
//! verdicts, the confusion matrix and how often the order changed the judge's verdict.
//!
//! Saved as `calibration.json` next to `judge.yaml`, it's the calibration `dn eval judge`
//! quotes in its reports, but only for the same judge model and pairwise prompt.
//! Any judge error makes the report incomplete and the command fail, as in `judge`.

use super::in_git_work_tree;
use super::judge::{Better, Comparison, Judge, compare, context, sha256_hex};
use super::parity::{Case, Set, SetRoot};
use clap::Args;
use dn_inference::{LoadedModel, ModelSpec, OpenAiCompatBackend};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[derive(Args)]
pub struct CalibrateArgs {
    /// The labels file (`dn.labels.v1`): pairs of answers with a person's verdict.
    labels: PathBuf,
    /// The judge (`judge.yaml`, with a `pairwise_prompt`).
    #[arg(long)]
    judge: PathBuf,
    /// The golden set the labelled cases are from.
    #[arg(long)]
    set: PathBuf,
    /// A private holdout, if labelled cases are from it (outside any git work tree).
    #[arg(long)]
    holdout: Option<PathBuf>,
    /// The chat skill library (only read if a case names a skill).
    #[arg(long, default_value = "skills/library")]
    skills: PathBuf,
    /// Report path (default: eval-calibration-<time>.json). Save it as `calibration.json`
    /// next to `judge.yaml` for `dn eval judge` to quote.
    #[arg(long, short)]
    out: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelFile {
    pub format: String,
    pub pairs: Vec<Label>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub id: String,
    /// The case of the set both answers answer.
    pub case: String,
    /// The two answers: files next to the labels file (plain relative paths).
    pub a: String,
    pub b: String,
    /// The person's verdict.
    pub better: Better,
}

impl LabelFile {
    pub fn parse(yaml: &str) -> Result<Self, String> {
        let f: LabelFile = serde_yaml_ng::from_str(yaml).map_err(|e| e.to_string())?;
        if f.format != "dn.labels.v1" {
            return Err(format!("format is {:?}, expected dn.labels.v1", f.format));
        }
        if f.pairs.is_empty() {
            return Err("no pairs".into());
        }
        let mut seen = std::collections::HashSet::new();
        for p in &f.pairs {
            if !seen.insert(p.id.as_str()) {
                return Err(format!("pair {} appears twice", p.id));
            }
            for f in [&p.a, &p.b] {
                let rel = Path::new(f);
                if rel.as_os_str().is_empty()
                    || rel.components().any(|c| !matches!(c, Component::Normal(_)))
                {
                    return Err(format!(
                        "pair {}: {f:?} must be a plain path next to the labels file",
                        p.id
                    ));
                }
            }
        }
        Ok(f)
    }
}

/// The verdict classes, in the order of the confusion matrix.
const CLASSES: [Better; 3] = [Better::A, Better::B, Better::Tie];

fn class(b: Better) -> usize {
    CLASSES.iter().position(|c| *c == b).unwrap_or(2)
}

/// The agreement between people and the judge over judged pairs, from the confusion matrix
/// `m[person][judge]`: exact agreement and Cohen's κ, or why there's no κ.
#[derive(Debug, PartialEq, Serialize)]
pub struct Agreement {
    pub pairs: usize,
    pub agreement: Option<f64>,
    pub kappa: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kappa_note: Option<String>,
}

pub fn agreement(m: &[[usize; 3]; 3]) -> Agreement {
    let n: usize = m.iter().flatten().sum();
    if n == 0 {
        return Agreement {
            pairs: 0,
            agreement: None,
            kappa: None,
            kappa_note: Some("no judged pairs".into()),
        };
    }
    let nf = n as f64;
    let po = (0..3).map(|k| m[k][k]).sum::<usize>() as f64 / nf;
    let pe: f64 = (0..3)
        .map(|k| {
            let person = m[k].iter().sum::<usize>() as f64 / nf;
            let judge = (0..3).map(|r| m[r][k]).sum::<usize>() as f64 / nf;
            person * judge
        })
        .sum();
    let (kappa, kappa_note) = if (1.0 - pe).abs() < 1e-12 {
        (
            None,
            Some("people and judge both gave a single verdict throughout: κ is undefined".into()),
        )
    } else {
        (Some((po - pe) / (1.0 - pe)), None)
    };
    Agreement {
        pairs: n,
        agreement: Some(po),
        kappa,
        kappa_note,
    }
}

#[derive(Serialize)]
struct PairResult {
    id: String,
    case: String,
    person: Better,
    /// The judge's verdict over both orders; absent on a judge error.
    #[serde(skip_serializing_if = "Option::is_none")]
    judge: Option<Better>,
    orders_disagree: bool,
    orders: Vec<super::judge::Order>,
    judge_runs: Vec<dn_core::run::RunResult>,
}

pub async fn run(a: CalibrateArgs) -> Result<(), String> {
    let judge = Judge::read(&a.judge)?;
    let Some((pairwise_prompt, pairwise_sha)) = judge.pairwise.clone() else {
        return Err(format!(
            "{}: calibration needs pairwise_prompt in judge.yaml",
            a.judge.display()
        ));
    };
    let key = judge.key()?;
    if let Some(h) = &a.holdout {
        if in_git_work_tree(&h.join("prompts")) {
            return Err(format!(
                "the holdout {} is inside a git work tree: keep it outside any repository",
                h.display()
            ));
        }
        if in_git_work_tree(&a.labels) {
            return Err("labels for holdout cases stay outside any git work tree".into());
        }
        match &a.out {
            Some(o) if !in_git_work_tree(o) => {}
            _ => {
                return Err(
                    "with --holdout, the report must go outside any git work tree: pass --out \
                     with a path there"
                        .into(),
                );
            }
        }
    }
    let raw = std::fs::read(&a.labels).map_err(|e| format!("{}: {e}", a.labels.display()))?;
    let labels = LabelFile::parse(&String::from_utf8_lossy(&raw))
        .map_err(|e| format!("{}: {e}", a.labels.display()))?;
    let dir = a.labels.parent().unwrap_or(Path::new("."));
    let set = Set::read(&a.set, a.holdout.as_deref(), &a.skills, false)?;
    let by_id: BTreeMap<&str, &(Case, SetRoot)> =
        set.cases.iter().map(|c| (c.0.id.as_str(), c)).collect();
    // Everything is read before the first judge call.
    let mut work = Vec::with_capacity(labels.pairs.len());
    for p in &labels.pairs {
        let Some((case, root)) = by_id.get(p.case.as_str()) else {
            return Err(format!("pair {}: case {} isn't in the set", p.id, p.case));
        };
        let read = |f: &str| {
            std::fs::read_to_string(dir.join(f)).map_err(|e| format!("pair {}: {f}: {e}", p.id))
        };
        work.push((p, context(case, root)?, read(&p.a)?, read(&p.b)?));
    }

    let model = LoadedModel::load(
        Arc::new(OpenAiCompatBackend::new()),
        &ModelSpec::endpoint(&judge.file.endpoint, &judge.file.model, key),
        &Default::default(),
    )
    .await
    .map_err(|e| format!("the judge: {e}"))?;
    let judge_ref = dn_core::model::ModelRef::Remote {
        endpoint_id: "judge".into(),
        model_id: judge.file.model.clone(),
    };
    let n = work.len();
    let mut results = Vec::with_capacity(n);
    for (i, (p, ctx, x, y)) in work.into_iter().enumerate() {
        let Comparison {
            verdict,
            orders_disagree,
            orders,
            runs,
        } = compare(
            &model,
            &judge_ref,
            &pairwise_prompt,
            &ctx,
            (&x, "a"),
            (&y, "b"),
        )
        .await;
        let judged = verdict.map(|v| match v {
            Some(true) => Better::A,
            Some(false) => Better::B,
            None => Better::Tie,
        });
        eprintln!(
            "[eval calibrate] {}/{n} {}: person {:?}, judge {:?}",
            i + 1,
            p.id,
            p.better,
            judged
        );
        results.push(PairResult {
            id: p.id.clone(),
            case: p.case.clone(),
            person: p.better,
            judge: judged,
            orders_disagree,
            orders,
            judge_runs: runs,
        });
    }
    model.unload().await.map_err(|e| e.to_string())?;

    let report = report(
        &results,
        &judge.file.model,
        &judge.file.pairwise_prompt.clone().unwrap_or_default(),
        &pairwise_sha,
        &sha256_hex(&raw),
    );
    let out = a
        .out
        .unwrap_or_else(|| PathBuf::from(format!("eval-calibration-{}.json", crate::bench::now())));
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&out, json + "\n").map_err(|e| format!("{}: {e}", out.display()))?;
    let s = &report["summary"];
    println!(
        "{n} pairs: agreement {}, κ {}, orders disagreed {}, judge errors {} · report: {}",
        s["agreement"],
        s["kappa"],
        s["orders_disagree"],
        s["judge_errors"],
        out.display()
    );
    if s["judge_errors"].as_u64().unwrap_or(0) > 0 {
        return Err(format!(
            "{} pairs couldn't be judged; the report says \"complete\": false",
            s["judge_errors"]
        ));
    }
    Ok(())
}

/// The calibration report (`dn.calibration.v1`).
fn report(
    results: &[PairResult],
    model: &str,
    prompt: &str,
    prompt_sha256: &str,
    labels_sha256: &str,
) -> serde_json::Value {
    let mut m = [[0usize; 3]; 3];
    for r in results {
        if let Some(j) = r.judge {
            m[class(r.person)][class(j)] += 1;
        }
    }
    let errors = results.iter().filter(|r| r.judge.is_none()).count();
    let ag = agreement(&m);
    serde_json::json!({
        "schema": "dn.calibration.v1",
        "dn_version": env!("CARGO_PKG_VERSION"),
        "unix_time": crate::bench::now(),
        "judge": {
            "model": model,
            "pairwise_prompt": prompt,
            "pairwise_prompt_sha256": prompt_sha256,
        },
        "labels_sha256": labels_sha256,
        "summary": {
            "pairs": results.len(),
            "judged": ag.pairs,
            "judge_errors": errors,
            "complete": errors == 0,
            "agreement": ag.agreement,
            "kappa": ag.kappa,
            "kappa_note": ag.kappa_note,
            "orders_disagree": results.iter().filter(|r| r.orders_disagree).count(),
            "confusion": {
                "rows": "person (A, B, tie)",
                "columns": "judge (A, B, tie)",
                "matrix": m,
            },
        },
        "items": results,
    })
}

/// The calibration `dn eval judge` quotes: `calibration.json` next to `judge.yaml`, if it was
/// made with this judge model and pairwise prompt and is complete. `Err` says why there's none.
pub fn for_judge(judge_yaml: &Path, judge: &Judge) -> Result<serde_json::Value, String> {
    let p = judge_yaml
        .parent()
        .unwrap_or(Path::new("."))
        .join("calibration.json");
    let s = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("no {} for this judge", p.display()));
        }
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    let v: serde_json::Value =
        serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?;
    let sha = judge.pairwise.as_ref().map(|x| x.1.as_str());
    if v["schema"] != "dn.calibration.v1"
        || v["judge"]["model"].as_str() != Some(judge.file.model.as_str())
        || v["judge"]["pairwise_prompt_sha256"].as_str() != sha
    {
        return Err(format!(
            "{} was made with another judge model or pairwise prompt",
            p.display()
        ));
    }
    if v["summary"]["complete"] != true {
        return Err(format!("{} is incomplete", p.display()));
    }
    Ok(serde_json::json!({
        "labels_sha256": v["labels_sha256"],
        "pairs": v["summary"]["judged"],
        "agreement": v["summary"]["agreement"],
        "kappa": v["summary"]["kappa"],
        "orders_disagree": v["summary"]["orders_disagree"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kappa_from_the_confusion_matrix() {
        // Perfect agreement over varied verdicts.
        let a = agreement(&[[5, 0, 0], [0, 3, 0], [0, 0, 2]]);
        assert_eq!(a.agreement, Some(1.0));
        assert!((a.kappa.unwrap() - 1.0).abs() < 1e-12);
        // A worked example: 20 pairs, 14 agree.
        // person A: 8 (judge A 6, B 1, tie 1); B: 7 (A 1, B 5, tie 1); tie: 5 (A 1, B 1, tie 3)
        let a = agreement(&[[6, 1, 1], [1, 5, 1], [1, 1, 3]]);
        assert_eq!(a.pairs, 20);
        assert!((a.agreement.unwrap() - 0.7).abs() < 1e-12);
        // pe = (8·8 + 7·7 + 5·5) / 400 = 138/400 = 0.345; κ = (0.7 − 0.345) / 0.655
        assert!((a.kappa.unwrap() - 0.355 / 0.655).abs() < 1e-9);
        // One verdict throughout: κ is undefined, and says so.
        let a = agreement(&[[4, 0, 0], [0, 0, 0], [0, 0, 0]]);
        assert!(a.kappa.is_none() && a.kappa_note.is_some());
        assert!(agreement(&[[0; 3]; 3]).agreement.is_none());
    }

    #[test]
    fn a_labels_file_is_checked_before_anything_runs() {
        let ok = "format: dn.labels.v1\npairs:\n- {id: p1, case: chat-001, a: x/a.md, b: x/b.md, better: A}\n";
        let f = LabelFile::parse(ok).unwrap();
        assert_eq!(f.pairs[0].better, Better::A);
        for bad in [
            "format: dn.labels.v2\npairs:\n- {id: p1, case: c, a: a.md, b: b.md, better: A}\n",
            "format: dn.labels.v1\npairs: []\n",
            "format: dn.labels.v1\npairs:\n- {id: p1, case: c, a: ../a.md, b: b.md, better: A}\n",
            "format: dn.labels.v1\npairs:\n- {id: p1, case: c, a: /a.md, b: b.md, better: A}\n",
            "format: dn.labels.v1\npairs:\n- {id: p1, case: c, a: a.md, b: b.md, better: C}\n",
            "format: dn.labels.v1\npairs:\n- {id: p1, case: c, a: a.md, b: b.md, better: A}\n- {id: p1, case: c, a: a.md, b: b.md, better: B}\n",
        ] {
            assert!(LabelFile::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_report_counts_errors_and_the_judge_quotes_only_a_matching_calibration() {
        let r = |person, judge| PairResult {
            id: "p".into(),
            case: "c".into(),
            person,
            judge,
            orders_disagree: false,
            orders: Vec::new(),
            judge_runs: Vec::new(),
        };
        let results = vec![
            r(Better::A, Some(Better::A)),
            r(Better::B, Some(Better::B)),
            r(Better::Tie, None),
        ];
        let rep = report(&results, "m-1", "pairwise-v1.md", "abc", "lsha");
        assert_eq!(rep["summary"]["judge_errors"], 1);
        assert_eq!(rep["summary"]["complete"], false);
        assert_eq!(rep["summary"]["judged"], 2);

        let dir = std::env::temp_dir().join(format!("dn-cal-{}", dn_core::Id::new()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("c.md"), "Criteria.").unwrap();
        std::fs::write(dir.join("p.md"), "Pairwise.").unwrap();
        std::fs::write(
            dir.join("judge.yaml"),
            "model: m-1\nendpoint: https://e.test/v1\nprompt: c.md\npairwise_prompt: p.md\n",
        )
        .unwrap();
        let judge = Judge::read(&dir.join("judge.yaml")).unwrap();
        let yaml = dir.join("judge.yaml");
        assert!(for_judge(&yaml, &judge).unwrap_err().contains("no "));
        let sha = sha256_hex(b"Pairwise.");
        let write = |rep: &serde_json::Value| {
            std::fs::write(dir.join("calibration.json"), rep.to_string()).unwrap()
        };
        write(&rep);
        assert!(for_judge(&yaml, &judge).is_err(), "another prompt");
        write(&report(&results, "m-1", "p.md", &sha, "lsha"));
        assert!(for_judge(&yaml, &judge).unwrap_err().contains("incomplete"));
        write(&report(&results[..2], "m-1", "p.md", &sha, "lsha"));
        let c = for_judge(&yaml, &judge).unwrap();
        assert_eq!(c["agreement"], 1.0);
        write(&report(&results[..2], "m-2", "p.md", &sha, "lsha"));
        assert!(for_judge(&yaml, &judge).is_err(), "another model");
        let _ = std::fs::remove_dir_all(dir);
    }
}
