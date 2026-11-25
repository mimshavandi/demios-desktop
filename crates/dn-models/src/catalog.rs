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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn builtin_catalog_is_well_formed() {
        let c = Catalog::builtin();
        assert!(c.models.len() >= 10);
        let mut ids = HashSet::new();
        for m in &c.models {
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
