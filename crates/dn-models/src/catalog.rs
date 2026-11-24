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
            assert!(m.size > 100_000_000, "{}", m.id);
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
    fn url_is_pinned() {
        let c = Catalog::builtin();
        let m = c.get("qwen2.5-0.5b-instruct-q4_k_m").unwrap();
        assert_eq!(
            m.url(),
            "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/9217f5db79a29953eb74d5343926648285ec7e67/qwen2.5-0.5b-instruct-q4_k_m.gguf"
        );
    }
}
