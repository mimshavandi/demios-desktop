//! The signed, offline licence file (ADR-0013 §6, ADR-0014 §9a).
//!
//! A licence is a small JSON file: who it's for, what it includes, what kind it is and when
//! it ends, signed with Ed25519 by Demios. This crate reads and checks it on the device.
//! There is no network code here: nothing is sent anywhere, at any time.
//!
//! # The file (format 1)
//!
//! ```json
//! {
//!   "format": 1,
//!   "key": "2026-1",
//!   "payload": {
//!     "entitlements": ["node"],
//!     "expires_at": 1798761600,
//!     "issue_id": "L-0001",
//!     "issued_at": 1795000000,
//!     "kind": "alpha",
//!     "licensee": "Jo Rivera"
//!   },
//!   "signature": "<128 hex digits>"
//! }
//! ```
//!
//! - `key` names the public key that signed it, so a key can be replaced without breaking
//!   older files.
//! - The signature is detached: it covers [`SIGNING_LABEL`] followed by the payload in
//!   canonical JSON (object keys sorted by their UTF-8 bytes, no spaces, integers only).
//!   The payload is checked in the form it was read, so how the file is laid out doesn't
//!   matter, and what is checked is exactly what is used.
//! - Unknown fields are refused, in the envelope and in the payload. A newer format is
//!   refused with a message, never half-read.
//!
//! # Time
//!
//! The caller passes the time, and keeps the latest time it has seen. A clock that went back
//! by more than [`CLOCK_SLACK`] is refused with "check your computer's clock" instead of being
//! trusted ([`checked_now`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The file format this version reads and writes.
pub const FORMAT: u32 = 1;

/// Signed in front of the canonical payload, so a licence signature is never valid for
/// anything else the same key might sign.
pub const SIGNING_LABEL: &[u8] = b"demios-licence/1\n";

/// The entitlement for the Demios Node (`dn node run`, `dn node pair`).
pub const NODE: &str = "node";

/// How far back the clock may move before a licence check refuses to trust it: one day.
pub const CLOCK_SLACK: i64 = 86_400;

/// A licence file is small; anything larger is refused before it is parsed.
pub const MAX_FILE_BYTES: usize = 64 * 1024;

/// A public key this build trusts for licences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicKey {
    /// The name a licence file gives in `key`.
    pub id: &'static str,
    /// The Ed25519 public key.
    pub ed25519: [u8; 32],
}

/// The keys this build trusts. Empty until the owner makes the signing key offline
/// (demios-desktop#377): until then no licence checks out, and nothing in the app relies on one.
pub const TRUSTED: &[PublicKey] = &[];

/// What a licence is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A time-limited trial.
    Trial,
    /// A founding-member licence.
    Founding,
    /// A Pro licence.
    Pro,
    /// A hand-signed licence for the alpha.
    Alpha,
}

impl Kind {
    /// Kinds that must say when they end.
    fn must_end(self) -> bool {
        matches!(self, Self::Trial | Self::Alpha)
    }
}

/// What a licence says, as it is signed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    /// Demios's id for this licence, for a refresh the person asks for.
    pub issue_id: String,
    /// Who it is for, shown as plain text.
    pub licensee: String,
    pub kind: Kind,
    /// What it includes: [`NODE`] and add-on ids.
    pub entitlements: Vec<String>,
    /// When it was made, Unix seconds.
    pub issued_at: i64,
    /// When it ends, Unix seconds; `None` for a licence that doesn't end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

/// A licence whose signature checked out with a trusted key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Licence {
    /// The id of the key that signed it.
    pub key: String,
    pub payload: Payload,
}

/// The file as stored.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: u32,
    key: String,
    payload: Value,
    signature: String,
}

/// Why a licence file can't be used. Each message completes "The licence can't be used: …".
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LicenceError {
    #[error("it isn't a valid Demios licence file ({0})")]
    Damaged(String),
    #[error("it's in a newer format ({0}). Update Demios to use it")]
    NewerFormat(u32),
    #[error("it was signed with a key this version of Demios doesn't know ({0})")]
    UnknownKey(String),
    #[error("its signature doesn't match")]
    Signature,
    /// The clock is more than [`CLOCK_SLACK`] behind the latest time seen, or before the
    /// licence was made.
    #[error("this computer's clock is behind. Check your computer's date and time")]
    ClockBehind,
}

/// Reads `file` and checks its signature with `keys` (normally [`TRUSTED`]). The dates are
/// checked separately, by [`Licence::status`].
pub fn verify(file: &[u8], keys: &[PublicKey]) -> Result<Licence, LicenceError> {
    if file.len() > MAX_FILE_BYTES {
        return Err(damaged("too large"));
    }
    let envelope: Envelope = serde_json::from_slice(file).map_err(|e| damaged(&format!("{e}")))?;
    if envelope.format != FORMAT {
        return Err(if envelope.format > FORMAT {
            LicenceError::NewerFormat(envelope.format)
        } else {
            damaged("unknown format")
        });
    }
    let key = keys
        .iter()
        .find(|k| k.id == envelope.key)
        .ok_or_else(|| LicenceError::UnknownKey(envelope.key.clone()))?;
    let signature = hex(&envelope.signature).ok_or(LicenceError::Signature)?;
    if signature.len() != 64 {
        return Err(LicenceError::Signature);
    }
    let mut signed = SIGNING_LABEL.to_vec();
    canonical(&envelope.payload, &mut signed)?;
    aws_lc_rs::signature::UnparsedPublicKey::new(&aws_lc_rs::signature::ED25519, key.ed25519)
        .verify(&signed, &signature)
        .map_err(|_| LicenceError::Signature)?;
    let payload: Payload =
        serde_json::from_value(envelope.payload).map_err(|e| damaged(&format!("{e}")))?;
    validate(&payload)?;
    Ok(Licence {
        key: envelope.key,
        payload,
    })
}

/// The bytes a signer signs for `payload`: [`SIGNING_LABEL`] and the canonical payload.
pub fn signing_bytes(payload: &Payload) -> Result<Vec<u8>, LicenceError> {
    validate(payload)?;
    let value = serde_json::to_value(payload).map_err(|e| damaged(&format!("{e}")))?;
    let mut out = SIGNING_LABEL.to_vec();
    canonical(&value, &mut out)?;
    Ok(out)
}

/// The licence file for `payload`, signed by key `key` with the 64-byte Ed25519
/// `signature` over [`signing_bytes`]. For the signing tool; the app never signs.
pub fn write_file(payload: &Payload, key: &str, signature: &[u8]) -> Result<String, LicenceError> {
    validate(payload)?;
    if signature.len() != 64 {
        return Err(LicenceError::Signature);
    }
    let envelope = Envelope {
        format: FORMAT,
        key: key.to_owned(),
        payload: serde_json::to_value(payload).map_err(|e| damaged(&format!("{e}")))?,
        signature: signature.iter().map(|b| format!("{b:02x}")).collect(),
    };
    let mut text = serde_json::to_string_pretty(&envelope).map_err(|e| damaged(&format!("{e}")))?;
    text.push('\n');
    Ok(text)
}

/// The time to check a licence against, given the clock `now` and the latest time this
/// device has seen. Moving the clock back a little uses the latest time seen, so it never
/// buys time; more than [`CLOCK_SLACK`] back is refused.
pub fn checked_now(now: i64, latest_seen: Option<i64>) -> Result<i64, LicenceError> {
    match latest_seen {
        Some(seen) if now.saturating_add(CLOCK_SLACK) < seen => Err(LicenceError::ClockBehind),
        Some(seen) => Ok(now.max(seen)),
        None => Ok(now),
    }
}

/// A licence's state for one entitlement, as the Node reports it and the app shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// The licence covers it now.
    Valid {
        kind: Kind,
        licensee: String,
        /// Unix seconds; `None` when it doesn't end.
        expires_at: Option<i64>,
    },
    /// The licence ended.
    Ended {
        kind: Kind,
        licensee: String,
        /// Unix seconds.
        ended_at: i64,
    },
    /// No licence file.
    Missing,
    /// A file that can't be used, with the reason in words.
    Unusable { reason: String },
}

impl Status {
    /// The state for a file that couldn't be read or checked.
    pub fn unusable(error: &LicenceError) -> Self {
        Self::Unusable {
            reason: error.to_string(),
        }
    }

    /// True when the licensed feature may run.
    pub fn allows_use(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }
}

impl Licence {
    /// Whether this licence covers `entitlement` at `now` (from [`checked_now`]).
    pub fn status(&self, entitlement: &str, now: i64) -> Status {
        let p = &self.payload;
        if now.saturating_add(CLOCK_SLACK) < p.issued_at {
            return Status::unusable(&LicenceError::ClockBehind);
        }
        if !p.entitlements.iter().any(|e| e == entitlement) {
            let what = if entitlement == NODE {
                "the Node"
            } else {
                "this add-on"
            };
            return Status::Unusable {
                reason: format!("it doesn't include {what}"),
            };
        }
        match p.expires_at {
            Some(end) if now >= end => Status::Ended {
                kind: p.kind,
                licensee: p.licensee.clone(),
                ended_at: end,
            },
            expires_at => Status::Valid {
                kind: p.kind,
                licensee: p.licensee.clone(),
                expires_at,
            },
        }
    }
}

fn damaged(what: &str) -> LicenceError {
    LicenceError::Damaged(what.to_owned())
}

/// What every licence must hold, whoever made it.
fn validate(p: &Payload) -> Result<(), LicenceError> {
    if p.issue_id.trim().is_empty() {
        return Err(damaged("no issue id"));
    }
    if p.licensee.trim().is_empty() {
        return Err(damaged("no licensee"));
    }
    if p.entitlements.is_empty() || p.entitlements.iter().any(|e| e.trim().is_empty()) {
        return Err(damaged("no entitlements"));
    }
    match p.expires_at {
        None if p.kind.must_end() => Err(damaged("a trial or alpha licence must end")),
        Some(end) if end <= p.issued_at => Err(damaged("it ends before it starts")),
        _ => Ok(()),
    }
}

/// Canonical JSON: object keys sorted by their UTF-8 bytes, no spaces, integers only.
fn canonical(value: &Value, out: &mut Vec<u8>) -> Result<(), LicenceError> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => {
            if !(n.is_i64() || n.is_u64()) {
                return Err(damaged("a number that isn't a whole number"));
            }
            out.extend_from_slice(n.to_string().as_bytes());
        }
        Value::String(s) => string(s, out)?,
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                canonical(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            // Sorted here, whatever order the map keeps (serde_json's `preserve_order`).
            let sorted: BTreeMap<&str, &Value> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
            out.push(b'{');
            for (i, (k, v)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                string(k, out)?;
                out.push(b':');
                canonical(v, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn string(s: &str, out: &mut Vec<u8>) -> Result<(), LicenceError> {
    let text = serde_json::to_string(s).map_err(|e| damaged(&format!("{e}")))?;
    out.extend_from_slice(text.as_bytes());
    Ok(())
}

fn hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    // Digits only: `from_str_radix` alone would also take a sign.
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests;
