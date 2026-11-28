//! Curated model catalogue (`catalog.json`, compiled in). URLs are pinned to a Hugging Face
//! commit, and every file has its size and SHA-256 so downloads are verified.

use dn_core::model::Capability;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub repo: String,
    /// Hugging Face commit the file is pinned to.
    pub revision: String,
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub quant: String,
    pub params_b: f64,
    pub capabilities: Vec<Capability>,
    /// Licence id from the model card (users accept it by downloading).
    pub license: String,
    #[serde(default)]
    pub notes: String,
    /// Chat models: whether the chat template handles tool calls, as read from the GGUF header
    /// (T-INF-26; checked by `every_chat_model_has_a_chat_template`). `None` for embedding
    /// models and models outside the catalogue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_support: Option<crate::kind::ToolSupport>,
    /// One of the models we recommend (#219): shown with a "Recommended" chip in the model
    /// list and in Hugging Face search results for its repo, on the machines
    /// [`Catalog::recommended_for`] picks it for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<Recommendation>,
    /// SHA-256 of the chat template in the pinned file, the original the model check
    /// compares other copies of this model with (#421). Only the hash is kept here; the
    /// template itself is the model publisher's, read from the file when it's needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_sha256: Option<String>,
    /// Fingerprint of the pinned file's tokenizer ([`crate::gguf::Layout::tokenizer_sha256`]).
    /// A file from this model's repo with another tokenizer is Blocked (#421).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenizer_sha256: Option<String>,
}

/// For which machines a catalogue model is recommended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Recommendation {
    /// `true`: on every machine (the search model). `false` is the same as no flag.
    Always(bool),
    /// `{"memory_gb": 8}`: on machines with at least this much memory, unless a pick for a
    /// bigger tier applies too (one chat pick per memory size).
    ForMemory { memory_gb: u32 },
}

impl CatalogEntry {
    /// `file` is a plain file name (no `/`, `\\`, `..` or drive prefix), so a pull can't
    /// write outside the models directory.
    pub fn has_plain_file_name(&self) -> bool {
        let f = &self.file;
        !f.is_empty() && !f.contains(['/', '\\', ':']) && f != "." && !f.contains("..")
    }

    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.revision, self.file
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub version: u32,
    pub models: Vec<CatalogEntry>,
}

impl Catalog {
    /// The built-in catalogue.
    pub fn builtin() -> Self {
        serde_json::from_str(include_str!("../catalog.json")).expect("catalog.json is valid")
    }

    pub fn get(&self, id: &str) -> Option<&CatalogEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    /// The entries recommended on a machine with `total_ram` bytes of memory: every
    /// `Always` pick, and the memory pick for the largest tier the machine reaches. Memory is
    /// rounded to whole GiB first, so an "8 GB" machine that reports a little less (memory the
    /// firmware keeps) still counts as 8 GB. Below the smallest tier there is no memory pick.
    pub fn recommended_for(&self, total_ram: u64) -> Vec<&CatalogEntry> {
        let gb = (total_ram + crate::fit::GIB / 2) / crate::fit::GIB;
        let tier = self
            .models
            .iter()
            .filter_map(|m| match m.recommended {
                Some(Recommendation::ForMemory { memory_gb }) if u64::from(memory_gb) <= gb => {
                    Some(memory_gb)
                }
                _ => None,
            })
            .max();
        self.models
            .iter()
            .filter(|m| match m.recommended {
                Some(Recommendation::Always(on)) => on,
                Some(Recommendation::ForMemory { memory_gb }) => Some(memory_gb) == tier,
                None => false,
            })
            .collect()
    }

    /// Whether an entry recommended on this machine (see [`Self::recommended_for`]) comes
    /// from Hugging Face repo `repo` (any file when `file` is `None`). Repo ids compare
    /// without case, as the Hub does.
    pub fn recommends(&self, repo: &str, file: Option<&str>, total_ram: u64) -> bool {
        self.recommended_for(total_ram)
            .iter()
            .any(|m| m.repo.eq_ignore_ascii_case(repo) && file.is_none_or(|f| f == m.file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_model_has_its_check_references() {
        let hex = |s: &str| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit());
        for m in Catalog::builtin().models {
            assert!(
                m.tokenizer_sha256.as_deref().is_some_and(hex),
                "{}: tokenizer_sha256 (run print_check_references)",
                m.id
            );
            if m.capabilities.contains(&Capability::Chat) {
                assert!(
                    m.template_sha256.as_deref().is_some_and(hex),
                    "{}: template_sha256 (run print_check_references)",
                    m.id
                );
            }
        }
    }

    #[test]
    fn builtin_catalog_is_well_formed() {
        let c = Catalog::builtin();
        assert!(c.models.len() >= 10);
        let mut ids = HashSet::new();
        for m in &c.models {
            // Chat models say whether they make tool calls (T-INF-26); others don't.
            assert_eq!(
                m.tool_support.is_some(),
                m.capabilities.contains(&Capability::Chat),
                "{}",
                m.id
            );
            assert!(ids.insert(&m.id), "duplicate id {}", m.id);
            assert_eq!(m.sha256.len(), 64, "{}", m.id);
            assert!(m.sha256.chars().all(|c| c.is_ascii_hexdigit()), "{}", m.id);
            assert_eq!(m.revision.len(), 40, "{}", m.id);
            assert!(m.file.ends_with(".gguf"), "{}", m.id);
            assert!(m.has_plain_file_name(), "{}", m.id);
            // The smallest entry is an 84 MB embedding model.
            assert!(m.size > 50_000_000, "{}", m.id);
            assert!(!m.capabilities.is_empty(), "{}", m.id);
        }
        assert!(c.get("qwen2.5-0.5b-instruct-q4_k_m").is_some());
        assert!(
            c.models
                .iter()
                .any(|m| m.capabilities.contains(&Capability::Embedding))
        );
    }

    #[test]
    fn nomic_embed_v15_is_a_tiny_embedding_model() {
        let c = Catalog::builtin();
        let m = c
            .get("nomic-embed-text-v1.5-q4_k_m")
            .expect("in the catalogue");
        // Never a chat model: the resolver and the slot pickers match on capabilities.
        assert_eq!(m.capabilities, [Capability::Embedding]);
        assert_eq!(m.license, "apache-2.0");
        assert_eq!(
            m.url(),
            "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5-GGUF/resolve/0188c9bf409793f810680a5a431e7b899c46104c/nomic-embed-text-v1.5.Q4_K_M.gguf"
        );
        // Ready with lots of headroom on the smallest machine we target.
        let fp = crate::ModelFootprint {
            weight_bytes: m.size,
            expert_bytes: 0,
            n_params: (m.params_b * 1e9) as u64,
            n_layer: 12,
            n_ctx_train: 2048,
            n_vocab: 30_522,
            n_embd: 768,
            kv_elems_per_token: 12 * 12 * 128,
            swa_kv_elems_per_token: 0,
            sliding_window: 0,
            quant: Some(m.quant.clone()),
        };
        let r = crate::fit(&fp, &crate::Machine::unified(8 * crate::fit::GIB));
        assert_eq!(r.status, dn_core::model::FitStatus::Ready);
        assert!(r.recommended.need_bytes < crate::fit::GIB);
        // Its 2048-token training context is not a reason to suggest a smaller quant.
        assert_eq!(r.suggested_quant, None);
    }

    /// Every chat model in the catalogue carries its own chat template in the GGUF, so it
    /// renders with its own template (T-INF-23). Reads each file's header from the Hub with
    /// range requests (a few MB per model).
    /// The model check's references (#421): each pinned file's chat template and tokenizer
    /// fingerprint, read from its header on the Hub. Prints them as JSON, and with
    /// `DN_TEMPLATES_OUT=<dir>` saves each template as `<sha256>.jinja`.
    #[tokio::test]
    #[ignore = "network: reads GGUF headers from huggingface.co"]
    async fn print_check_references() {
        use sha2::{Digest, Sha256};
        let hub = crate::hub::HubClient::new(crate::hub::HubConfig::from_env()).unwrap();
        for m in Catalog::builtin().models {
            let file = crate::hub::RepoFile {
                path: m.file.clone(),
                size: m.size,
                sha256: Some(m.sha256.clone()),
            };
            let g = hub.gguf_header(&m.repo, &m.revision, &file).await.unwrap();
            let tpl = g.chat_template().map(|t| {
                let h: String = Sha256::digest(t.as_bytes())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                if let Some(dir) = std::env::var_os("DN_TEMPLATES_OUT") {
                    std::fs::write(std::path::Path::new(&dir).join(format!("{h}.jinja")), t)
                        .unwrap();
                }
                h
            });
            println!(
                "{}",
                serde_json::json!({
                    "id": m.id,
                    "template_sha256": tpl,
                    "tokenizer_sha256": g.layout.tokenizer_sha256,
                })
            );
        }
    }

    #[tokio::test]
    #[ignore = "network: reads GGUF headers from huggingface.co"]
    async fn every_chat_model_has_a_chat_template() {
        let hub = crate::hub::HubClient::new(crate::hub::HubConfig::from_env()).unwrap();
        let mut missing = vec![];
        for m in Catalog::builtin().models {
            if !m.capabilities.contains(&Capability::Chat) {
                continue;
            }
            let file = crate::hub::RepoFile {
                path: m.file.clone(),
                size: m.size,
                sha256: Some(m.sha256.clone()),
            };
            let g = hub.gguf_header(&m.repo, &m.revision, &file).await.unwrap();
            // The catalogue's tool_support is what the header says (T-INF-26).
            let traits = crate::kind::from_gguf(&g);
            assert_eq!(
                Some(traits.tool_support),
                m.tool_support,
                "{}: catalogue tool_support differs from the header",
                m.id
            );
            match g.chat_template() {
                Some(t) => {
                    eprintln!("ok  {} ({} bytes)", m.id, t.len());
                    // DN_TEMPLATES_OUT=<dir> saves them, for dn-inference's
                    // `real_templates_render` test (DN_TEST_TEMPLATES).
                    if let Some(dir) = std::env::var_os("DN_TEMPLATES_OUT") {
                        let p = std::path::Path::new(&dir).join(format!("{}.jinja", m.id));
                        std::fs::write(p, t).unwrap();
                    }
                }
                None => missing.push(m.id),
            }
        }
        assert!(missing.is_empty(), "no chat template in: {missing:?}");
    }

    /// The recommended models (#219): the search model on every machine, and the chat
    /// default the notes already name for 8 GB and 16 GB machines. The chip comes from this
    /// data, not from the UI.
    #[test]
    fn the_recommended_models_follow_the_machine_memory() {
        let c = Catalog::builtin();
        let gb = |n: u64| n * crate::fit::GIB;
        let ids = |ram: u64| -> Vec<&str> {
            c.recommended_for(ram)
                .iter()
                .map(|m| m.id.as_str())
                .collect()
        };
        let search = "nomic-embed-text-v2-moe-q5_k_m";
        // Below 8 GB: the search model only, no chat pick.
        assert_eq!(ids(gb(4)), [search]);
        // 8 GB (also as reported with some memory kept by the firmware), and up to 15 GB.
        let eight = ["llama-3.2-3b-instruct-q4_k_m", search];
        assert_eq!(ids(gb(8)), eight);
        assert_eq!(ids(gb(8) - gb(1) / 4), eight);
        assert_eq!(ids(gb(12)), eight);
        // 16 GB and more: the 16 GB pick only, never two chat picks.
        assert_eq!(ids(gb(16)), ["qwen3-8b-q4_k_m", search]);
        assert_eq!(ids(gb(64)), ["qwen3-8b-q4_k_m", search]);
        // Each memory pick is the model its notes call the default for that size.
        for m in &c.models {
            if let Some(Recommendation::ForMemory { memory_gb }) = m.recommended {
                assert!(m.capabilities.contains(&Capability::Chat), "{}", m.id);
                assert!(m.notes.contains(&format!("{memory_gb} GB")), "{}", m.id);
            }
        }

        assert!(c.recommends("nomic-ai/nomic-embed-text-v2-moe-GGUF", None, gb(4)));
        assert!(c.recommends(
            "NOMIC-AI/nomic-embed-text-v2-moe-gguf",
            Some("nomic-embed-text-v2-moe.Q5_K_M.gguf"),
            gb(4)
        ));
        // The same repo's other file isn't the recommended one.
        assert!(!c.recommends(
            "nomic-ai/nomic-embed-text-v2-moe-GGUF",
            Some("nomic-embed-text-v2-moe.Q8_0.gguf"),
            gb(4)
        ));
        assert!(c.recommends("Qwen/Qwen3-8B-GGUF", None, gb(16)));
        assert!(!c.recommends("Qwen/Qwen3-8B-GGUF", None, gb(8)));
        assert!(!c.recommends("Qwen/Qwen2.5-0.5B-Instruct-GGUF", None, gb(16)));
    }

    #[test]
    fn path_like_file_names_rejected() {
        let mut m = Catalog::builtin().models[0].clone();
        for bad in ["../x.gguf", "a/b.gguf", "a\\b.gguf", "C:x.gguf", "..", ""] {
            m.file = bad.into();
            assert!(!m.has_plain_file_name(), "{bad}");
        }
    }

    #[test]
    fn url_is_pinned() {
        let c = Catalog::builtin();
        let m = c.get("qwen2.5-0.5b-instruct-q4_k_m").unwrap();
        assert_eq!(
            m.url(),
            "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/9217f5db79a29953eb74d5343926648285ec7e67/qwen2.5-0.5b-instruct-q4_k_m.gguf"
        );
    }
}
