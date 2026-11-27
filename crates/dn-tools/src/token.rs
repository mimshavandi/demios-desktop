//! One-time approval tokens for tools that always need a fresh approval (ADR-0013 §9.4).
//!
//! A tool in the "approval required" class runs only with a token for **that exact call**:
//! the call (its run and call id, or the held call's id), the capability and the exact
//! arguments. The executor checks and consumes the token immediately before it runs the
//! call, wherever the call came from (a chat, an add-on handler, a job).
//!
//! - **Minted only by a person's answer.** In this crate, by the approval card's Allow once
//!   ([`crate::ApprovalDecision::Approve`]), inside the executor; or from a held call a
//!   device answered, through [`crate::Executor::approval_for_held`], which takes the
//!   one-time proof only `dn_jobs::held::take_approved` hands out. There is no other way
//!   to make one, and no token type is in `dn-plugin-api`.
//! - **Single use.** Presenting a token removes it, whether it matched or not.
//! - **Expires.** A token not used within [`TOKEN_TTL`] is refused.
//! - **Withdrawn with its call.** [`crate::Executor::revoke_held_approval`] refuses a token
//!   minted for a call that was withdrawn since.
//!
//! The token is a check in the executor, not a sandbox: in-process add-ons are trusted code
//! (ADR-0013 §1). What it guarantees is that no setting, schedule or add-on request makes
//! such a tool run without a person answering that one call.

use dn_core::Id;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a minted token stays usable.
pub const TOKEN_TTL: Duration = Duration::from_secs(60);

/// A handle to a one-time approval for one call. It carries no authority of its own: the
/// executor's book records what it was minted for, and only the book can mint one.
#[derive(Clone, PartialEq, Eq)]
pub struct ApprovalToken {
    id: Id,
}

impl std::fmt::Debug for ApprovalToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApprovalToken(..)")
    }
}

/// What a token was minted for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    /// `<run id>/<call id>`: for a held call, its run and the held call's id.
    pub call: String,
    pub capability_id: String,
    pub arguments: serde_json::Value,
}

impl Binding {
    pub(crate) fn new(
        run: Id,
        call_id: &str,
        capability_id: &str,
        args: &serde_json::Value,
    ) -> Self {
        Self {
            call: format!("{run}/{call_id}"),
            capability_id: capability_id.to_owned(),
            arguments: args.clone(),
        }
    }
}

/// Why a token was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// Never minted, already used, or its call was withdrawn.
    Unknown,
    Expired,
    /// Minted for another call, tool or arguments.
    OtherCall,
}

#[derive(Debug)]
struct Issued {
    binding: Binding,
    expires: Instant,
}

/// The tokens minted and not yet used.
#[derive(Debug, Default)]
pub(crate) struct TokenBook {
    issued: Mutex<HashMap<Id, Issued>>,
}

impl TokenBook {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Id, Issued>> {
        // A poisoned lock still holds valid entries; refusing every token would only make
        // approved calls fail, never let one through.
        self.issued.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn mint(&self, binding: Binding) -> ApprovalToken {
        let id = Id::new();
        let now = Instant::now();
        let mut book = self.lock();
        book.retain(|_, i| i.expires > now);
        book.insert(
            id,
            Issued {
                binding,
                expires: now + TOKEN_TTL,
            },
        );
        ApprovalToken { id }
    }

    /// Uses up `token` for `binding`. The token is gone afterwards, whatever the answer.
    pub(crate) fn consume(&self, token: &ApprovalToken, binding: &Binding) -> Result<(), Refused> {
        let issued = self.lock().remove(&token.id).ok_or(Refused::Unknown)?;
        if issued.expires <= Instant::now() {
            return Err(Refused::Expired);
        }
        if &issued.binding != binding {
            return Err(Refused::OtherCall);
        }
        Ok(())
    }

    /// Refuses every token minted for `call` (`<run id>/<call id>`).
    pub(crate) fn revoke_call(&self, call: &str) {
        self.lock().retain(|_, i| i.binding.call != call);
    }

    /// Forgets every token ("Delete all local data").
    pub(crate) fn clear(&self) {
        self.lock().clear();
    }

    #[cfg(test)]
    fn age(&self, token: &ApprovalToken, by: Duration) {
        if let Some(i) = self.lock().get_mut(&token.id) {
            i.expires -= by;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding(args: serde_json::Value) -> Binding {
        Binding {
            call: "r/c".into(),
            capability_id: "plugin.mail.send".into(),
            arguments: args,
        }
    }

    #[test]
    fn a_token_is_used_once() {
        let book = TokenBook::default();
        let t = book.mint(binding(json!({"to": "a"})));
        assert_eq!(book.consume(&t, &binding(json!({"to": "a"}))), Ok(()));
        assert_eq!(
            book.consume(&t, &binding(json!({"to": "a"}))),
            Err(Refused::Unknown)
        );
    }

    #[test]
    fn a_token_is_bound_to_the_exact_call_and_a_mismatch_uses_it_up() {
        let book = TokenBook::default();
        let t = book.mint(binding(json!({"to": "a"})));
        assert_eq!(
            book.consume(&t, &binding(json!({"to": "b"}))),
            Err(Refused::OtherCall)
        );
        // Presenting it for the wrong call spent it.
        assert_eq!(
            book.consume(&t, &binding(json!({"to": "a"}))),
            Err(Refused::Unknown)
        );
        let t = book.mint(binding(json!({"to": "a"})));
        let mut other_tool = binding(json!({"to": "a"}));
        other_tool.capability_id = "plugin.mail.forward".into();
        assert_eq!(book.consume(&t, &other_tool), Err(Refused::OtherCall));
        let t = book.mint(binding(json!({"to": "a"})));
        let mut other_call = binding(json!({"to": "a"}));
        other_call.call = "r/d".into();
        assert_eq!(book.consume(&t, &other_call), Err(Refused::OtherCall));
    }

    #[test]
    fn an_expired_or_withdrawn_token_is_refused() {
        let book = TokenBook::default();
        let t = book.mint(binding(json!({})));
        book.age(&t, TOKEN_TTL);
        assert_eq!(book.consume(&t, &binding(json!({}))), Err(Refused::Expired));

        let t = book.mint(binding(json!({})));
        book.revoke_call("r/c");
        assert_eq!(book.consume(&t, &binding(json!({}))), Err(Refused::Unknown));

        let t = book.mint(binding(json!({})));
        book.clear();
        assert_eq!(book.consume(&t, &binding(json!({}))), Err(Refused::Unknown));
    }

    #[test]
    fn a_made_up_token_is_refused() {
        let book = TokenBook::default();
        let _real = book.mint(binding(json!({})));
        let forged = ApprovalToken { id: Id::new() };
        assert_eq!(
            book.consume(&forged, &binding(json!({}))),
            Err(Refused::Unknown)
        );
        assert_eq!(format!("{forged:?}"), "ApprovalToken(..)");
    }
}
