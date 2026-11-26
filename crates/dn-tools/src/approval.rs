//! Approval gate (T-CON-03). Calls that need approval are sent to the UI as
//! [`ApprovalRequest`]s on an async channel; the UI answers each with an [`ApprovalDecision`].
//!
//! Policies (ADR-0006): `always_ask`, `ask_once_per_conversation` (an "allow for this chat"
//! answer is remembered for the conversation), `auto`.

use dn_core::Id;
use dn_core::capability::{ApprovalPolicy, ApprovalRequester, RiskTier};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// How long a call waits for an answer before it is denied (unless changed with
/// [`ApprovalGate::with_timeout`]).
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Run this call once.
    Approve,
    /// Run, and don't ask again for this capability in this conversation
    /// (only honoured for `ask_once_per_conversation`; `always_ask` asks every time).
    ApproveForConversation,
    Deny,
}

/// Serializable part of a request, for the UI (Tauri event payload).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalPrompt {
    pub request_id: Id,
    /// Grant scope: `conv:<conversation id>`, or `run:<run id>` for runs without a chat.
    /// "Allow for this chat" grants are remembered per scope.
    pub scope: String,
    pub capability_id: String,
    pub title: String,
    pub description: String,
    pub risk: RiskTier,
    pub policy: ApprovalPolicy,
    /// The exact arguments the tool will receive. The UI must show these: the model may have
    /// been prompt-injected (docs/security/mcp-tools.md).
    pub arguments: serde_json::Value,
    /// Who asked: the chat's model, or an add-on (and its job). The UI names an add-on on
    /// the prompt, so its request never looks like the model's own (ADR-0013 §4).
    #[serde(default)]
    pub requester: ApprovalRequester,
}

/// A pending approval. Dropping it without answering counts as `Deny`.
#[derive(Debug)]
pub struct ApprovalRequest {
    pub prompt: ApprovalPrompt,
    responder: oneshot::Sender<ApprovalDecision>,
}

impl ApprovalRequest {
    /// Delivers the answer. Returns `false` when the waiting call is already gone (timed
    /// out, cancelled): the decision had no effect and the UI must say so.
    pub fn respond(self, decision: ApprovalDecision) -> bool {
        self.responder.send(decision).is_ok()
    }

    /// The waiting call is gone (timed out, cancelled or its run ended): answering it now
    /// has no effect.
    pub fn is_closed(&self) -> bool {
        self.responder.is_closed()
    }
}

/// What the gate concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateOutcome {
    /// Allowed without asking (policy `auto` or a remembered grant).
    AutoAllowed,
    Approved,
    Denied,
    /// No answer within the timeout, or the UI side is gone.
    Unanswered,
}

impl GateOutcome {
    pub fn allowed(self) -> bool {
        matches!(self, Self::AutoAllowed | Self::Approved)
    }
}

#[derive(Debug)]
pub struct ApprovalGate {
    tx: mpsc::Sender<ApprovalRequest>,
    grants: Mutex<HashSet<(String, String)>>,
    timeout: Option<Duration>,
}

impl ApprovalGate {
    /// Create a gate and the receiver the UI drains.
    pub fn new(buffer: usize) -> (Self, mpsc::Receiver<ApprovalRequest>) {
        let (tx, rx) = mpsc::channel(buffer.max(1));
        (
            Self {
                tx,
                grants: Mutex::new(HashSet::new()),
                timeout: Some(DEFAULT_APPROVAL_TIMEOUT),
            },
            rx,
        )
    }

    /// Deny when the user doesn't answer in time. Default: [`DEFAULT_APPROVAL_TIMEOUT`].
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// True when a call would run without asking.
    pub fn is_satisfied(
        &self,
        conversation_id: &str,
        capability_id: &str,
        policy: ApprovalPolicy,
    ) -> bool {
        match policy {
            ApprovalPolicy::Auto => true,
            ApprovalPolicy::AlwaysAsk => false,
            ApprovalPolicy::AskOncePerConversation => self
                .grants
                .lock()
                .is_ok_and(|g| g.contains(&(conversation_id.to_owned(), capability_id.to_owned()))),
        }
    }

    /// Forget "allow for this chat" grants for a scope (`CallContext::scope()`, e.g. when the
    /// conversation is closed).
    pub fn clear_conversation(&self, conversation_id: &str) {
        if let Ok(mut g) = self.grants.lock() {
            g.retain(|(c, _)| c != conversation_id);
        }
    }

    /// Forget "allow for this chat" grants for capabilities starting with `prefix` (e.g.
    /// `mcp.<server>.` when a server is removed or points somewhere new).
    pub fn clear_capability_prefix(&self, prefix: &str) {
        if let Ok(mut g) = self.grants.lock() {
            g.retain(|(_, cap)| !cap.starts_with(prefix));
        }
    }

    /// Like [`Self::clear_capability_prefix`], and returns the grants it forgot as
    /// `(scope, capability)` pairs, so an Undo can put them back ([`Self::restore_grants`]).
    /// A poisoned lock forgets nothing it can return: the calls then ask again.
    pub fn take_capability_prefix(&self, prefix: &str) -> Vec<(String, String)> {
        let Ok(mut g) = self.grants.lock() else {
            return Vec::new();
        };
        let taken: Vec<(String, String)> = g
            .iter()
            .filter(|(_, cap)| cap.starts_with(prefix))
            .cloned()
            .collect();
        g.retain(|(_, cap)| !cap.starts_with(prefix));
        taken
    }

    /// Puts back grants returned by [`Self::take_capability_prefix`] (Undo of a removal).
    pub fn restore_grants(&self, grants: impl IntoIterator<Item = (String, String)>) {
        if let Ok(mut g) = self.grants.lock() {
            g.extend(grants);
        }
    }

    /// Forget every "allow for this chat" grant ("Delete all local data").
    pub fn clear_all(&self) {
        if let Ok(mut g) = self.grants.lock() {
            g.clear();
        }
    }

    /// Ask if the policy requires it and wait for the answer.
    pub async fn check(&self, prompt: ApprovalPrompt) -> GateOutcome {
        if self.is_satisfied(&prompt.scope, &prompt.capability_id, prompt.policy) {
            return GateOutcome::AutoAllowed;
        }
        let key = (prompt.scope.clone(), prompt.capability_id.clone());
        let policy = prompt.policy;
        let (responder, answer) = oneshot::channel();
        if self
            .tx
            .send(ApprovalRequest { prompt, responder })
            .await
            .is_err()
        {
            return GateOutcome::Unanswered;
        }
        let decision = match self.timeout {
            Some(t) => match tokio::time::timeout(t, answer).await {
                Ok(r) => r.ok(),
                Err(_) => None,
            },
            None => answer.await.ok(),
        };
        match decision {
            Some(ApprovalDecision::Approve) => GateOutcome::Approved,
            Some(ApprovalDecision::ApproveForConversation) => {
                if policy == ApprovalPolicy::AskOncePerConversation
                    && let Ok(mut g) = self.grants.lock()
                {
                    g.insert(key);
                }
                GateOutcome::Approved
            }
            Some(ApprovalDecision::Deny) => GateOutcome::Denied,
            None => GateOutcome::Unanswered,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn request_reports_closed_after_timeout() {
        let (gate, mut rx) = ApprovalGate::new(1);
        let gate = gate.with_timeout(Duration::from_millis(20));
        let check =
            tokio::spawn(async move { gate.check(prompt(ApprovalPolicy::AlwaysAsk)).await });
        let req = rx.recv().await.unwrap();
        assert!(!req.is_closed());
        assert_eq!(check.await.unwrap(), GateOutcome::Unanswered);
        assert!(req.is_closed());
        assert!(
            !req.respond(ApprovalDecision::Approve),
            "a late answer has no effect"
        );
    }

    fn prompt(policy: ApprovalPolicy) -> ApprovalPrompt {
        ApprovalPrompt {
            request_id: Id::new(),
            scope: "c1".into(),
            capability_id: "mcp.fs.write_file".into(),
            title: "write_file".into(),
            description: String::new(),
            risk: RiskTier::LowWrite,
            policy,
            arguments: serde_json::json!({}),
            requester: ApprovalRequester::Model,
        }
    }

    #[tokio::test]
    async fn ask_once_remembers_grant() {
        let (gate, mut rx) = ApprovalGate::new(4);
        let ui = tokio::spawn(async move {
            let req = rx.recv().await.unwrap();
            assert_eq!(req.prompt.capability_id, "mcp.fs.write_file");
            assert!(req.respond(ApprovalDecision::ApproveForConversation));
            rx
        });
        assert_eq!(
            gate.check(prompt(ApprovalPolicy::AskOncePerConversation))
                .await,
            GateOutcome::Approved
        );
        let _rx = ui.await.unwrap();
        assert_eq!(
            gate.check(prompt(ApprovalPolicy::AskOncePerConversation))
                .await,
            GateOutcome::AutoAllowed
        );
        gate.clear_conversation("c1");
        assert!(!gate.is_satisfied(
            "c1",
            "mcp.fs.write_file",
            ApprovalPolicy::AskOncePerConversation
        ));
    }

    #[test]
    fn clearing_a_server_prefix_keeps_other_grants() {
        let (gate, _rx) = ApprovalGate::new(1);
        {
            let mut g = gate.grants.lock().unwrap();
            g.insert(("c1".into(), "mcp.fs.write_file".into()));
            g.insert(("c2".into(), "mcp.fs.delete".into()));
            g.insert(("c1".into(), "mcp.fsx.write".into()));
        }
        gate.clear_capability_prefix("mcp.fs.");
        let ask = ApprovalPolicy::AskOncePerConversation;
        assert!(!gate.is_satisfied("c1", "mcp.fs.write_file", ask));
        assert!(!gate.is_satisfied("c2", "mcp.fs.delete", ask));
        assert!(
            gate.is_satisfied("c1", "mcp.fsx.write", ask),
            "other server kept"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn default_timeout_denies_after_ten_minutes() {
        let (gate, mut rx) = ApprovalGate::new(4);
        assert_eq!(gate.timeout, Some(DEFAULT_APPROVAL_TIMEOUT));
        let _ui = tokio::spawn(async move {
            let _req = rx.recv().await; // never answered
            std::future::pending::<()>().await;
        });
        // Paused clock: the runtime auto-advances to the 10-minute deadline.
        assert_eq!(
            gate.check(prompt(ApprovalPolicy::AlwaysAsk)).await,
            GateOutcome::Unanswered
        );
    }

    #[tokio::test]
    async fn dropped_request_and_timeout_deny() {
        let (gate, mut rx) = ApprovalGate::new(4);
        let gate = gate.with_timeout(Duration::from_millis(50));
        tokio::spawn(async move {
            let req = rx.recv().await.unwrap();
            drop(req);
            let _keep = rx.recv().await;
        });
        assert_eq!(
            gate.check(prompt(ApprovalPolicy::AlwaysAsk)).await,
            GateOutcome::Unanswered
        );
        assert_eq!(
            gate.check(prompt(ApprovalPolicy::Auto)).await,
            GateOutcome::AutoAllowed
        );
    }
}
