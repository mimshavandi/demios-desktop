//! Approval gate (T-CON-03). Calls that need approval are sent to the UI as
//! [`ApprovalRequest`]s on an async channel; the UI answers each with an [`ApprovalDecision`].
//!
//! Policies (ADR-0006): `always_ask`, `ask_once_per_conversation` (an "allow for this chat"
//! answer is remembered for the conversation), `auto`.

use dn_core::Id;
use dn_core::capability::{ApprovalPolicy, RiskTier};
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
    pub conversation_id: String,
    pub capability_id: String,
    pub title: String,
    pub description: String,
    pub risk: RiskTier,
    pub policy: ApprovalPolicy,
    /// The exact arguments the tool will receive. The UI must show these: the model may have
    /// been prompt-injected (docs/security/mcp-tools.md).
    pub arguments: serde_json::Value,
}

/// A pending approval. Dropping it without answering counts as `Deny`.
#[derive(Debug)]
pub struct ApprovalRequest {
    pub prompt: ApprovalPrompt,
    responder: oneshot::Sender<ApprovalDecision>,
}

impl ApprovalRequest {
    pub fn respond(self, decision: ApprovalDecision) {
        let _ = self.responder.send(decision);
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

    /// Ask if the policy requires it and wait for the answer.
    pub async fn check(&self, prompt: ApprovalPrompt) -> GateOutcome {
        if self.is_satisfied(
            &prompt.conversation_id,
            &prompt.capability_id,
            prompt.policy,
        ) {
            return GateOutcome::AutoAllowed;
        }
        let key = (prompt.conversation_id.clone(), prompt.capability_id.clone());
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

    fn prompt(policy: ApprovalPolicy) -> ApprovalPrompt {
        ApprovalPrompt {
            request_id: Id::new(),
            conversation_id: "c1".into(),
            capability_id: "mcp.fs.write_file".into(),
            title: "write_file".into(),
            description: String::new(),
            risk: RiskTier::LowWrite,
            policy,
            arguments: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn ask_once_remembers_grant() {
        let (gate, mut rx) = ApprovalGate::new(4);
        let ui = tokio::spawn(async move {
            let req = rx.recv().await.unwrap();
            assert_eq!(req.prompt.capability_id, "mcp.fs.write_file");
            req.respond(ApprovalDecision::ApproveForConversation);
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
