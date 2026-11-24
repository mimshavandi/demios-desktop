//! JSON Schema validation for manifests (via the `jsonschema` crate, no remote `$ref`).
//!
//! The bundled schemas use standard Draft 2020-12 plus our own `format`s
//! (`dotted-id`, `prompt-ref`, `prompt-version`, `identifier`, `model-ref`), which are
//! registered here and enforced.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// JSON Pointer to the offending value, e.g. `/model_slots/0/capability`.
    pub path: String,
    pub message: String,
}

const FORMATS: &[&str] = &[
    "dotted-id",
    "prompt-ref",
    "prompt-version",
    "identifier",
    "model-ref",
];

/// Compiles a bundled schema with our formats. Panics only on a broken bundled schema,
/// which the unit tests catch.
pub fn compile(schema: &Value) -> jsonschema::Validator {
    let mut opts = jsonschema::options().should_validate_formats(true);
    for &f in FORMATS {
        opts = opts.with_format(f, move |s: &str| check_format(f, s).is_ok());
    }
    opts.build(schema).expect("bundled schema compiles")
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => format!("`{s}`"),
        other => other.to_string(),
    }
}

pub fn validate(validator: &jsonschema::Validator, value: &Value) -> Vec<Problem> {
    validator
        .iter_errors(value)
        .map(|e| {
            let path = e.instance_path().to_string();
            let message = match e.kind() {
                jsonschema::error::ValidationErrorKind::Format { format } => {
                    let shown = show(e.instance().as_ref());
                    match check_format(format, e.instance().as_str().unwrap_or_default()) {
                        Err(why) => format!("{shown} {why}"),
                        Ok(()) => e.to_string(),
                    }
                }
                jsonschema::error::ValidationErrorKind::Enum { options } => {
                    let opts: Vec<String> =
                        options.as_array().into_iter().flatten().map(show).collect();
                    format!(
                        "{} is not one of {}",
                        show(e.instance().as_ref()),
                        opts.join(", ")
                    )
                }
                _ => e.to_string(),
            };
            Problem {
                path: if path.is_empty() { "/".into() } else { path },
                message,
            }
        })
        .collect()
}

pub(crate) fn is_dotted_id(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|seg| {
            let mut c = seg.chars();
            c.next().is_some_and(|f| f.is_ascii_lowercase())
                && c.all(|ch| {
                    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-'
                })
        })
}

pub(crate) fn is_version(s: &str) -> bool {
    s.strip_prefix('v').is_some_and(|n| {
        !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && !n.starts_with('0')
    })
}

pub(crate) fn is_identifier(s: &str) -> bool {
    let mut c = s.chars();
    c.next()
        .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn check_format(format: &str, s: &str) -> Result<(), &'static str> {
    let ok = match format {
        "dotted-id" => is_dotted_id(s),
        "prompt-version" => is_version(s),
        "identifier" => is_identifier(s),
        "prompt-ref" => match s.split_once('@') {
            Some((id, v)) => is_dotted_id(id) && is_version(v),
            None => is_dotted_id(s),
        },
        "model-ref" => s.parse::<dn_core::model::ModelRef>().is_ok(),
        _ => true,
    };
    if ok {
        return Ok(());
    }
    Err(match format {
        "dotted-id" => "is not a dotted id (lowercase segments like `text.summarize`)",
        "prompt-version" => "is not a version like `v1`",
        "identifier" => "is not an identifier (letters, digits, `_`)",
        "prompt-ref" => "is not a prompt ref like `text.summarize` or `text.summarize@v2`",
        "model-ref" => {
            "is not a model ref (local:<id>, byok:<provider>:<id>, remote:<endpoint>:<id>)"
        }
        _ => "is invalid",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reports_paths_and_reasons() {
        let v = compile(&json!({
            "type": "object", "additionalProperties": false, "required": ["id", "slots"],
            "properties": {
                "id": {"type": "string", "format": "dotted-id"},
                "slots": {"type": "array", "minItems": 1, "items": {"enum": ["chat", "tts"]}},
                "n": {"type": "integer", "minimum": 1}
            }
        }));
        let p = validate(
            &v,
            &json!({"id": "Bad Id", "slots": ["chat", "fax"], "n": 0, "x": 1}),
        );
        let at = |path: &str| {
            p.iter()
                .filter(|x| x.path == path)
                .map(|x| x.message.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            at("/id"),
            ["`Bad Id` is not a dotted id (lowercase segments like `text.summarize`)"]
        );
        assert_eq!(at("/slots/1"), ["`fax` is not one of `chat`, `tts`"]);
        assert_eq!(at("/n").len(), 1, "{p:?}");
        assert!(
            p.iter().any(|x| x.path == "/" && x.message.contains("'x'")),
            "{p:?}"
        );
        assert!(
            validate(&v, &json!({}))
                .iter()
                .any(|x| x.message.contains("\"slots\""))
        );
        assert!(validate(&v, &json!({"id": "a.b", "slots": ["tts"]})).is_empty());
    }

    #[test]
    fn bundled_schemas_compile() {
        for s in [
            crate::manifest::AGENT_SCHEMA,
            crate::manifest::SKILL_SCHEMA,
            crate::manifest::PROMPT_SCHEMA,
        ] {
            let schema: Value = serde_json::from_str(s).unwrap();
            jsonschema::meta::validate(&schema).unwrap();
            compile(&schema);
        }
    }
}
