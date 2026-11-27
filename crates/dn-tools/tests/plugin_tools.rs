//! ADR-0013 §2 and §4: add-on tools run through the same executor path as MCP tools, and an
//! add-on's own calls carry a host-built context: their own approval scope, inherited
//! narrowing and a tier limit. Jobs never get an approval by themselves.

mod common;

use common::{call, ctx_in, executor};
use dn_agent::{BoxFuture, CancelToken, ToolContext, ToolExecutor};
use dn_core::Id;
use dn_core::capability::{ApprovalPolicy, ApprovalRequester, CapabilitySource, RiskTier};
use dn_tools::error::ToolError;
use dn_tools::{
    ApprovalDecision, CallContext, CapabilityOverride, InProcessTool, JobCall, McpCallOutput,
    Narrowed, PluginCall, PluginTool, ToolAllowlist,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Records every call and the context it ran with.
#[derive(Default)]
struct Recorder(Mutex<Vec<CallContext>>);

impl Recorder {
    fn calls(&self) -> Vec<CallContext> {
        self.0.lock().unwrap().clone()
    }
}

impl InProcessTool for Recorder {
    fn call<'a>(
        &'a self,
        ctx: &'a CallContext,
        _arguments: serde_json::Value,
    ) -> BoxFuture<'a, Result<McpCallOutput, ToolError>> {
        self.0.lock().unwrap().push(ctx.clone());
        Box::pin(async {
            Ok(McpCallOutput {
                content: serde_json::json!([{"type": "text", "text": "done"}]),
                structured_content: None,
                is_error: false,
            })
        })
    }
}

fn tool(name: &str, risk: RiskTier, floor: RiskTier, h: Arc<Recorder>) -> PluginTool {
    PluginTool {
        name: name.into(),
        title: name.into(),
        description: format!("{name} test tool"),
        input_schema: serde_json::json!({"type": "object"}),
        output_schema: None,
        risk,
        floor,
        idempotency_key_field: None,
        approval_required: false,
        handler: h,
    }
}

/// The message the user (and the model) sees for a refused call.
fn user_message(e: dn_core::CoreError) -> String {
    match e {
        dn_core::CoreError::Capability { user_message, .. } => user_message,
        other => panic!("{other:?}"),
    }
}

fn plugin_ctx(max_tier: RiskTier, job: Option<JobCall>) -> CallContext {
    CallContext::for_plugin(
        Id::new(),
        PluginCall {
            plugin_id: "notes".into(),
            name: "Notes".into(),
            max_tier,
            job,
        },
        Vec::new(),
    )
}

#[tokio::test]
async fn an_add_on_tool_is_offered_and_runs_through_the_executor() {
    let (ex, _rx) = executor();
    let h = Arc::new(Recorder::default());
    let caps = ex
        .add_plugin(
            "acme.notes",
            vec![tool("search", RiskTier::Read, RiskTier::Read, h.clone())],
        )
        .unwrap();
    assert_eq!(caps[0].spec.id, "plugin.acme.notes.search");
    assert_eq!(
        caps[0].spec.source,
        CapabilitySource::Plugin {
            plugin_id: "acme.notes".into()
        }
    );
    let names: Vec<String> = ex.tool_specs().into_iter().map(|t| t.name).collect();
    assert_eq!(names, ["plugin__acme_notes__search"]);
    let out = ex
        .execute(
            &ctx_in("conv", Id::new()),
            &call("c1", "plugin__acme_notes__search", serde_json::json!({})),
        )
        .await
        .unwrap();
    assert_eq!(out.output.text(), "done");
    assert_eq!(h.calls().len(), 1);
}

#[tokio::test]
async fn a_tool_below_its_floor_is_refused_and_a_user_override_cant_go_below_it() {
    let (ex, _rx) = executor();
    let h = Arc::new(Recorder::default());
    assert!(
        ex.add_plugin(
            "notes",
            vec![tool("w", RiskTier::Read, RiskTier::LowWrite, h.clone())]
        )
        .is_err()
    );
    ex.add_plugin(
        "notes",
        vec![tool("w", RiskTier::LowWrite, RiskTier::LowWrite, h)],
    )
    .unwrap();
    let mut ov = HashMap::new();
    ov.insert(
        "plugin.notes.w".to_owned(),
        CapabilityOverride {
            risk: Some(RiskTier::Read),
            ..Default::default()
        },
    );
    ex.set_overrides(ov);
    let cap = ex.resolve("plugin.notes.w").unwrap();
    assert_eq!(cap.spec.risk, RiskTier::LowWrite);
    assert_eq!(cap.approval, ApprovalPolicy::AskOncePerConversation);
    // Bad ids and repeated registrations are refused.
    assert!(ex.add_plugin("notes", vec![]).is_err());
    assert!(ex.add_plugin("No_tes", vec![]).is_err());
}

#[tokio::test]
async fn a_write_asks_and_the_prompt_names_who_asked() {
    let (ex, mut rx) = executor();
    let h = Arc::new(Recorder::default());
    ex.add_plugin(
        "notes",
        vec![tool(
            "save",
            RiskTier::LowWrite,
            RiskTier::LowWrite,
            h.clone(),
        )],
    )
    .unwrap();
    // From a chat: the model asked.
    let ex2 = ex.clone();
    let run = tokio::spawn(async move {
        ex2.execute(
            &ctx_in("conv", Id::new()),
            &call("c1", "plugin.notes.save", serde_json::json!({})),
        )
        .await
    });
    let req = rx.recv().await.unwrap();
    assert_eq!(req.prompt.requester, ApprovalRequester::Model);
    assert_eq!(req.prompt.scope, "conv:conv");
    req.respond(ApprovalDecision::Approve);
    run.await.unwrap().unwrap();

    // From the add-on itself: its own scope, and the prompt names it.
    let ex2 = ex.clone();
    let run = tokio::spawn(async move {
        ex2.execute(
            &plugin_ctx(RiskTier::LowWrite, None),
            &call("c2", "plugin.notes.save", serde_json::json!({})),
        )
        .await
    });
    let req = rx.recv().await.unwrap();
    assert!(
        req.prompt.scope.starts_with("plugin:notes:"),
        "{}",
        req.prompt.scope
    );
    assert_eq!(
        req.prompt.requester,
        ApprovalRequester::Plugin {
            plugin_id: "notes".into(),
            name: "Notes".into(),
            job: None
        }
    );
    req.respond(ApprovalDecision::Deny);
    let e = user_message(run.await.unwrap().unwrap_err());
    assert!(e.contains("declined"), "{e}");
    assert_eq!(h.calls().len(), 1);
}

#[tokio::test]
async fn a_chat_grant_is_never_shared_with_an_add_on_run() {
    let (ex, mut rx) = executor();
    let h = Arc::new(Recorder::default());
    ex.add_plugin(
        "notes",
        vec![tool("save", RiskTier::LowWrite, RiskTier::LowWrite, h)],
    )
    .unwrap();
    // "Allow for this chat" in a chat.
    let ex2 = ex.clone();
    let run = tokio::spawn(async move {
        ex2.execute(
            &ctx_in("conv", Id::new()),
            &call("c1", "plugin.notes.save", serde_json::json!({})),
        )
        .await
    });
    rx.recv()
        .await
        .unwrap()
        .respond(ApprovalDecision::ApproveForConversation);
    run.await.unwrap().unwrap();
    // The add-on's own call still asks.
    let ex2 = ex.clone();
    let run = tokio::spawn(async move {
        ex2.execute(
            &plugin_ctx(RiskTier::LowWrite, None),
            &call("c2", "plugin.notes.save", serde_json::json!({})),
        )
        .await
    });
    let req = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the add-on's call must ask")
        .unwrap();
    req.respond(ApprovalDecision::Approve);
    run.await.unwrap().unwrap();
}

#[tokio::test]
async fn an_add_on_cant_call_above_its_tier() {
    let (ex, mut rx) = executor();
    let h = Arc::new(Recorder::default());
    ex.add_plugin(
        "notes",
        vec![tool(
            "save",
            RiskTier::LowWrite,
            RiskTier::LowWrite,
            h.clone(),
        )],
    )
    .unwrap();
    let e = user_message(
        ex.execute(
            &plugin_ctx(RiskTier::Read, None),
            &call("c1", "plugin.notes.save", serde_json::json!({})),
        )
        .await
        .unwrap_err(),
    );
    assert!(e.contains("turned off"), "{e}");
    assert!(rx.try_recv().is_err(), "no prompt for a refused call");
    assert!(h.calls().is_empty());
}

#[tokio::test]
async fn a_jobs_call_is_held_unless_the_user_set_that_tool_to_run_without_asking() {
    let (ex, mut rx) = executor();
    let h = Arc::new(Recorder::default());
    ex.add_plugin(
        "notes",
        vec![
            tool("look", RiskTier::Read, RiskTier::Read, h.clone()),
            tool("save", RiskTier::LowWrite, RiskTier::LowWrite, h.clone()),
        ],
    )
    .unwrap();
    let job = |auto: &[&str]| JobCall {
        id: "daily".into(),
        name: "Daily brief".into(),
        auto_tools: auto.iter().map(|s| s.to_string()).collect(),
    };
    // A read tool runs without asking in a chat, but that default doesn't count for a job.
    let e = user_message(
        ex.execute(
            &plugin_ctx(RiskTier::Sensitive, Some(job(&[]))),
            &call("c1", "plugin.notes.look", serde_json::json!({})),
        )
        .await
        .unwrap_err(),
    );
    assert!(e.contains("waits for your answer"), "{e}");
    assert!(
        rx.try_recv().is_err(),
        "a held call never shows a prompt that times out"
    );
    assert!(h.calls().is_empty());
    // A tool the user set to run without asking for this add-on runs, even a write.
    ex.execute(
        &plugin_ctx(RiskTier::Sensitive, Some(job(&["plugin.notes.save"]))),
        &call("c2", "plugin.notes.save", serde_json::json!({})),
    )
    .await
    .unwrap();
    assert!(rx.try_recv().is_err());
    assert_eq!(h.calls().len(), 1);
}

#[tokio::test]
async fn narrowing_is_inherited_by_an_add_ons_handler_and_enforced_by_the_executor() {
    let (ex, _rx) = executor();
    let h = Arc::new(Recorder::default());
    ex.add_plugin(
        "notes",
        vec![tool("look", RiskTier::Read, RiskTier::Read, h.clone())],
    )
    .unwrap();
    // A chat whose skill allows the add-on's tools: the handler sees the skill's list.
    let inner: Arc<dyn ToolExecutor> = ex.clone();
    let narrowed = Narrowed::new(
        inner,
        ToolAllowlist::new(&["plugin.notes.*"]).unwrap(),
        ex.capability_resolver(),
    );
    let tctx = ToolContext {
        agent_id: "assistant.default".into(),
        caller: "chat.reply".into(),
        conversation_id: Some(Id::new()),
        run_id: Id::new(),
        step: 0,
        cancel: CancelToken::new(),
        allowed_tools: Vec::new(),
    };
    let out = narrowed
        .execute(
            &call("c1", "plugin__notes__look", serde_json::json!({})),
            &tctx,
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(
        h.calls()[0].allowed_tools,
        vec![vec!["plugin.notes.*".to_owned()]]
    );
    // A nested call made with that inherited list can't reach outside it, even if the
    // add-on itself may.
    let nested = CallContext::for_plugin(
        Id::new(),
        PluginCall {
            plugin_id: "notes".into(),
            name: "Notes".into(),
            max_tier: RiskTier::Read,
            job: None,
        },
        vec![vec!["mcp.files.*".to_owned()]],
    );
    let e = user_message(
        ex.execute(
            &nested,
            &call("c2", "plugin.notes.look", serde_json::json!({})),
        )
        .await
        .unwrap_err(),
    );
    assert!(e.contains("turned off"), "{e}");
    assert_eq!(h.calls().len(), 1);
}

#[tokio::test]
async fn a_write_replays_from_the_ledger_instead_of_running_twice() {
    let (ex, mut rx) = executor();
    let h = Arc::new(Recorder::default());
    ex.add_plugin(
        "notes",
        vec![tool(
            "save",
            RiskTier::LowWrite,
            RiskTier::LowWrite,
            h.clone(),
        )],
    )
    .unwrap();
    let ctx = ctx_in("conv", Id::new());
    let c = call("c1", "plugin.notes.save", serde_json::json!({"x": 1}));
    let ex2 = ex.clone();
    let (ctx2, c2) = (ctx.clone(), c.clone());
    let run = tokio::spawn(async move { ex2.execute(&ctx2, &c2).await });
    rx.recv().await.unwrap().respond(ApprovalDecision::Approve);
    assert!(!run.await.unwrap().unwrap().replayed);
    let again = ex.execute(&ctx, &c).await.unwrap();
    assert!(again.replayed);
    assert_eq!(h.calls().len(), 1);
}

#[tokio::test]
async fn removing_an_add_on_removes_its_tools_and_grants() {
    let (ex, mut rx) = executor();
    let h = Arc::new(Recorder::default());
    let reg = |ex: &dn_tools::Executor| {
        ex.add_plugin(
            "notes",
            vec![tool(
                "save",
                RiskTier::LowWrite,
                RiskTier::LowWrite,
                h.clone(),
            )],
        )
        .unwrap()
    };
    reg(&ex);
    let ex2 = ex.clone();
    let run = tokio::spawn(async move {
        ex2.execute(
            &ctx_in("conv", Id::new()),
            &call("c1", "plugin.notes.save", serde_json::json!({})),
        )
        .await
    });
    rx.recv()
        .await
        .unwrap()
        .respond(ApprovalDecision::ApproveForConversation);
    run.await.unwrap().unwrap();
    ex.remove_plugin("notes");
    assert!(ex.resolve("plugin.notes.save").is_none());
    let e = user_message(
        ex.execute(
            &ctx_in("conv", Id::new()),
            &call("c2", "plugin.notes.save", serde_json::json!({})),
        )
        .await
        .unwrap_err(),
    );
    assert!(e.contains("isn't available"), "{e}");
    // Registered again, the old "allow for this chat" grant is gone.
    reg(&ex);
    assert!(!ex.gate().is_satisfied(
        "conv:conv",
        "plugin.notes.save",
        ApprovalPolicy::AskOncePerConversation
    ));
}

#[tokio::test]
async fn a_call_approved_after_its_add_on_was_removed_doesnt_run() {
    let (ex, mut rx) = executor();
    let h = Arc::new(Recorder::default());
    ex.add_plugin(
        "notes",
        vec![tool(
            "save",
            RiskTier::LowWrite,
            RiskTier::LowWrite,
            h.clone(),
        )],
    )
    .unwrap();
    let ex2 = ex.clone();
    let run = tokio::spawn(async move {
        ex2.execute(
            &ctx_in("conv", Id::new()),
            &call("c1", "plugin.notes.save", serde_json::json!({})),
        )
        .await
    });
    let req = rx.recv().await.unwrap();
    // The add-on is turned off while the card is open; then the person approves.
    ex.remove_plugin("notes");
    req.respond(ApprovalDecision::Approve);
    assert!(run.await.unwrap().is_err());
    assert!(h.calls().is_empty());
}

#[tokio::test]
async fn overrides_and_registration_can_run_at_the_same_time() {
    let (ex, _rx) = executor();
    let h = Arc::new(Recorder::default());
    let tasks: Vec<_> = (0..20)
        .map(|i| {
            let ex = ex.clone();
            let h = h.clone();
            tokio::task::spawn_blocking(move || {
                if i % 2 == 0 {
                    ex.set_overrides(HashMap::new());
                } else {
                    let id = format!("p{i}");
                    ex.add_plugin(&id, vec![tool("t", RiskTier::Read, RiskTier::Read, h)])
                        .unwrap();
                }
            })
        })
        .collect();
    for t in tasks {
        tokio::time::timeout(Duration::from_secs(10), t)
            .await
            .expect("no deadlock")
            .unwrap();
    }
    assert_eq!(ex.capabilities().len(), 10);
}
