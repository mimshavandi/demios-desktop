//! ADR-0013 §9.4: tools that always need a fresh approval. No standing permission, a job's
//! call is always held, and the call runs only with a one-time approval for that exact
//! call, minted by Allow once or by a person's answer to the held call.

mod common;

use common::{call, ctx_in, executor};
use dn_agent::BoxFuture;
use dn_core::Id;
use dn_core::capability::{ApprovalPolicy, RiskTier};
use dn_jobs::JobKey;
use dn_jobs::held::{self, Answer, AnswerVerifier, HoldRequest};
use dn_plugin_api::PluginId;
use dn_tools::error::ToolError;
use dn_tools::{
    ApprovalDecision, CallContext, CapabilityOverride, Executor, InProcessTool, JobCall,
    McpCallOutput, PluginCall, PluginTool,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Records the arguments of every call that ran.
#[derive(Default)]
struct Sent(Mutex<Vec<serde_json::Value>>);

impl Sent {
    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

impl InProcessTool for Sent {
    fn call<'a>(
        &'a self,
        _ctx: &'a CallContext,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Result<McpCallOutput, ToolError>> {
        self.0.lock().unwrap().push(arguments);
        Box::pin(async {
            Ok(McpCallOutput {
                content: json!([{"type": "text", "text": "sent"}]),
                structured_content: None,
                is_error: false,
            })
        })
    }
}

const SEND: &str = "plugin.mail.send";

fn mail(ex: &Executor, sent: Arc<Sent>, required: bool) {
    ex.add_plugin(
        "mail",
        vec![PluginTool {
            name: "send".into(),
            title: "Send mail".into(),
            description: "Sends an email".into(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            risk: RiskTier::ExternalWrite,
            floor: RiskTier::Read,
            idempotency_key_field: None,
            approval_required: required,
            handler: sent,
        }],
    )
    .unwrap();
}

fn job_ctx(run: Id, auto_send: bool) -> CallContext {
    CallContext::for_plugin(
        run,
        PluginCall {
            plugin_id: "mail".into(),
            name: "Mail".into(),
            max_tier: RiskTier::Sensitive,
            job: Some(JobCall {
                id: "drafts".into(),
                name: "Drafts".into(),
                auto_tools: if auto_send {
                    vec![SEND.into()]
                } else {
                    Vec::new()
                },
            }),
        },
        Vec::new(),
    )
}

fn user_message(e: dn_core::CoreError) -> String {
    match e {
        dn_core::CoreError::Capability { user_message, .. } => user_message,
        other => panic!("{other:?}"),
    }
}

fn developer_message(e: dn_core::CoreError) -> String {
    match e {
        dn_core::CoreError::Capability {
            developer_message, ..
        } => developer_message,
        other => panic!("{other:?}"),
    }
}

/// Every standing choice the user could have stored.
fn standing(ex: &Executor, policy: ApprovalPolicy) {
    let mut ov = HashMap::new();
    ov.insert(
        SEND.to_owned(),
        CapabilityOverride {
            approval: Some(policy),
            ..Default::default()
        },
    );
    ex.set_overrides(ov);
}

#[tokio::test]
async fn a_stored_run_without_asking_is_ignored_and_shown_as_ignored() {
    let (ex, mut rx) = executor();
    let sent = Arc::new(Sent::default());
    mail(&ex, sent.clone(), true);
    for policy in [ApprovalPolicy::Auto, ApprovalPolicy::AskOncePerConversation] {
        standing(&ex, policy);
        let cap = ex.resolve(SEND).unwrap();
        assert!(cap.spec.approval_required);
        assert_eq!(cap.approval, ApprovalPolicy::AlwaysAsk);
        assert_eq!(cap.approval_ignored, Some(policy));
    }
    // It asks, with a card that offers Allow once only.
    let ex2 = ex.clone();
    let run = tokio::spawn(async move {
        ex2.execute(
            &ctx_in("c", Id::new()),
            &call("1", SEND, json!({"to": "a"})),
        )
        .await
    });
    let req = rx.recv().await.unwrap();
    assert!(req.prompt.approval_required);
    assert_eq!(req.prompt.policy, ApprovalPolicy::AlwaysAsk);
    req.respond(ApprovalDecision::Approve);
    run.await.unwrap().unwrap();
    assert_eq!(sent.count(), 1);
}

#[tokio::test]
async fn always_allow_is_refused_and_every_call_asks_again() {
    let (ex, mut rx) = executor();
    let sent = Arc::new(Sent::default());
    mail(&ex, sent.clone(), true);
    let run = Id::new();
    let ex2 = ex.clone();
    let first = tokio::spawn(async move {
        ex2.execute(&ctx_in("c", run), &call("1", SEND, json!({"to": "a"})))
            .await
    });
    rx.recv()
        .await
        .unwrap()
        .respond(ApprovalDecision::ApproveForConversation);
    let e = developer_message(first.await.unwrap().unwrap_err());
    assert!(e.contains("isn't accepted"), "{e}");
    assert_eq!(sent.count(), 0, "\"always allow\" doesn't run it");

    for id in ["2", "3"] {
        let ex2 = ex.clone();
        let id = id.to_owned();
        let next = tokio::spawn(async move {
            ex2.execute(&ctx_in("c", run), &call(&id, SEND, json!({"to": "a"})))
                .await
        });
        rx.recv()
            .await
            .expect("asks every time")
            .respond(ApprovalDecision::Approve);
        next.await.unwrap().unwrap();
    }
    assert_eq!(sent.count(), 2);
}

#[tokio::test]
async fn a_jobs_call_is_held_even_when_set_to_run_without_asking() {
    let (ex, mut rx) = executor();
    let sent = Arc::new(Sent::default());
    mail(&ex, sent.clone(), true);
    standing(&ex, ApprovalPolicy::Auto);
    let e = ex
        .execute(
            &job_ctx(Id::new(), true),
            &call("1", SEND, json!({"to": "a"})),
        )
        .await
        .unwrap_err();
    assert!(user_message(e).contains("waits for your answer"));
    assert!(rx.try_recv().is_err(), "held, never a card that times out");
    assert_eq!(sent.count(), 0);

    // The same tool without the mark: the user's choice for the add-on runs it.
    let (ex, _rx) = executor();
    let sent = Arc::new(Sent::default());
    mail(&ex, sent.clone(), false);
    ex.execute(
        &job_ctx(Id::new(), true),
        &call("1", SEND, json!({"to": "a"})),
    )
    .await
    .unwrap();
    assert_eq!(sent.count(), 1);
}

/// The desktop app's own window, the only answerer without a Node.
struct Window;

impl AnswerVerifier for Window {
    fn verify(&self, by: &str, _m: &[u8], _s: Option<&[u8]>) -> Result<(), String> {
        (by == "window")
            .then_some(())
            .ok_or_else(|| "not the window".into())
    }
}

fn jobs_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for m in dn_jobs::migrations() {
        conn.execute_batch(m).unwrap();
    }
    let key = JobKey::new(PluginId::new("mail").unwrap(), "drafts");
    dn_jobs::store::set_enabled(&conn, &key, true, 0).unwrap();
    conn
}

/// Holds the job's call, lets the window allow it, and hands it out.
fn hold_and_allow(conn: &rusqlite::Connection, run: Id, args: serde_json::Value) -> held::Approved {
    hold_and_allow_as(conn, run, SEND, args)
}

fn hold_and_allow_as(
    conn: &rusqlite::Connection,
    run: Id,
    tool: &str,
    args: serde_json::Value,
) -> held::Approved {
    let h = held::hold(
        conn,
        &HoldRequest {
            job: JobKey::new(PluginId::new("mail").unwrap(), "drafts"),
            run,
            tool: tool.into(),
            arguments: args,
            summary: "Send mail".into(),
        },
        None,
        10,
    )
    .unwrap();
    held::answer(
        conn,
        &Window,
        &Answer {
            approval: h.id,
            approve: true,
            arguments_sha256: h.arguments_sha256.clone(),
            answered_by: "window".into(),
            signature: None,
        },
        11,
    )
    .unwrap();
    held::take_approved(conn, h.id).unwrap()
}

#[tokio::test]
async fn a_held_call_runs_once_with_the_answer_as_its_one_time_approval() {
    let (ex, mut rx) = executor();
    let sent = Arc::new(Sent::default());
    mail(&ex, sent.clone(), true);
    let conn = jobs_db();
    let run = Id::new();
    let approved = hold_and_allow(&conn, run, json!({"to": "a", "body": "hi"}));
    let (held, tc, token) = ex.approval_for_held(approved).unwrap();
    assert_eq!(tc.id, held.id.to_string());
    let ctx = job_ctx(run, false).with_approval(token);
    ex.execute(&ctx, &tc).await.unwrap();
    assert_eq!(sent.count(), 1);
    assert!(rx.try_recv().is_err(), "the answer was the approval");

    // The same token again: used up.
    let again = ex
        .execute(&ctx, &call("other", SEND, tc.arguments.clone()))
        .await;
    assert!(again.is_err());
    assert_eq!(sent.count(), 1);
}

#[tokio::test]
async fn a_token_is_refused_for_other_arguments_another_call_or_after_withdrawal() {
    let (ex, _rx) = executor();
    let sent = Arc::new(Sent::default());
    mail(&ex, sent.clone(), true);
    let conn = jobs_db();
    let run = Id::new();

    // Other arguments than the person allowed.
    let (_, tc, token) = ex
        .approval_for_held(hold_and_allow(&conn, run, json!({"to": "a"})))
        .unwrap();
    let mut changed = tc.clone();
    changed.arguments = json!({"to": "attacker"});
    let e = ex
        .execute(&job_ctx(run, false).with_approval(token.clone()), &changed)
        .await
        .unwrap_err();
    assert!(developer_message(e).contains("another call or other arguments"));
    // A mismatch used it up: the right call can't use it any more either.
    assert!(
        ex.execute(&job_ctx(run, false).with_approval(token), &tc)
            .await
            .is_err()
    );

    // Another run (another call with the same id and arguments).
    let (_, tc, token) = ex
        .approval_for_held(hold_and_allow(&conn, run, json!({"to": "b"})))
        .unwrap();
    assert!(
        ex.execute(&job_ctx(Id::new(), false).with_approval(token), &tc)
            .await
            .is_err()
    );

    // Withdrawn after it was minted.
    let (held, tc, token) = ex
        .approval_for_held(hold_and_allow(&conn, run, json!({"to": "c"})))
        .unwrap();
    ex.revoke_held_approval(run, held.id);
    let e = ex
        .execute(&job_ctx(run, false).with_approval(token), &tc)
        .await
        .unwrap_err();
    assert!(developer_message(e).contains("no unused approval"));

    // "Delete all local data" forgets every token.
    let (_, tc, token) = ex
        .approval_for_held(hold_and_allow(&conn, run, json!({"to": "d"})))
        .unwrap();
    ex.forget_all().unwrap();
    assert!(
        ex.execute(&job_ctx(run, false).with_approval(token), &tc)
            .await
            .is_err()
    );
    assert_eq!(sent.count(), 0);
}

#[tokio::test]
async fn a_chat_or_an_add_on_handler_cant_borrow_a_held_calls_token() {
    let (ex, mut rx) = executor();
    let sent = Arc::new(Sent::default());
    mail(&ex, sent.clone(), true);
    let conn = jobs_db();
    let run = Id::new();
    let (_, tc, token) = ex
        .approval_for_held(hold_and_allow(&conn, run, json!({"to": "a"})))
        .unwrap();
    // A chat call carrying it, for another call id: refused; it never asks instead.
    let ctx = ctx_in("c", run).with_approval(token);
    let e = ex
        .execute(&ctx, &call("x", SEND, tc.arguments.clone()))
        .await
        .unwrap_err();
    assert!(developer_message(e).contains("another call or other arguments"));
    assert!(rx.try_recv().is_err());
    assert_eq!(sent.count(), 0);
}

#[tokio::test]
async fn the_users_mark_adds_to_an_unmarked_tool_and_cant_remove_the_add_ons() {
    let (ex, _rx) = executor();
    let sent = Arc::new(Sent::default());
    mail(&ex, sent.clone(), false);
    let mut ov = HashMap::new();
    ov.insert(
        SEND.to_owned(),
        CapabilityOverride {
            approval_required: true,
            ..Default::default()
        },
    );
    ex.set_overrides(ov);
    assert!(ex.resolve(SEND).unwrap().spec.approval_required);

    let (ex, _rx) = executor();
    mail(&ex, sent, true);
    ex.set_overrides(HashMap::from([(
        SEND.to_owned(),
        CapabilityOverride::default(),
    )]));
    assert!(ex.resolve(SEND).unwrap().spec.approval_required);
}

#[tokio::test]
async fn a_call_held_under_the_models_tool_name_runs_with_its_approval() {
    let (ex, _rx) = executor();
    let sent = Arc::new(Sent::default());
    mail(&ex, sent.clone(), true);
    let conn = jobs_db();
    let run = Id::new();
    let llm_name = ex.resolve(SEND).unwrap().llm_name;
    assert_ne!(llm_name, SEND);
    let approved = hold_and_allow_as(&conn, run, &llm_name, json!({"to": "a"}));
    let (_, tc, token) = ex.approval_for_held(approved).unwrap();
    ex.execute(&job_ctx(run, false).with_approval(token), &tc)
        .await
        .unwrap();
    assert_eq!(sent.count(), 1);
}
