//! What a model is for (T-INF-26): chat (with or without tool calls), embeddings, or a base
//! model that can't chat. Read from the GGUF header, a local file's or a Hugging Face file's
//! (range requests, no download), so the UI can label models before they are added.
//!
//! - **Embedding**: the model declares a pooling type, or is a BERT-family architecture
//!   ([`crate::template::is_embedding`]).
//! - **Chat**: it has a chat template (in the GGUF, or stored next to it from the repo's
//!   `tokenizer_config.json`). Every local chat renders with that template (T-INF-23).
//! - **Base**: neither; a text model without a chat template, which Demios can't chat with.
//!
//! Tool calls ([`ToolSupport`], the same values as the app's `ModelRegistry::tool_support`):
//! `Supported` when the chat template handles a `tools` list or writes a tool-call marker the
//! model was trained on (`<tool_call>`, `[TOOL_CALLS]`, `<|python_tag|>`, …); `Unknown` for
//! other chat templates (the model may still follow the tool prompt, a probe can tell);
//! `Unsupported` for embedding and base models. A probe's answer is stored as `checked`.

use crate::gguf::Gguf;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Chat,
    Embedding,
    /// A text model without a chat template: not usable for chat in Demios.
    Base,
}

/// Whether a model makes tool calls. Same values (and serialised names) as the desktop's
/// `ToolSupport` for providers and servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSupport {
    Supported,
    Unsupported,
    Unknown,
}

/// A model's kind and tool support, and where that came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTraits {
    pub kind: ModelKind,
    pub tool_support: ToolSupport,
    /// How it was decided: from the GGUF header, from Hub metadata only (a hint, before the
    /// header was read), or by a tool-call probe.
    pub source: TraitsSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraitsSource {
    Header,
    Hint,
    Checked,
}

impl ModelTraits {
    /// The label for the UI: "Chat · Tools", "Chat", "Embedding" or "Base model".
    pub fn label(&self) -> &'static str {
        match (self.kind, self.tool_support) {
            (ModelKind::Chat, ToolSupport::Supported) => "Chat · Tools",
            (ModelKind::Chat, _) => "Chat",
            (ModelKind::Embedding, _) => "Embedding",
            (ModelKind::Base, _) => "Base model",
        }
    }

    /// The result of a one-time tool-call probe for a chat model whose template didn't say.
    pub fn checked(self, makes_tool_calls: bool) -> Self {
        if self.kind != ModelKind::Chat {
            return self;
        }
        Self {
            tool_support: if makes_tool_calls {
                ToolSupport::Supported
            } else {
                ToolSupport::Unsupported
            },
            source: TraitsSource::Checked,
            ..self
        }
    }
}

/// Text in a chat template that shows the model was trained for tool calls: it renders a
/// `tools` list, tool results or tool calls, or writes one of the common call markers.
const TOOL_MARKERS: &[&str] = &[
    "<tool_call>",
    "</tool_call>",
    "[TOOL_CALLS]",
    "[AVAILABLE_TOOLS]",
    "<|python_tag|>",
    "<|tool_call",
    "<|tool▁calls",
    "<start_function_call>",
    "<|start|>functions",
    "tool_calls",
];

fn template_has_tools(t: &str) -> bool {
    // `tools` used as a variable: `{% if tools %}`, `tools is defined`, `for tool in tools`,
    // `tools | tojson`. A bare word in a sentence of the template doesn't count.
    let uses_tools_var = [
        "if tools", "tools is", "in tools", "tools |", "tools|", "tools[",
    ]
    .iter()
    .any(|p| t.contains(p));
    uses_tools_var || TOOL_MARKERS.iter().any(|m| t.contains(m))
}

/// Traits from a chat template's text (`None`: no template), for a non-embedding model.
fn from_template(template: Option<&str>) -> ModelTraits {
    match template {
        None => ModelTraits {
            kind: ModelKind::Base,
            tool_support: ToolSupport::Unsupported,
            source: TraitsSource::Header,
        },
        Some(t) => ModelTraits {
            kind: ModelKind::Chat,
            tool_support: if template_has_tools(t) {
                ToolSupport::Supported
            } else {
                ToolSupport::Unknown
            },
            source: TraitsSource::Header,
        },
    }
}

/// Traits from a GGUF header alone (a Hugging Face file before download).
pub fn from_gguf(g: &Gguf) -> ModelTraits {
    if crate::template::is_embedding(g) {
        return ModelTraits {
            kind: ModelKind::Embedding,
            tool_support: ToolSupport::Unsupported,
            source: TraitsSource::Header,
        };
    }
    from_template(g.chat_template())
}

/// Traits of the GGUF at `path` (already parsed as `g`), counting a chat template stored next
/// to it ([`crate::template::sidecar_path`]).
pub fn of_file(g: &Gguf, path: &Path) -> ModelTraits {
    if crate::template::is_embedding(g) || g.chat_template().is_some() {
        return from_gguf(g);
    }
    let side = std::fs::read_to_string(crate::template::sidecar_path(path)).ok();
    from_template(side.as_deref().filter(|s| !s.trim().is_empty()))
}

/// A hint from Hub metadata, before any header is read (search results). `None` when the
/// metadata doesn't say.
pub fn hint(pipeline_tag: Option<&str>, tags: &[String]) -> Option<ModelTraits> {
    let has = |t: &str| tags.iter().any(|x| x.eq_ignore_ascii_case(t));
    let kind = match pipeline_tag {
        Some("feature-extraction" | "sentence-similarity") => ModelKind::Embedding,
        _ if has("sentence-transformers") || has("embeddings") => ModelKind::Embedding,
        Some("text-generation" | "image-text-to-text" | "conversational") => ModelKind::Chat,
        _ if has("conversational") => ModelKind::Chat,
        _ => return None,
    };
    let tool_support = match kind {
        ModelKind::Chat if has("tool-use") || has("function-calling") || has("tools") => {
            ToolSupport::Supported
        }
        ModelKind::Chat => ToolSupport::Unknown,
        _ => ToolSupport::Unsupported,
    };
    Some(ModelTraits {
        kind,
        tool_support,
        source: TraitsSource::Hint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gguf::write::{V, gguf};

    fn header(kv: Vec<(&str, V<'_>)>) -> Gguf {
        let bytes = gguf(&kv, &[("blk.0.attn_q.weight", &[8, 8], 0, 64)]);
        Gguf::from_prefix(&bytes, bytes.len() as u64).unwrap()
    }

    #[test]
    fn kinds_from_the_header() {
        let qwen3 = header(vec![
            ("general.architecture", V::Str("qwen3")),
            (
                "tokenizer.chat_template",
                V::Str(
                    "{% if tools %}<tools>{% for t in tools %}{{ t | tojson }}{% endfor %}</tools>{% endif %}<tool_call>",
                ),
            ),
        ]);
        let t = from_gguf(&qwen3);
        assert_eq!(
            (t.kind, t.tool_support),
            (ModelKind::Chat, ToolSupport::Supported)
        );
        assert_eq!(t.label(), "Chat · Tools");

        let plain = header(vec![
            ("general.architecture", V::Str("llama")),
            (
                "tokenizer.chat_template",
                V::Str("{% for m in messages %}<|{{ m.role }}|>{{ m.content }}{% endfor %}"),
            ),
        ]);
        let t = from_gguf(&plain);
        assert_eq!(
            (t.kind, t.tool_support),
            (ModelKind::Chat, ToolSupport::Unknown)
        );
        assert_eq!(t.label(), "Chat");

        let emb = header(vec![
            ("general.architecture", V::Str("nomic-bert")),
            ("nomic-bert.pooling_type", V::U32(1)),
        ]);
        assert_eq!(from_gguf(&emb).label(), "Embedding");

        let base = header(vec![("general.architecture", V::Str("llama"))]);
        let t = from_gguf(&base);
        assert_eq!((t.kind, t.label()), (ModelKind::Base, "Base model"));
    }

    #[test]
    fn a_template_next_to_the_file_makes_it_a_chat_model() {
        let dir = std::env::temp_dir().join(format!("dn-kind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("m.gguf");
        let base = header(vec![("general.architecture", V::Str("llama"))]);
        assert_eq!(of_file(&base, &p).kind, ModelKind::Base);
        std::fs::write(
            crate::template::sidecar_path(&p),
            "{% for m in messages %}{{ m.content }}{% endfor %}",
        )
        .unwrap();
        assert_eq!(of_file(&base, &p).kind, ModelKind::Chat);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tool_markers_of_common_families() {
        for t in [
            "{{- '<|python_tag|>' + tool_call.name }}", // Llama 3.1
            "[AVAILABLE_TOOLS] {{ tools }}[/AVAILABLE_TOOLS]", // Mistral
            "{%- if message.tool_calls %}",             // many
            "{% for tool in tools %}",
        ] {
            assert!(template_has_tools(t), "{t}");
        }
        // The word in prose is not a tool-aware template.
        assert!(!template_has_tools(
            "You are a helpful assistant with many tools at hand."
        ));
    }

    #[test]
    fn hints_from_hub_metadata() {
        let tags = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            hint(Some("feature-extraction"), &tags(&["gguf"]))
                .unwrap()
                .kind,
            ModelKind::Embedding
        );
        let chat = hint(
            Some("text-generation"),
            &tags(&["gguf", "function-calling"]),
        )
        .unwrap();
        assert_eq!(
            (chat.label(), chat.source),
            ("Chat · Tools", TraitsSource::Hint)
        );
        assert_eq!(hint(None, &tags(&["gguf"])), None);
    }

    #[test]
    fn a_probe_decides_unknown() {
        let unknown = from_template(Some("{{ messages }}"));
        assert_eq!(unknown.checked(true).label(), "Chat · Tools");
        let no = unknown.checked(false);
        assert_eq!(
            (no.tool_support, no.source),
            (ToolSupport::Unsupported, TraitsSource::Checked)
        );
        // An embedding model isn't changed by a probe.
        let emb = ModelTraits {
            kind: ModelKind::Embedding,
            tool_support: ToolSupport::Unsupported,
            source: TraitsSource::Header,
        };
        assert_eq!(emb.checked(true), emb);
    }
}

#[cfg(test)]
mod real_templates {
    /// Prints the label every `*.jinja` in `DN_TEST_TEMPLATES` gets (see dn-models'
    /// `every_chat_model_has_a_chat_template`, which saves the catalogue's).
    #[test]
    #[ignore = "needs DN_TEST_TEMPLATES"]
    fn label_real_templates() {
        let dir = std::env::var("DN_TEST_TEMPLATES").expect("set DN_TEST_TEMPLATES");
        let mut rows: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jinja"))
            .map(|p| {
                let t = std::fs::read_to_string(&p).unwrap();
                let label = super::from_template(Some(&t)).label();
                (p.file_stem().unwrap().to_string_lossy().into_owned(), label)
            })
            .collect();
        rows.sort();
        for (name, label) in rows {
            eprintln!("{label:14} {name}");
        }
    }
}
