//! Model references, capabilities and slots (ADR-0003, ADR-0005).
//! Ported concept: Demios `model_source_refs.py` / `model_registry_models.py`.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// Where a model runs. String form:
/// `local:<model_id>` · `byok:<provider_id>:<model_id>` · `remote:<endpoint_id>:<model_id>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ModelRef {
    /// Runs in-process via `dn-inference`.
    Local { model_id: String },
    /// User's own key with a hosted provider (OpenAI, Anthropic, Gemini, ...).
    Byok {
        provider_id: String,
        model_id: String,
    },
    /// Any OpenAI-compatible endpoint (Ollama, LM Studio, vLLM, FreeToken, team box).
    Remote {
        endpoint_id: String,
        model_id: String,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "invalid model ref `{0}`: expected local:<id>, byok:<provider>:<id> or remote:<endpoint>:<id>"
)]
pub struct ModelRefParseError(pub String);

impl FromStr for ModelRef {
    type Err = ModelRefParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ModelRefParseError(s.to_string());
        let (scheme, rest) = s.split_once(':').ok_or_else(err)?;
        let two = |r: &str| -> Result<(String, String), ModelRefParseError> {
            let (a, b) = r.split_once(':').ok_or_else(err)?;
            if a.is_empty() || b.is_empty() {
                return Err(err());
            }
            Ok((a.to_string(), b.to_string()))
        };
        match scheme {
            "local" if !rest.is_empty() => Ok(Self::Local {
                model_id: rest.to_string(),
            }),
            "byok" => two(rest).map(|(provider_id, model_id)| Self::Byok {
                provider_id,
                model_id,
            }),
            "remote" => two(rest).map(|(endpoint_id, model_id)| Self::Remote {
                endpoint_id,
                model_id,
            }),
            _ => Err(err()),
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local { model_id } => write!(f, "local:{model_id}"),
            Self::Byok {
                provider_id,
                model_id,
            } => write!(f, "byok:{provider_id}:{model_id}"),
            Self::Remote {
                endpoint_id,
                model_id,
            } => write!(f, "remote:{endpoint_id}:{model_id}"),
        }
    }
}

impl Serialize for ModelRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ModelRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// What a model can be used for. Superset of Demios `ModelCapability`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Chat,
    Vision,
    Embedding,
    Stt,
    Tts,
    Image,
}

/// A named model slot an agent declares (e.g. `primary`, `fast`, `embed`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ModelSlot {
    pub slot_id: String,
    pub capability: Capability,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub default_model: Option<ModelRef>,
    #[serde(default = "yes")]
    pub user_configurable: bool,
}

fn yes() -> bool {
    true
}

/// Readiness of a model source (Demios `ReadinessStatus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    Missing,
    Degraded,
    Disabled,
}

/// Whether a model fits this machine (ported from Demios Edge `ModelFootprintEstimator`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum FitStatus {
    Ready,
    Caution,
    MemoryPressure,
    MemoryBlocked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for s in [
            "local:qwen3-8b-q4_k_m",
            "byok:anthropic:claude-sonnet-5-5",
            "remote:ollama:llama3.3:70b",
        ] {
            let r: ModelRef = s.parse().unwrap();
            assert_eq!(r.to_string(), s);
            let j = serde_json::to_string(&r).unwrap();
            assert_eq!(serde_json::from_str::<ModelRef>(&j).unwrap(), r);
        }
    }

    #[test]
    fn model_id_may_contain_colons() {
        let r: ModelRef = "remote:ollama:llama3.3:70b".parse().unwrap();
        assert_eq!(
            r,
            ModelRef::Remote {
                endpoint_id: "ollama".into(),
                model_id: "llama3.3:70b".into()
            }
        );
    }

    #[test]
    fn rejects_bad_refs() {
        for s in [
            "",
            "local:",
            "byok:openai",
            "byok::gpt",
            "runtime:x",
            "gpt-4o",
        ] {
            assert!(s.parse::<ModelRef>().is_err(), "{s}");
        }
    }
}
