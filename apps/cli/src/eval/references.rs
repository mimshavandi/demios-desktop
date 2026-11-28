//! `dn eval references` (#407, ADR-0020 §8 and Q5): reference answers for pairwise judging,
//! from Claude and ChatGPT through their APIs, under the owner's keys.
//!
//! Each case is answered exactly as `dn eval parity` answers it (the same history, project
//! instructions, quoted files and mock tools, shared through `parity::answer_case`), so a
//! reference is a fair comparison. The answers go to `<out>/<source>/<case id>.md`, and
//! `<out>/<source>/meta.json` records the model, the settings and a hash of every case as it
//! was answered, so `dn eval judge` can refuse a reference whose case has changed since.
//!
//! No silent fallbacks: the key comes from an environment variable and is required; a case
//! that fails (or answers nothing) writes no file, removes an older one for it, is listed in
//! `meta.json` as failed and makes the command exit with an error. References for a holdout
//! stay outside any git work tree, as the holdout does.

use super::in_git_work_tree;
use super::judge::{plain_name, sha256_hex};
use super::parity::{Case, Set, SetRoot, answer_case};
use clap::{Args, ValueEnum};
use dn_core::chat::ChatModel;
use dn_core::model::ModelRef;
use dn_providers::{ProviderConfig, ProviderKind};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Anthropic's API.
    Claude,
    /// OpenAI's API.
    Chatgpt,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Chatgpt => "chatgpt",
        }
    }

    fn kind(self) -> ProviderKind {
        match self {
            Self::Claude => ProviderKind::Anthropic,
            Self::Chatgpt => ProviderKind::OpenAi,
        }
    }

    fn default_key_env(self) -> &'static str {
        match self {
            Self::Claude => "ANTHROPIC_API_KEY",
            Self::Chatgpt => "OPENAI_API_KEY",
        }
    }
}

#[derive(Args)]
pub struct ReferencesArgs {
    /// The golden set (prompts/, files/, tools.yaml).
    #[arg(long)]
    set: PathBuf,
    /// A private holdout with the same shape (outside any git work tree); its references
    /// must go outside one too.
    #[arg(long)]
    holdout: Option<PathBuf>,
    /// Which provider answers.
    #[arg(long, value_enum)]
    source: Source,
    /// The exact model id at that provider (recorded in meta.json).
    #[arg(long)]
    model: String,
    /// The environment variable holding the provider's key (default: ANTHROPIC_API_KEY or
    /// OPENAI_API_KEY).
    #[arg(long)]
    api_key_env: Option<String>,
    /// The references folder; answers go to <out>/<source>/<case id>.md.
    #[arg(long)]
    out: PathBuf,
    /// Only cases whose id starts with this.
    #[arg(long)]
    only: Option<String>,
    /// Output tokens per model call at most (as `dn eval parity`).
    #[arg(long, default_value_t = 1024)]
    max_tokens: u32,
    /// The chat skills a case can turn on with `skill:` (the app's library).
    #[arg(long, default_value = "skills/library")]
    skills: PathBuf,
}

/// A case as it was answered: its hash, for spotting a reference whose case has changed.
pub fn case_sha256(case: &Case) -> String {
    sha256_hex(serde_json::to_string(case).unwrap_or_default().as_bytes())
}

/// What one run did per case.
#[derive(Debug, PartialEq, Serialize)]
pub struct Outcome {
    pub id: String,
    /// `None` when the case failed (no file written).
    pub error: Option<String>,
}

/// Answers `cases` with `model` and writes the references under `dir` (`<out>/<source>`),
/// updating its `meta.json`. A `meta.json` from another model is refused.
pub async fn write_references(
    model: &dyn ChatModel,
    model_ref: &ModelRef,
    set: &Set,
    cases: &[&(Case, SetRoot)],
    dir: &Path,
    meta_head: serde_json::Value,
    max_tokens: u32,
) -> Result<Vec<Outcome>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let meta_path = dir.join("meta.json");
    let mut meta = match std::fs::read_to_string(&meta_path) {
        Ok(s) => {
            let v: serde_json::Value =
                serde_json::from_str(&s).map_err(|e| format!("{}: {e}", meta_path.display()))?;
            if v["model"] != meta_head["model"] {
                return Err(format!(
                    "{} holds references from {}; pass another --out or remove them first",
                    dir.display(),
                    v["model"]
                ));
            }
            v
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("{}: {e}", meta_path.display())),
    };
    if let (Some(m), Some(h)) = (meta.as_object_mut(), meta_head.as_object()) {
        for (k, v) in h {
            m.insert(k.clone(), v.clone());
        }
    }
    let n = cases.len();
    let mut outcomes = Vec::with_capacity(n);
    for (i, (case, root)) in cases.iter().map(|c| (&c.0, &c.1)).enumerate() {
        if !plain_name(&case.id) {
            return Err(format!("case id {:?} can't name a reference file", case.id));
        }
        let file = dir.join(format!("{}.md", case.id));
        let (t, tools) = answer_case(
            model,
            model_ref,
            "eval.references",
            set,
            case,
            root,
            max_tokens,
        )
        .await?;
        let error = match (&t.error, t.text.trim().is_empty()) {
            (Some(e), _) => Some(e.clone()),
            (None, true) => Some("the answer was empty".into()),
            (None, false) => None,
        };
        match &error {
            None => {
                std::fs::write(&file, &t.text).map_err(|e| format!("{}: {e}", file.display()))?
            }
            Some(_) => match std::fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", file.display())),
            },
        }
        let entry = serde_json::json!({
            "case_sha256": case_sha256(case),
            "failed": error.is_some(),
            "error": error,
            "stop": t.result.stop,
            "usage": t.result.usage,
            "tool_calls": tools.calls().len(),
        });
        meta["cases"][case.id.as_str()] = entry;
        eprintln!(
            "[eval references] {}/{n} {}: {}",
            i + 1,
            case.id,
            error.as_deref().unwrap_or("written")
        );
        outcomes.push(Outcome {
            id: case.id.clone(),
            error,
        });
    }
    let text = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
    std::fs::write(&meta_path, text + "\n").map_err(|e| format!("{}: {e}", meta_path.display()))?;
    Ok(outcomes)
}

pub async fn run(a: ReferencesArgs) -> Result<(), String> {
    let source = a.source;
    if let Some(h) = &a.holdout {
        if in_git_work_tree(&h.join("prompts")) {
            return Err(format!(
                "the holdout {} is inside a git work tree: keep it outside any repository",
                h.display()
            ));
        }
        if in_git_work_tree(&a.out.join(source.name())) {
            return Err(
                "a holdout's references must go outside any git work tree: pass --out with a \
                 path there"
                    .into(),
            );
        }
    }
    let key_env = a
        .api_key_env
        .clone()
        .unwrap_or_else(|| source.default_key_env().into());
    let key = std::env::var(&key_env).map_err(|_| {
        format!(
            "${key_env} is not set: the {} key is read from it",
            source.name()
        )
    })?;
    let set = Set::read(&a.set, a.holdout.as_deref(), &a.skills, false)?;
    let cases: Vec<&(Case, SetRoot)> = set
        .cases
        .iter()
        .filter(|(c, _)| a.only.as_ref().is_none_or(|p| c.id.starts_with(p.as_str())))
        .collect();
    if cases.is_empty() {
        return Err("no cases to answer".into());
    }
    let mut cfg = ProviderConfig::new(source.name(), source.kind());
    cfg.api_key = Some(dn_secrets::Secret::new(key));
    let provider = dn_providers::build(cfg).map_err(|e| e.to_string())?;
    let model_ref = ModelRef::Byok {
        provider_id: source.name().into(),
        model_id: a.model.clone(),
    };
    let head = serde_json::json!({
        "schema": "dn.references.v1",
        "source": source.name(),
        "model": a.model,
        "dn_version": env!("CARGO_PKG_VERSION"),
        "unix_time": crate::bench::now(),
        "system": "stand-in",
        "settings": { "temperature": 0, "reasoning": "off", "max_tokens": a.max_tokens },
        "holdout": a.holdout.is_some(),
    });
    let dir = a.out.join(source.name());
    let outcomes = write_references(
        &*provider,
        &model_ref,
        &set,
        &cases,
        &dir,
        head,
        a.max_tokens,
    )
    .await?;
    let failed: Vec<&str> = outcomes
        .iter()
        .filter(|o| o.error.is_some())
        .map(|o| o.id.as_str())
        .collect();
    println!(
        "{} references written, {} failed · {}",
        outcomes.len() - failed.len(),
        failed.len(),
        dir.display()
    );
    if !failed.is_empty() {
        return Err(format!(
            "no reference for {} (listed in meta.json); run them again with --only",
            failed.join(", ")
        ));
    }
    Ok(())
}

/// The cases whose reference was made from a different version of the case (by
/// `meta.json`'s `case_sha256`), or with no record of it.
pub fn stale(meta: &serde_json::Value, cases: &BTreeMap<&str, &Case>, ids: &[&str]) -> Vec<String> {
    ids.iter()
        .filter(|id| {
            let Some(case) = cases.get(*id) else {
                return false;
            };
            meta["cases"][**id]["case_sha256"].as_str() != Some(case_sha256(case).as_str())
        })
        .map(|id| id.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_core::chat::{ChatRequest, EventStream, FinishReason, StreamEvent, Usage};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// A provider that answers with the given texts in order ("" for a failure).
    struct Scripted(Mutex<VecDeque<String>>);
    struct Items(VecDeque<Result<StreamEvent, dn_core::CoreError>>);
    impl futures_core::Stream for Items {
        type Item = Result<StreamEvent, dn_core::CoreError>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(self.0.pop_front())
        }
    }
    impl ChatModel for Scripted {
        fn chat(&self, _req: ChatRequest) -> EventStream {
            let text = self.0.lock().unwrap().pop_front().unwrap_or_default();
            let events = if text == "FAIL" {
                vec![Err(dn_core::CoreError::Cancelled)]
            } else {
                vec![
                    Ok(StreamEvent::TextDelta { text }),
                    Ok(StreamEvent::Done {
                        finish_reason: FinishReason::Stop,
                        usage: Usage {
                            input_tokens: 10,
                            output_tokens: 5,
                            ..Default::default()
                        },
                    }),
                ]
            };
            Box::pin(Items(events.into()))
        }
    }

    fn set() -> Set {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        Set::read(
            &root.join("eval/parity-example"),
            None,
            &root.join("../../skills/library"),
            false,
        )
        .unwrap()
    }

    fn model_ref() -> ModelRef {
        ModelRef::Byok {
            provider_id: "claude".into(),
            model_id: "scripted".into(),
        }
    }

    fn head(model: &str) -> serde_json::Value {
        serde_json::json!({ "schema": "dn.references.v1", "source": "claude", "model": model })
    }

    #[tokio::test]
    async fn references_are_written_per_case_and_failures_leave_no_file() {
        let set = set();
        let cases: Vec<&(Case, SetRoot)> = set.cases.iter().take(2).collect();
        assert_eq!(cases.len(), 2);
        let dir = std::env::temp_dir().join(format!("dn-refs-{}", dn_core::Id::new()));
        let (a, b) = (cases[0].0.id.clone(), cases[1].0.id.clone());
        // An older reference for the case that will fail must not survive.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{b}.md")), "old").unwrap();
        let m = Scripted(Mutex::new(["The answer.", "FAIL"].map(String::from).into()));
        let out = write_references(&m, &model_ref(), &set, &cases, &dir, head("m-1"), 256)
            .await
            .unwrap();
        assert!(out[0].error.is_none() && out[1].error.is_some());
        assert_eq!(
            std::fs::read_to_string(dir.join(format!("{a}.md"))).unwrap(),
            "The answer."
        );
        assert!(!dir.join(format!("{b}.md")).exists());
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["model"], "m-1");
        assert_eq!(meta["cases"][a.as_str()]["failed"], false);
        assert_eq!(meta["cases"][b.as_str()]["failed"], true);
        assert_eq!(
            meta["cases"][a.as_str()]["case_sha256"],
            case_sha256(&cases[0].0)
        );

        // Another model's references in the same folder are refused, not mixed.
        let m = Scripted(Mutex::new(["x"].map(String::from).into()));
        let e = write_references(&m, &model_ref(), &set, &cases[..1], &dir, head("m-2"), 256)
            .await
            .unwrap_err();
        assert!(e.contains("m-1"), "{e}");

        // An empty answer is a failure too.
        let m = Scripted(Mutex::new([""].map(String::from).into()));
        let out = write_references(&m, &model_ref(), &set, &cases[..1], &dir, head("m-1"), 256)
            .await
            .unwrap();
        assert_eq!(out[0].error.as_deref(), Some("the answer was empty"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reference_is_stale_when_its_case_changed() {
        let set = set();
        let (case, _) = &set.cases[0];
        let mut changed = case.clone();
        changed.turns[0].content.push_str(" (edited)");
        let meta = serde_json::json!({ "cases": { case.id.as_str(): { "case_sha256": case_sha256(case) } } });
        let ids = [case.id.as_str()];
        let same: BTreeMap<&str, &Case> = [(case.id.as_str(), case)].into();
        assert!(stale(&meta, &same, &ids).is_empty());
        let edited: BTreeMap<&str, &Case> = [(case.id.as_str(), &changed)].into();
        assert_eq!(stale(&meta, &edited, &ids), std::slice::from_ref(&case.id));
        assert_eq!(
            stale(&serde_json::Value::Null, &same, &ids).len(),
            1,
            "no record"
        );
    }
}
