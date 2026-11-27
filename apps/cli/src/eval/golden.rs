//! Golden prompts (ADR-0020 §8 "parity"): answers each prompt of a `dn.golden.v1` file and
//! records the answer with its run result. Judging against the reference answers (pairwise,
//! a fixed judge from another model family) reads these reports; it isn't part of R0.
//!
//! The prompt set lives outside this repository's code (W-docs owns it); a **private**
//! holdout (`"private": true`) is refused when it, or its report, is inside a git work tree,
//! so it can't be committed by accident.

use super::{ModelArgs, Report, Turn, eval_config, in_git_work_tree, turn};
use clap::Args;
use dn_core::chat::{ChatMessage, Role};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "dn.golden.v1";

/// What the prompts are about (the report counts per category).
pub const CATEGORIES: &[&str] = &["chat", "projects", "tools", "research"];

#[derive(Args)]
pub struct GoldenArgs {
    /// The golden-prompt file (`dn.golden.v1`).
    file: PathBuf,
    #[command(flatten)]
    model: ModelArgs,
    /// Output tokens per answer at most.
    #[arg(long, default_value_t = 1024)]
    max_tokens: u32,
    /// Only the first N prompts.
    #[arg(long)]
    limit: Option<usize>,
    /// Report path (default: eval-golden-<time>.json).
    #[arg(long, short)]
    out: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenFile {
    pub format: String,
    #[serde(default)]
    pub about: String,
    /// A private holdout: it and its report must stay outside any git work tree.
    #[serde(default)]
    pub private: bool,
    /// System prompt for every item that doesn't set its own.
    #[serde(default)]
    pub system: Option<String>,
    pub items: Vec<GoldenItem>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenItem {
    pub id: String,
    pub category: String,
    #[serde(default)]
    pub system: Option<String>,
    /// The conversation so far; the last turn is the user's.
    pub turns: Vec<GoldenTurn>,
    /// Reference answers by source (e.g. `claude`, `chatgpt`), for the judge.
    #[serde(default)]
    pub references: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenTurn {
    pub role: GoldenRole,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoldenRole {
    User,
    Assistant,
}

impl GoldenFile {
    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let f: GoldenFile =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        f.check().map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(f)
    }

    /// The format rules: known format and categories, unique ids, a user turn last.
    pub fn check(&self) -> Result<(), String> {
        if self.format != FORMAT {
            return Err(format!("format is `{}`, expected `{FORMAT}`", self.format));
        }
        if self.items.is_empty() {
            return Err("no items".into());
        }
        let mut ids = HashSet::new();
        for it in &self.items {
            if it.id.trim().is_empty() {
                return Err("an item has an empty id".into());
            }
            if !ids.insert(it.id.as_str()) {
                return Err(format!("item id `{}` appears twice", it.id));
            }
            if !CATEGORIES.contains(&it.category.as_str()) {
                return Err(format!(
                    "item `{}`: category `{}` isn't one of {}",
                    it.id,
                    it.category,
                    CATEGORIES.join(", ")
                ));
            }
            match it.turns.last() {
                Some(t) if t.role == GoldenRole::User && !t.content.trim().is_empty() => {}
                _ => {
                    return Err(format!(
                        "item `{}`: the last turn must be a non-empty user turn",
                        it.id
                    ));
                }
            }
        }
        Ok(())
    }

    fn history(&self, it: &GoldenItem) -> Vec<ChatMessage> {
        let mut h = Vec::new();
        if let Some(s) = it.system.as_ref().or(self.system.as_ref()) {
            h.push(ChatMessage::text(Role::System, s.clone()));
        }
        h.extend(it.turns.iter().map(|t| {
            let role = match t.role {
                GoldenRole::User => Role::User,
                GoldenRole::Assistant => Role::Assistant,
            };
            ChatMessage::text(role, t.content.clone())
        }));
        h
    }
}

#[derive(Serialize)]
struct Item {
    id: String,
    category: String,
    /// The sources with a reference answer (the judge compares against them).
    references: Vec<String>,
    #[serde(flatten)]
    turn: Turn,
}

pub async fn run(a: GoldenArgs) -> Result<(), String> {
    let file = GoldenFile::read(&a.file)?;
    let out = a.out.clone();
    if file.private {
        if in_git_work_tree(&a.file) {
            return Err(format!(
                "{} is a private holdout inside a git work tree: keep it outside any repository",
                a.file.display()
            ));
        }
        match &out {
            Some(o) if !in_git_work_tree(o) => {}
            _ => {
                return Err(
                    "a private holdout's report must go outside any git work tree: pass \
                            --out with a path there"
                        .into(),
                );
            }
        }
    }
    let m = a.model.load(None).await?;
    let cfg = eval_config("eval.golden", &m.model_ref, Some(a.max_tokens));
    let n = a.limit.unwrap_or(file.items.len()).min(file.items.len());
    let mut items = Vec::with_capacity(n);
    for (i, it) in file.items.iter().take(n).enumerate() {
        let t = turn(&m.chat, &cfg, file.history(it)).await;
        eprintln!(
            "[eval golden] {}/{n} {}: {:?}, {} output tokens, {} ms",
            i + 1,
            it.id,
            t.result.stop,
            t.result.usage.output_tokens,
            t.result.wall_ms
        );
        items.push(Item {
            id: it.id.clone(),
            category: it.category.clone(),
            references: it.references.keys().cloned().collect(),
            turn: t,
        });
    }
    let failed = items.iter().filter(|i| i.turn.error.is_some()).count();
    let mut by_category = BTreeMap::<&str, usize>::new();
    for i in &items {
        *by_category.entry(i.category.as_str()).or_default() += 1;
    }
    let summary = serde_json::json!({
        "file": a.file.file_name().map(|s| s.to_string_lossy()),
        "about": file.about,
        "private": file.private,
        "answered": items.len() - failed,
        "failed": failed,
        "by_category": by_category,
        "judged": false,
    });
    let report = Report::new("golden", &m, summary, items);
    let path = report.write(out)?;
    m.chat.unload().await?;
    println!(
        "{} answered, {failed} failed · report: {}",
        report.items.len() - failed,
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<GoldenFile, String> {
        let f: GoldenFile = serde_json::from_str(s).map_err(|e| e.to_string())?;
        f.check().map(|()| f)
    }

    #[test]
    fn the_example_file_is_valid() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/golden.example.json");
        let f = GoldenFile::read(&p).unwrap();
        assert!(!f.private);
        let h = f.history(&f.items[0]);
        assert_eq!(h.last().unwrap().role, Role::User);
    }

    #[test]
    fn bad_files_are_refused_with_the_reason() {
        let item = |id: &str, cat: &str, role: &str| {
            format!(
                r#"{{"id":"{id}","category":"{cat}","turns":[{{"role":"{role}","content":"hi"}}]}}"#
            )
        };
        let file = |items: &[String]| {
            format!(
                r#"{{"format":"dn.golden.v1","items":[{}]}}"#,
                items.join(",")
            )
        };
        assert!(parse(&file(&[item("a", "chat", "user")])).is_ok());
        let dup = parse(&file(&[
            item("a", "chat", "user"),
            item("a", "chat", "user"),
        ]));
        assert!(dup.unwrap_err().contains("twice"));
        assert!(
            parse(&file(&[item("a", "poetry", "user")]))
                .unwrap_err()
                .contains("category")
        );
        assert!(
            parse(&file(&[item("a", "chat", "assistant")]))
                .unwrap_err()
                .contains("last turn")
        );
        assert!(
            parse(r#"{"format":"dn.golden.v2","items":[]}"#)
                .unwrap_err()
                .contains("format")
        );
        assert!(parse(r#"{"format":"dn.golden.v1","items":[],"extra":1}"#).is_err());
    }
}
