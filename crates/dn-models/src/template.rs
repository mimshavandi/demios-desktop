//! Where a local chat model's prompt template comes from (T-INF-23). Every local chat model
//! renders with its own Jinja template; there is no generic fallback. The template is:
//!
//! 1. the GGUF's `tokenizer.chat_template`, or
//! 2. for a GGUF without one, the `chat_template` of the pinned Hugging Face repo's
//!    `tokenizer_config.json`, fetched when the model is downloaded and stored next to it as
//!    `<gguf>`[`SUFFIX`] (so nothing is fetched at chat time).
//!
//! A model with neither can't be added as a chat model. Embedding models need no template.

use crate::gguf::{Gguf, MetaValue};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Suffix of the template file stored next to a GGUF. `dn-inference` reads the same name
/// (`dn_inference::llamacpp::CHAT_TEMPLATE_SUFFIX`).
pub const SUFFIX: &str = ".chat_template.jinja";

/// Largest template accepted (real ones are under 32 KiB).
pub const MAX_TEMPLATE_BYTES: usize = 1 << 20;

/// User-facing reason a model can't be used for chat.
pub const NO_TEMPLATE_MESSAGE: &str = "This model has no chat template";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateSource {
    /// `tokenizer.chat_template` in the GGUF.
    Gguf,
    /// The repo's `tokenizer_config.json`, stored next to the GGUF.
    TokenizerConfig,
}

/// `<gguf>.chat_template.jinja`.
pub fn sidecar_path(gguf: &Path) -> PathBuf {
    let mut p = gguf.as_os_str().to_owned();
    p.push(SUFFIX);
    PathBuf::from(p)
}

/// An embedding model (it declares a pooling type, as llama.cpp's embedding models do, or is a
/// BERT-family architecture). These need no chat template.
pub fn is_embedding(g: &Gguf) -> bool {
    let arch = g.arch();
    arch.ends_with("bert")
        || matches!(
            g.get(&format!("{arch}.pooling_type")),
            Some(MetaValue::Int(_))
        )
}

/// Where the chat template of the GGUF at `path` (already parsed as `g`) comes from, or `None`.
pub fn source(g: &Gguf, path: &Path) -> Option<TemplateSource> {
    if g.chat_template().is_some() {
        return Some(TemplateSource::Gguf);
    }
    std::fs::metadata(sidecar_path(path))
        .ok()
        .filter(|m| m.is_file() && m.len() > 0 && m.len() <= MAX_TEMPLATE_BYTES as u64)
        .map(|_| TemplateSource::TokenizerConfig)
}

/// The `chat_template` of a `tokenizer_config.json`: a string, or a list of named templates
/// of which the one named `default` is used.
pub fn from_tokenizer_config(json: &serde_json::Value) -> Option<String> {
    let t = match json.get("chat_template")? {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(list) => list
            .iter()
            .find(|x| x.get("name").and_then(|n| n.as_str()) == Some("default"))?
            .get("template")?
            .as_str()?
            .to_string(),
        _ => return None,
    };
    (!t.trim().is_empty() && t.len() <= MAX_TEMPLATE_BYTES).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tokenizer_config_string_or_default_entry() {
        assert_eq!(
            from_tokenizer_config(&json!({"chat_template": "{{ x }}"})).as_deref(),
            Some("{{ x }}")
        );
        let list = json!({"chat_template": [
            {"name": "tool_use", "template": "T"},
            {"name": "default", "template": "D"}
        ]});
        assert_eq!(from_tokenizer_config(&list).as_deref(), Some("D"));
        assert_eq!(
            from_tokenizer_config(&json!({"chat_template": [{"name": "rag", "template": "R"}]})),
            None
        );
        assert_eq!(from_tokenizer_config(&json!({"chat_template": "  "})), None);
        assert_eq!(from_tokenizer_config(&json!({"bos_token": "<s>"})), None);
        let huge = "x".repeat(MAX_TEMPLATE_BYTES + 1);
        assert_eq!(from_tokenizer_config(&json!({"chat_template": huge})), None);
    }

    #[test]
    fn sidecar_is_next_to_the_gguf() {
        assert_eq!(
            sidecar_path(Path::new("/m/a-Q4_K_M.gguf")),
            Path::new("/m/a-Q4_K_M.gguf.chat_template.jinja")
        );
    }
}
