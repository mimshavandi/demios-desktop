//! How much a remote model reads at once (ADR-0020 §2.1), per model, never guessed.
//!
//! - **Anthropic, OpenAI:** their models APIs don't report it, so a small table of
//!   published sizes ([`PUBLISHED`]), each with its source and the date it was checked. It's
//!   kept current by hand; a release check flags entries older than [`MAX_AGE_DAYS`] (#352).
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

/// One published context size: where it's published and when that page was last checked.
pub struct Published {
    pub kind: ProviderKind,
    /// The model id; it also matches the same id with a date suffix or `-latest`.
    pub id: &'static str,
    pub tokens: u32,
    /// The provider's page that states the size (https).
    pub source: &'static str,
    /// When the size was last checked against `source` (YYYY-MM-DD).
    pub checked: &'static str,
}

/// Entries older than this (in days) are flagged by the release check (#352).
pub const MAX_AGE_DAYS: i64 = 183;

/// Published context sizes. An id matches its entry exactly, or with a date suffix
/// (`-20250929`, `-2025-04-14`) or `-latest`. A model that isn't here is unknown, never the
/// nearest entry.
///
/// To update (#352, docs/releasing.md): open each entry's `source`, correct `tokens` if the
/// page says otherwise, and set `checked` to today; add a model only with the page that
/// states its size, never by analogy with a sibling. Then run the release check:
/// `cargo test -p dn-providers --lib published_sizes_are_recent -- --ignored`.
pub const PUBLISHED: &[Published] = &[
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-fable-5-2",
        tokens: 1_000_000,
        source: "https://platform.claude.com/docs/en/about-claude/models/overview",
        checked: "2026-11-28",
    },
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-opus-5-5",
        tokens: 1_000_000,
        source: "https://platform.claude.com/docs/en/about-claude/models/overview",
        checked: "2026-11-28",
    },
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-sonnet-5-5",
        tokens: 1_000_000,
        source: "https://platform.claude.com/docs/en/about-claude/models/overview",
        checked: "2026-11-28",
    },
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-haiku-5-5",
        tokens: 1_000_000,
        source: "https://platform.claude.com/docs/en/about-claude/models/overview",
        checked: "2026-11-28",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "gpt-6.2-astra",
        tokens: 1_050_000,
        source: "https://developers.openai.com/api/docs/models/gpt-6.2-astra",
        checked: "2026-11-28",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "gpt-6.2-sol",
        tokens: 1_050_000,
        source: "https://developers.openai.com/api/docs/models/gpt-6.2-sol",
        checked: "2026-11-28",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "gpt-6.1-terra",
        tokens: 1_050_000,
        source: "https://developers.openai.com/api/docs/models/gpt-6.1-terra",
        checked: "2026-11-28",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "gpt-6-luna",
        tokens: 1_050_000,
        source: "https://developers.openai.com/api/docs/models/gpt-6-luna",
        checked: "2026-11-28",
    },
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-opus-4-1",
        tokens: 200_000,
        source: "https://docs.anthropic.com/en/docs/about-claude/models/overview",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-opus-4",
        tokens: 200_000,
        source: "https://docs.anthropic.com/en/docs/about-claude/models/overview",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-sonnet-4-5",
        tokens: 200_000,
        source: "https://docs.anthropic.com/en/docs/about-claude/models/overview",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-sonnet-4",
        tokens: 200_000,
        source: "https://docs.anthropic.com/en/docs/about-claude/models/overview",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-haiku-4-5",
        tokens: 200_000,
        source: "https://docs.anthropic.com/en/docs/about-claude/models/overview",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-3-7-sonnet",
        tokens: 200_000,
        source: "https://docs.anthropic.com/en/docs/about-claude/models/overview",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-3-5-sonnet",
        tokens: 200_000,
        source: "https://docs.anthropic.com/en/docs/about-claude/models/overview",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::Anthropic,
        id: "claude-3-5-haiku",
        tokens: 200_000,
        source: "https://docs.anthropic.com/en/docs/about-claude/models/overview",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "gpt-4.1",
        tokens: 1_047_576,
        source: "https://platform.openai.com/docs/models",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "gpt-4.1-mini",
        tokens: 1_047_576,
        source: "https://platform.openai.com/docs/models",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "gpt-4.1-nano",
        tokens: 1_047_576,
        source: "https://platform.openai.com/docs/models",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "gpt-4o",
        tokens: 128_000,
        source: "https://platform.openai.com/docs/models",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "gpt-4o-mini",
        tokens: 128_000,
        source: "https://platform.openai.com/docs/models",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "o3",
        tokens: 200_000,
        source: "https://platform.openai.com/docs/models",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "o3-mini",
        tokens: 200_000,
        source: "https://platform.openai.com/docs/models",
        checked: "2026-11-27",
    },
    Published {
        kind: ProviderKind::OpenAi,
        id: "o4-mini",
        tokens: 200_000,
        source: "https://platform.openai.com/docs/models",
        checked: "2026-11-27",
    },
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
        .filter(|p| p.kind == kind && model_id.starts_with(p.id))
        .find(|p| version_suffix(&model_id[p.id.len()..]))
        .map(|p| p.tokens)
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
        assert_eq!(published(a, "claude-opus-5-5"), Some(1_000_000));
        assert_eq!(published(o, "gpt-6.2-sol"), Some(1_050_000));
        assert_eq!(published(o, "gpt-6-luna"), Some(1_050_000));
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

    /// Days since 1970-01-01 for a `YYYY-MM-DD` date (proleptic Gregorian), or `None`.
    fn days(date: &str) -> Option<i64> {
        let mut it = date.split('-');
        let (y, m, d): (i64, i64, i64) = (
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
        );
        if it.next().is_some()
            || date.len() != 10
            || !(1..=12).contains(&m)
            || !(1..=31).contains(&d)
        {
            return None;
        }
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Some(era * 146_097 + doe - 719_468)
    }

    fn today() -> i64 {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after 1970")
            .as_secs();
        (secs / 86_400) as i64
    }

    #[test]
    fn every_published_size_names_its_source_and_a_real_check_date() {
        assert_eq!(days("1970-01-01"), Some(0));
        assert_eq!(days("2026-11-27"), Some(20_784));
        assert_eq!(days("2026-13-01"), None);
        assert_eq!(days("26-11-27"), None);
        let mut seen = std::collections::HashSet::new();
        for p in PUBLISHED {
            assert!(seen.insert((p.kind, p.id)), "{} appears twice", p.id);
            assert!(
                p.source.starts_with("https://"),
                "{}: source must be https",
                p.id
            );
            assert!(p.tokens > 0, "{}", p.id);
            let d = days(p.checked).unwrap_or_else(|| panic!("{}: checked {:?}", p.id, p.checked));
            assert!(d <= today(), "{}: checked in the future", p.id);
        }
    }

    /// The release check (docs/releasing.md): fails, naming them, when entries were last
    /// checked more than [`MAX_AGE_DAYS`] ago. Ignored in normal runs so CI doesn't start
    /// failing on a date.
    #[test]
    #[ignore = "release check: cargo test -p dn-providers --lib published_sizes_are_recent -- --ignored"]
    fn published_sizes_are_recent() {
        let now = today();
        let old: Vec<String> = PUBLISHED
            .iter()
            .filter(|p| days(p.checked).is_none_or(|d| now - d > MAX_AGE_DAYS))
            .map(|p| format!("{} (checked {}, {})", p.id, p.checked, p.source))
            .collect();
        assert!(
            old.is_empty(),
            "check these context sizes again (older than {MAX_AGE_DAYS} days):\n{}",
            old.join("\n")
        );
    }
}
