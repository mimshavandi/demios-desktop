//! HTTP plumbing shared by the adapters: the "drop unsupported params and retry once"
//! gateway policy, and mapping HTTP failures to typed `CoreError`s.

use crate::sse::{SseDecoder, SseEvent};
use dn_core::{CoreError, RecoveryAction};
use serde_json::Value;

/// An optional request parameter the gateway may drop if the provider rejects it.
#[derive(Clone, Debug)]
pub(crate) struct OptionalParam {
    /// Dot path into the JSON body, e.g. `generationConfig.thinkingConfig`.
    pub path: &'static str,
    /// Extra words that identify this parameter in error messages (besides its own name).
    pub aliases: &'static [&'static str],
    /// Rename instead of dropping when the error suggests the new name.
    pub rename_to: Option<&'static str>,
    /// Called with the removed value, so a param that must not silently disappear (a
    /// response schema) can be replaced by something equivalent (a prompt instruction).
    pub fallback: Option<fn(&mut Value, Value)>,
}

impl OptionalParam {
    pub const fn new(path: &'static str) -> Self {
        Self {
            path,
            aliases: &[],
            rename_to: None,
            fallback: None,
        }
    }
}

/// Instruction used when a provider can't enforce a response schema natively.
pub(crate) fn schema_instruction(schema: &Value) -> String {
    format!(
        "Reply with only a JSON value (no prose, no code fences) that matches this JSON Schema:\n{schema}"
    )
}

/// Header names whose values are credentials: marked sensitive so they never show up in
/// `Debug` output or HTTP/2 header tables.
const SENSITIVE: &[&str] = &["authorization", "x-api-key", "x-goog-api-key"];

pub(crate) struct Call<'a> {
    pub client: &'a reqwest::Client,
    pub provider_id: &'a str,
    pub method: reqwest::Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Option<Value>,
    pub optional: Vec<OptionalParam>,
    /// The key, so we can scrub it from anything echoed back in error bodies.
    pub secret: Option<&'a str>,
}

/// Sends the request. On a 400 that names one of `optional`, removes (or renames) that
/// parameter and retries exactly once.
pub(crate) async fn send(mut call: Call<'_>) -> Result<reqwest::Response, CoreError> {
    let mut retried = false;
    loop {
        let mut rb = call.client.request(call.method.clone(), &call.url);
        for (k, v) in &call.headers {
            let mut hv = reqwest::header::HeaderValue::from_str(v).map_err(|_| {
                unavailable(
                    "The API key contains characters that can't be sent.",
                    format!("{}: invalid value for header `{k}`", call.provider_id),
                    vec![RecoveryAction::AddProviderKey],
                )
            })?;
            hv.set_sensitive(SENSITIVE.contains(k));
            rb = rb.header(*k, hv);
        }
        if let Some(body) = &call.body {
            rb = rb
                .header("content-type", "application/json")
                .body(serde_json::to_vec(body).unwrap_or_default());
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| network_error(call.provider_id, &e))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u32>().ok());
        let text = resp.text().await.unwrap_or_default();
        if status.as_u16() == 400
            && !retried
            && let Some(body) = call.body.as_mut()
            && drop_unsupported(body, &call.optional, &text)
        {
            retried = true;
            continue;
        }
        return Err(http_error(
            call.provider_id,
            status.as_u16(),
            retry_after,
            &text,
            call.secret,
        ));
    }
}

/// Removes/renames the first optional param present in `body` that `error_text` names.
fn drop_unsupported(body: &mut Value, optional: &[OptionalParam], error_text: &str) -> bool {
    let err = error_text.to_ascii_lowercase();
    for p in optional {
        let leaf = p.path.rsplit('.').next().unwrap_or(p.path);
        let names = std::iter::once(leaf.to_string())
            .chain(std::iter::once(snake_case(leaf)))
            .chain(p.aliases.iter().map(|a| a.to_string()));
        let mentioned = names
            .map(|n| n.to_ascii_lowercase())
            .any(|n| err.contains(&n));
        if !mentioned {
            continue;
        }
        let Some(value) = take_path(body, p.path) else {
            continue;
        };
        if let Some(new) = p.rename_to
            && err.contains(new)
        {
            set_path(body, new, value);
        } else if let Some(f) = p.fallback {
            f(body, value);
        }
        return true;
    }
    false
}

fn snake_case(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn take_path(body: &mut Value, path: &str) -> Option<Value> {
    let mut parts: Vec<&str> = path.split('.').collect();
    let last = parts.pop()?;
    let mut cur = body;
    for p in parts {
        cur = cur.get_mut(p)?;
    }
    cur.as_object_mut()?.remove(last)
}

fn set_path(body: &mut Value, path: &str, value: Value) {
    if let Some(obj) = body.as_object_mut() {
        obj.insert(path.to_string(), value);
    }
}

/// Scrubs the key and truncates provider error bodies before they go into an error.
pub(crate) fn redact(text: &str, secret: Option<&str>) -> String {
    let mut t = text.to_string();
    if let Some(s) = secret.filter(|s| s.len() >= 4) {
        t = t.replace(s, "***");
    }
    if t.len() > 500 {
        let mut cut = 500;
        while !t.is_char_boundary(cut) {
            cut -= 1;
        }
        t.truncate(cut);
        t.push('…');
    }
    t
}

pub(crate) fn unavailable(
    user_message: impl Into<String>,
    developer_message: impl Into<String>,
    recovery: Vec<RecoveryAction>,
) -> CoreError {
    CoreError::ModelUnavailable {
        user_message: user_message.into(),
        developer_message: developer_message.into(),
        recovery,
    }
}

pub(crate) fn http_error(
    provider_id: &str,
    status: u16,
    retry_after: Option<u32>,
    body: &str,
    secret: Option<&str>,
) -> CoreError {
    let dev = format!("{provider_id}: HTTP {status}: {}", redact(body, secret));
    match status {
        401 | 403 => unavailable(
            format!("The API key for {provider_id} was rejected. Add a valid key."),
            dev,
            vec![RecoveryAction::AddProviderKey],
        ),
        404 => unavailable(
            format!("{provider_id} doesn't offer this model, or the endpoint URL is wrong."),
            dev,
            vec![],
        ),
        408 | 429 => unavailable(
            format!("{provider_id} is rate limiting requests. Try again shortly."),
            dev,
            vec![RecoveryAction::Retry {
                after_seconds: retry_after,
            }],
        ),
        500..=599 => unavailable(
            format!("{provider_id} is having problems. Try again shortly."),
            dev,
            vec![RecoveryAction::Retry {
                after_seconds: retry_after,
            }],
        ),
        _ => unavailable(format!("{provider_id} rejected the request."), dev, vec![]),
    }
}

pub(crate) fn network_error(provider_id: &str, e: &reqwest::Error) -> CoreError {
    unavailable(
        format!("Couldn't reach {provider_id}. Check your connection or the endpoint URL."),
        format!("{provider_id}: {e}"),
        vec![RecoveryAction::Retry {
            after_seconds: None,
        }],
    )
}

pub(crate) fn stream_error(provider_id: &str, detail: &str, secret: Option<&str>) -> CoreError {
    unavailable(
        format!("{provider_id} stopped the response with an error."),
        format!("{provider_id}: stream error: {}", redact(detail, secret)),
        vec![RecoveryAction::Retry {
            after_seconds: None,
        }],
    )
}

/// The body ended cleanly but without the provider's end-of-stream marker.
pub(crate) fn truncated(provider_id: &str, missing: &str) -> CoreError {
    unavailable(
        format!("The response from {provider_id} was cut off."),
        format!("{provider_id}: stream ended without {missing}"),
        vec![RecoveryAction::Retry {
            after_seconds: None,
        }],
    )
}

pub(crate) fn bad_response(provider_id: &str, detail: impl std::fmt::Display) -> CoreError {
    unavailable(
        format!("{provider_id} sent a response we couldn't read."),
        format!("{provider_id}: {detail}"),
        vec![],
    )
}

/// Pull-based SSE reader over a streaming HTTP body.
pub(crate) struct SseReader<'a> {
    provider_id: &'a str,
    resp: reqwest::Response,
    dec: SseDecoder,
    pending: std::collections::VecDeque<SseEvent>,
    eof: bool,
}

impl<'a> SseReader<'a> {
    pub fn new(provider_id: &'a str, resp: reqwest::Response) -> Self {
        Self {
            provider_id,
            resp,
            dec: SseDecoder::new(),
            pending: Default::default(),
            eof: false,
        }
    }

    pub async fn next(&mut self) -> Result<Option<SseEvent>, CoreError> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Ok(Some(ev));
            }
            if self.eof {
                return Ok(None);
            }
            match self
                .resp
                .chunk()
                .await
                .map_err(|e| network_error(self.provider_id, &e))?
            {
                Some(chunk) => self.pending.extend(self.dec.feed(&chunk)),
                None => {
                    self.eof = true;
                    self.pending.extend(self.dec.finish());
                }
            }
        }
    }
}

pub(crate) async fn json(provider_id: &str, resp: reqwest::Response) -> Result<Value, CoreError> {
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| network_error(provider_id, &e))?;
    serde_json::from_slice(&bytes).map_err(|e| bad_response(provider_id, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn drops_named_param_and_renames_when_suggested() {
        let opt = vec![
            OptionalParam::new("temperature"),
            OptionalParam {
                path: "max_tokens",
                rename_to: Some("max_completion_tokens"),
                ..OptionalParam::new("max_tokens")
            },
            OptionalParam {
                aliases: &["thinking"],
                ..OptionalParam::new("generationConfig.thinkingConfig")
            },
        ];
        let mut b = json!({"temperature": 0.2, "max_tokens": 10});
        assert!(drop_unsupported(
            &mut b,
            &opt,
            "Unsupported value: 'temperature' does not support 0.2 with this model."
        ));
        assert_eq!(b, json!({"max_tokens": 10}));
        assert!(drop_unsupported(
            &mut b,
            &opt,
            "Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead."
        ));
        assert_eq!(b, json!({"max_completion_tokens": 10}));
        let mut g = json!({"generationConfig": {"thinkingConfig": {"thinkingBudget": 0}, "temperature": 1}});
        assert!(drop_unsupported(
            &mut g,
            &opt,
            "Budget 0 is invalid. This model only works in thinking mode."
        ));
        assert_eq!(g, json!({"generationConfig": {"temperature": 1}}));
        assert!(!drop_unsupported(&mut g, &opt, "messages: field required"));
    }

    #[test]
    fn redacts_key_from_error_bodies() {
        let r = redact("Incorrect API key provided: sk-abc123", Some("sk-abc123"));
        assert!(!r.contains("sk-abc123"));
    }
}
