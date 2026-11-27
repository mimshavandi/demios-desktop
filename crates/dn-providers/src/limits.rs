//! How much a remote model reads at once (ADR-0020 §2.1), per model, never guessed.
//!
//! - **Anthropic, OpenAI:** their models APIs don't report it, so a small table of
//!   published sizes ([`published`]). It's kept current by hand (#352).
//! - **Gemini:** `GET models/{id}`, input plus output limit.
//! - **OpenAI-compatible servers:** the context the server has the model loaded with:
//!   - llama.cpp `llama-server`: `GET /props`;
//!   - LM Studio: `GET /api/v0/models/{id}`, `loaded_context_length`;
//!   - vLLM: `GET /v1/models`, `max_model_len`;
//!   - Ollama: `GET /api/ps` for a loaded model (before the first load it's unknown, #360).
//!
//!   A model's *maximum* isn't used: a server silently cuts what doesn't fit the context it
//!   loaded.
//!
//! A known size is cached per adapter and model. An unknown one is
//! [`ModelLimits::default`], and the runtime then asks the person to set it.

use crate::adapter::{Inner, ProviderKind, ServerFamily};
use dn_core::chat::{LimitSource, ModelLimits};
use dn_core::model::ModelRef;
use serde_json::Value;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(4);

/// Published context sizes, checked against each provider's model documentation on
/// 2026-11-27. An id matches its entry exactly, or with a date suffix (`-20250929`,
/// `-2025-04-14`) or `-latest`. A model that isn't here is unknown, not the nearest entry.
const PUBLISHED: &[(ProviderKind, &str, u32)] = &[
    // https://docs.anthropic.com/en/docs/about-claude/models/overview
    (ProviderKind::Anthropic, "claude-opus-4-1", 200_000),
    (ProviderKind::Anthropic, "claude-opus-4", 200_000),
    (ProviderKind::Anthropic, "claude-sonnet-4-5", 200_000),
    (ProviderKind::Anthropic, "claude-sonnet-4", 200_000),
    (ProviderKind::Anthropic, "claude-haiku-4-5", 200_000),
    (ProviderKind::Anthropic, "claude-3-7-sonnet", 200_000),
    (ProviderKind::Anthropic, "claude-3-5-sonnet", 200_000),
    (ProviderKind::Anthropic, "claude-3-5-haiku", 200_000),
    // https://platform.openai.com/docs/models
    (ProviderKind::OpenAi, "gpt-4.1", 1_047_576),
    (ProviderKind::OpenAi, "gpt-4.1-mini", 1_047_576),
    (ProviderKind::OpenAi, "gpt-4.1-nano", 1_047_576),
    (ProviderKind::OpenAi, "gpt-4o", 128_000),
    (ProviderKind::OpenAi, "gpt-4o-mini", 128_000),
    (ProviderKind::OpenAi, "o3", 200_000),
    (ProviderKind::OpenAi, "o3-mini", 200_000),
    (ProviderKind::OpenAi, "o4-mini", 200_000),
];

/// Whether `rest` (what follows an entry's id) is a date or `-latest`.
fn version_suffix(rest: &str) -> bool {
    let Some(s) = rest.strip_prefix('-') else {
        return rest.is_empty();
    };
    if s == "latest" {
        return true;
    }
    let digits = s.chars().filter(char::is_ascii_digit).count();
    s.chars().all(|c| c.is_ascii_digit() || c == '-') && digits == 8
}

/// The published size of `model_id` at `kind`, if it's in the table.
pub fn published(kind: ProviderKind, model_id: &str) -> Option<u32> {
    PUBLISHED
        .iter()
        .filter(|(k, id, _)| *k == kind && model_id.starts_with(id))
        .find(|(_, id, _)| version_suffix(&model_id[id.len()..]))
        .map(|(_, _, n)| *n)
}

/// What `inner` knows about `model`'s context, cached once known.
pub(crate) async fn resolve(inner: &Inner, model: &ModelRef) -> ModelLimits {
    let model_id = match model {
        ModelRef::Byok {
            provider_id,
            model_id,
        }
        | ModelRef::Remote {
            endpoint_id: provider_id,
            model_id,
        } if provider_id == inner.id() => model_id.as_str(),
        _ => return ModelLimits::default(),
    };
    if let Some(n) = inner.cached_limit(model_id) {
        return n;
    }
    let found = match inner.cfg.kind {
        ProviderKind::Anthropic | ProviderKind::OpenAi => published(inner.cfg.kind, model_id)
            .map(|n| ModelLimits::known(n, LimitSource::Published)),
        ProviderKind::Gemini => gemini(inner, model_id).await,
        ProviderKind::OpenAiCompatible => server(inner, model_id).await,
    };
    match found {
        Some(l) => {
            inner.cache_limit(model_id, l);
            l
        }
        None => ModelLimits::default(),
    }
}

fn provider(n: u64) -> Option<ModelLimits> {
    u32::try_from(n)
        .ok()
        .filter(|n| *n > 0)
        .map(|n| ModelLimits::known(n, LimitSource::Provider))
}

async fn gemini(inner: &Inner, model_id: &str) -> Option<ModelLimits> {
    let u = url(&inner.base, &["models", model_id])?;
    let rb = inner.client.get(u).header("x-goog-api-key", inner.key()?);
    let v = json(rb).await?;
    let input = v["inputTokenLimit"].as_u64()?;
    let output = v["outputTokenLimit"].as_u64().unwrap_or(0);
    provider(input + output)
}

async fn server(inner: &Inner, model_id: &str) -> Option<ModelLimits> {
    let root = inner
        .base
        .strip_suffix("/v1")
        .unwrap_or(&inner.base)
        .to_string();
    let get = |u: reqwest::Url| {
        let mut rb = inner.client.get(u);
        if let Some(k) = inner.key() {
            rb = rb.bearer_auth(k);
        }
        rb
    };
    let n = match inner.cfg.server_family {
        ServerFamily::LlamaCpp => {
            let v = json(get(url(&root, &["props"])?)).await?;
            v["default_generation_settings"]["n_ctx"]
                .as_u64()
                .or_else(|| v["n_ctx"].as_u64())?
        }
        ServerFamily::LmStudio => {
            let v = json(get(url(&root, &["api", "v0", "models", model_id])?)).await?;
            v["loaded_context_length"].as_u64()?
        }
        ServerFamily::Vllm => {
            let v = json(get(url(&inner.base, &["models"])?)).await?;
            v["data"]
                .as_array()?
                .iter()
                .find(|m| m["id"].as_str() == Some(model_id))?["max_model_len"]
                .as_u64()?
        }
        ServerFamily::Ollama => {
            let v = json(get(url(&root, &["api", "ps"])?)).await?;
            v["models"].as_array()?.iter().find(|m| {
                m["name"].as_str() == Some(model_id) || m["model"].as_str() == Some(model_id)
            })?["context_length"]
                .as_u64()?
        }
        ServerFamily::Unknown => return None,
    };
    provider(n)
}

/// `base` with `segments` appended, each escaped as one path segment (a model id can hold
/// `/`, `?` or `#`).
fn url(base: &str, segments: &[&str]) -> Option<reqwest::Url> {
    if segments
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return None;
    }
    let mut u = reqwest::Url::parse(base).ok()?;
    u.path_segments_mut().ok()?.pop_if_empty().extend(segments);
    Some(u)
}

async fn json(rb: reqwest::RequestBuilder) -> Option<Value> {
    let r = rb.timeout(TIMEOUT).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let bytes = tokio::time::timeout(TIMEOUT, r.bytes()).await.ok()?.ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_id_is_one_escaped_path_segment() {
        let u = url(
            "http://127.0.0.1:1234/",
            &["api", "v0", "models", "a/b?c#d"],
        )
        .unwrap();
        assert_eq!(
            u.as_str(),
            "http://127.0.0.1:1234/api/v0/models/a%2Fb%3Fc%23d"
        );
        let u = url("https://x.test/v1beta", &["models", "gemini-2.5-pro"]).unwrap();
        assert_eq!(u.as_str(), "https://x.test/v1beta/models/gemini-2.5-pro");
        for bad in ["", ".", ".."] {
            assert!(
                url("https://x.test/v1beta", &["models", bad]).is_none(),
                "{bad:?}"
            );
        }
        let u = url("https://x.test/v1beta", &["models", "gemini-2.5-pro"]).unwrap();
        assert_eq!(u.as_str(), "https://x.test/v1beta/models/gemini-2.5-pro");
    }

    #[test]
    fn published_sizes_match_exact_ids_and_dated_versions_only() {
        let a = ProviderKind::Anthropic;
        assert_eq!(published(a, "claude-sonnet-4-5"), Some(200_000));
        assert_eq!(published(a, "claude-sonnet-4-5-20250929"), Some(200_000));
        assert_eq!(published(a, "claude-3-5-sonnet-latest"), Some(200_000));
        assert_eq!(published(a, "claude-sonnet-4-20250514"), Some(200_000));
        let o = ProviderKind::OpenAi;
        assert_eq!(published(o, "gpt-4.1-2025-04-14"), Some(1_047_576));
        assert_eq!(published(o, "gpt-4.1-mini"), Some(1_047_576));
        assert_eq!(published(o, "gpt-4o-mini"), Some(128_000));
        // Not a version of an entry: unknown, never the nearest match.
        for id in [
            "gpt-4o-audio-preview",
            "gpt-5-chat",
            "claude-sonnet-4-5-x",
            "o3-pro",
            "gpt-4.1-mini-x",
        ] {
            assert_eq!(published(o, id).or(published(a, id)), None, "{id}");
        }
        // Another provider's table doesn't apply.
        assert_eq!(published(ProviderKind::OpenAiCompatible, "gpt-4o"), None);
    }
}
