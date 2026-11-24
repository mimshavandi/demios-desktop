//! Validator for the JSON Schema subset used by `schemas/*.schema.json`.
//!
//! Supported keywords: `type`, `enum`, `const`, `required`, `properties`,
//! `additionalProperties` (boolean), `items`, `minItems`, `minLength`, `minimum`,
//! `maximum` and our `format`s (`dotted-id`, `prompt-ref`, `prompt-version`,
//! `identifier`, `model-ref`). Other validators ignore unknown formats, so the schema
//! files stay standard Draft 2020-12 for editors and other tooling. A keyword this
//! validator doesn't know is a bug in our schema files, so the unit tests reject it.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// JSON Pointer to the offending value, e.g. `/model_slots/0/capability`.
    pub path: String,
    pub message: String,
}

#[cfg(test)]
pub(crate) const KNOWN: &[&str] = &[
    "$schema",
    "$id",
    "title",
    "description",
    "type",
    "enum",
    "const",
    "required",
    "properties",
    "additionalProperties",
    "items",
    "minItems",
    "minLength",
    "minimum",
    "maximum",
    "format",
];

pub fn validate(schema: &Value, value: &Value) -> Vec<Problem> {
    let mut out = Vec::new();
    check(schema, value, "", &mut out);
    out
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_ok(want: &str, v: &Value) -> bool {
    let have = type_name(v);
    want == have || (want == "number" && have == "integer")
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => format!("`{s}`"),
        other => other.to_string(),
    }
}

fn check(schema: &Value, v: &Value, path: &str, out: &mut Vec<Problem>) {
    let mut fail = |message: String| {
        out.push(Problem {
            path: if path.is_empty() {
                "/".into()
            } else {
                path.into()
            },
            message,
        })
    };
    if let Some(t) = schema.get("type") {
        let wants: Vec<&str> = match t {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        if !wants.iter().any(|w| type_ok(w, v)) {
            fail(format!(
                "expected {}, found {}",
                wants.join(" or "),
                type_name(v)
            ));
            return;
        }
    }
    if let Some(Value::Array(options)) = schema.get("enum")
        && !options.contains(v)
    {
        let opts: Vec<String> = options.iter().map(show).collect();
        fail(format!("{} is not one of {}", show(v), opts.join(", ")));
        return;
    }
    if let Some(c) = schema.get("const")
        && c != v
    {
        fail(format!("must be {}", show(c)));
        return;
    }
    if let (Some(f), Value::String(s)) = (schema.get("format").and_then(Value::as_str), v)
        && let Err(why) = check_format(f, s)
    {
        fail(format!("{} {why}", show(v)));
    }
    if let (Some(min), Value::String(s)) = (schema.get("minLength").and_then(Value::as_u64), v)
        && (s.chars().count() as u64) < min
    {
        fail(if min == 1 {
            "must not be empty".into()
        } else {
            format!("must be at least {min} characters")
        });
    }
    if let Some(n) = v.as_f64() {
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
            && n < min
        {
            fail(format!("must be ≥ {min}"));
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
            && n > max
        {
            fail(format!("must be ≤ {max}"));
        }
    }
    match v {
        Value::Object(obj) => {
            for r in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !obj.contains_key(r) {
                    out.push(Problem {
                        path: if path.is_empty() {
                            "/".into()
                        } else {
                            path.into()
                        },
                        message: format!("missing required field `{r}`"),
                    });
                }
            }
            let props = schema.get("properties").and_then(Value::as_object);
            let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
            for (k, child) in obj {
                let child_path = format!("{path}/{}", k.replace('~', "~0").replace('/', "~1"));
                match props.and_then(|p| p.get(k)) {
                    Some(s) => check(s, child, &child_path, out),
                    None if closed => {
                        let known: Vec<&str> = props
                            .map(|p| p.keys().map(String::as_str).collect())
                            .unwrap_or_default();
                        out.push(Problem {
                            path: child_path,
                            message: format!(
                                "unknown field `{k}` (expected one of: {})",
                                known.join(", ")
                            ),
                        });
                    }
                    None => {}
                }
            }
        }
        Value::Array(items) => {
            if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
                && (items.len() as u64) < min
            {
                out.push(Problem {
                    path: if path.is_empty() {
                        "/".into()
                    } else {
                        path.into()
                    },
                    message: format!("needs at least {min} item(s)"),
                });
            }
            if let Some(item_schema) = schema.get("items") {
                for (i, item) in items.iter().enumerate() {
                    check(item_schema, item, &format!("{path}/{i}"), out);
                }
            }
        }
        _ => {}
    }
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
        let schema = json!({
            "type": "object", "additionalProperties": false, "required": ["id", "slots"],
            "properties": {
                "id": {"type": "string", "format": "dotted-id"},
                "slots": {"type": "array", "minItems": 1, "items": {"enum": ["chat", "tts"]}},
                "n": {"type": "integer", "minimum": 1}
            }
        });
        let p = validate(
            &schema,
            &json!({"id": "Bad Id", "slots": ["chat", "fax"], "n": 0, "x": 1}),
        );
        let got: Vec<(String, String)> = p.into_iter().map(|p| (p.path, p.message)).collect();
        assert!(got.contains(&(
            "/id".into(),
            "`Bad Id` is not a dotted id (lowercase segments like `text.summarize`)".into()
        )));
        assert!(got.contains(&(
            "/slots/1".into(),
            "`fax` is not one of `chat`, `tts`".into()
        )));
        assert!(got.contains(&("/n".into(), "must be ≥ 1".into())));
        assert!(
            got.iter()
                .any(|(p, m)| p == "/x" && m.starts_with("unknown field `x`"))
        );
        assert!(
            validate(&schema, &json!({}))
                .iter()
                .any(|p| p.message == "missing required field `slots`")
        );
        assert!(validate(&schema, &json!({"id": "a.b", "slots": ["tts"]})).is_empty());
    }

    #[test]
    fn shipped_schemas_use_only_supported_keywords() {
        fn walk(v: &Value, at: &str) {
            if let Value::Object(o) = v {
                for (k, child) in o {
                    if k == "properties" {
                        for (name, s) in child.as_object().unwrap() {
                            walk(s, &format!("{at}/{name}"));
                        }
                        continue;
                    }
                    assert!(
                        KNOWN.contains(&k.as_str()),
                        "unsupported keyword `{k}` at {at}"
                    );
                    walk(child, at);
                }
            }
        }
        for s in [
            crate::manifest::AGENT_SCHEMA,
            crate::manifest::SKILL_SCHEMA,
            crate::manifest::PROMPT_SCHEMA,
        ] {
            walk(&serde_json::from_str(s).unwrap(), "");
        }
    }
}
