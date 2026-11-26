//! The registry against a real app state (database, executor, jobs connection) and small
//! test add-ons: loading and refusals, consent and revocation, the tool path, panics, jobs'
//! held calls, and "Delete all".

use super::*;
use dn_plugin_api::{
    BoxFuture, ErrorKind, Host, Id, JobDecl, PermissionRequest, PluginError, PluginId,
    PluginManifest, Registrar, RiskTier, Schedule, ToolDecl, ToolHandler, ToolInput, ToolResult,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, PartialEq)]
enum Does {
    ListChats,
    Panic,
    CallMailFromJob,
    Nothing,
}

struct Tool(Does, Arc<AtomicUsize>);

impl ToolHandler for Tool {
    fn call<'a>(
        &'a self,
        host: &'a Host,
        _input: ToolInput,
    ) -> BoxFuture<'a, Result<ToolResult, PluginError>> {
        Box::pin(async move {
            self.1.fetch_add(1, Ordering::SeqCst);
            match self.0 {
                Does::ListChats => {
                    let n = host.chats().list().await?.len();
                    Ok(ToolResult::text(format!("{n} chats")))
                }
                Does::Panic => panic!("boom"),
                _ => Ok(ToolResult::text("done")),
            }
        })
    }
}

struct Job;

impl dn_plugin_api::JobHandler for Job {
    fn run<'a>(
        &'a self,
        host: &'a Host,
        _ctx: &'a dn_plugin_api::JobContext,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(async move {
            host.tools()
                .call("plugin.notes.save", serde_json::json!({"x": 1}))
                .await?;
            Ok(())
        })
    }
}

struct TestAddon {
    m: PluginManifest,
    does: Does,
    calls: Arc<AtomicUsize>,
    deleted: Arc<AtomicUsize>,
    panic_in_register: bool,
}

impl TestAddon {
    fn new(id: &str, perms: Vec<PermissionRequest>, does: Does) -> Self {
        let mut m =
            PluginManifest::new(PluginId::new(id).unwrap(), format!("Add-on {id}"), "1.0.0");
        for p in perms {
            m = m.with_permission(p);
        }
        Self {
            m,
            does,
            calls: Arc::default(),
            deleted: Arc::default(),
            panic_in_register: false,
        }
    }
}

impl Plugin for TestAddon {
    fn manifest(&self) -> &PluginManifest {
        &self.m
    }
    fn register(&self, r: &mut Registrar) -> Result<(), PluginError> {
        if self.panic_in_register {
            panic!("register failed");
        }
        r.tool(
            ToolDecl::new(
                "look",
                "Look",
                "Looks at things",
                serde_json::json!({"type": "object"}),
                r.risk_floor(),
            ),
            Arc::new(Tool(self.does, Arc::clone(&self.calls))),
        )?;
        r.tool(
            ToolDecl::new(
                "save",
                "Save",
                "Saves things",
                serde_json::json!({"type": "object"}),
                RiskTier::LowWrite.max(r.risk_floor()),
            ),
            Arc::new(Tool(Does::Nothing, Arc::clone(&self.calls))),
        )?;
        if self.does == Does::CallMailFromJob {
            r.job(
                JobDecl::new("daily", "Daily", Schedule::Every { secs: 3600 }, 600, 60),
                Arc::new(Job),
            )?;
        }
        Ok(())
    }
    fn forget_conversation<'a>(
        &'a self,
        _: &'a Host,
        _: Id,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(async { Ok(()) })
    }
    fn delete_all<'a>(&'a self, host: &'a Host) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(async move {
            self.deleted.fetch_add(1, Ordering::SeqCst);
            // Deleting is allowed; writing is not.
            host.settings().remove("tone").await?;
            assert_eq!(
                host.settings()
                    .set("tone", serde_json::json!(1))
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::TierLimit
            );
            Ok(())
        })
    }
}

fn with_state(f: impl AsyncFnOnce(Arc<AppState>)) {
    let dir = std::env::temp_dir().join(format!("dn-addons-{}", Id::new()));
    let path = dir.join("db.sqlite3");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let st = AppState::open_for_tests(&path).unwrap();
        f(st).await;
    });
    let _ = std::fs::remove_dir_all(dir);
}

fn summary_of(st: &AppState, id: &str) -> AddonSummary {
    st.addons
        .list(st)
        .unwrap()
        .into_iter()
        .find(|a| a.id == id)
        .unwrap()
}

async fn run_tool(st: &AppState, name: &str) -> Result<dn_tools::ToolOutcome, dn_core::CoreError> {
    let exec = st.executor.clone().unwrap();
    exec.execute(
        &dn_tools::CallContext::new(Some("conv".into()), Id::new()),
        &dn_core::chat::ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: serde_json::json!({}),
        },
    )
    .await
}

#[test]
fn the_regular_build_has_no_add_ons() {
    with_state(async |st| {
        Addons::install(&st, Vec::new());
        assert!(!st.addons.any());
        assert!(st.addons.list(&st).unwrap().is_empty());
    });
}

#[test]
fn an_add_on_loads_its_tools_into_the_executor_and_asks_for_consent() {
    with_state(async |st| {
        let a = TestAddon::new("notes", vec![PermissionRequest::ChatsRead], Does::ListChats);
        let calls = Arc::clone(&a.calls);
        Addons::install(&st, vec![Arc::new(a)]);
        let s = summary_of(&st, "notes");
        assert_eq!(s.state, "running");
        assert!(s.needs_consent);
        let pending = st.addons.pending_consent(&st).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].permissions[0].permission, "chats.read");
        let exec = st.executor.clone().unwrap();
        let cap = exec.resolve("plugin.notes.look").unwrap();
        assert_eq!(
            cap.spec.source,
            dn_core::capability::CapabilitySource::Plugin {
                plugin_id: "notes".into()
            }
        );
        // Not allowed yet: the handler runs, and its chats call fails clearly.
        let e = run_tool(&st, "plugin.notes.look").await.unwrap_err();
        assert!(e.to_string().contains("chats.read"), "{e}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn consent_applies_at_once_and_turning_a_permission_off_bites_on_the_next_call() {
    with_state(async |st| {
        let a = TestAddon::new("notes", vec![PermissionRequest::ChatsRead], Does::ListChats);
        Addons::install(&st, vec![Arc::new(a)]);
        st.addons
            .consent(
                &st,
                "notes",
                &["chats.read"],
                ConsentAnswer {
                    allowed: vec!["chats.read".into()],
                    chats_all: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(st.addons.pending_consent(&st).unwrap().is_empty());
        st.create_conversation_in(None).unwrap();
        let out = run_tool(&st, "plugin.notes.look").await.unwrap();
        assert_eq!(out.output.text(), "1 chats");
        st.addons
            .set_permission(&st, "notes", "chats.read", false)
            .unwrap();
        let e = run_tool(&st, "plugin.notes.look").await.unwrap_err();
        assert!(e.to_string().contains("turned off"), "{e}");
        // Consent can't grant something the add-on didn't ask for.
        assert!(
            st.addons
                .consent(
                    &st,
                    "notes",
                    &["notify"],
                    ConsentAnswer {
                        allowed: vec!["notify".into()],
                        ..Default::default()
                    }
                )
                .is_err()
        );
    });
}

#[test]
fn overlapping_ids_incompatible_versions_and_failing_registers_are_not_loaded() {
    with_state(async |st| {
        let a = TestAddon::new("acme", vec![], Does::Nothing);
        let b = TestAddon::new("acme.notes", vec![], Does::Nothing);
        let mut old = TestAddon::new("old", vec![], Does::Nothing);
        old.m.api_major = dn_plugin_api::API_MAJOR + 1;
        let mut bad = TestAddon::new("bad", vec![], Does::Nothing);
        bad.panic_in_register = true;
        let good = TestAddon::new("good", vec![], Does::Nothing);
        Addons::install(
            &st,
            vec![
                Arc::new(a),
                Arc::new(b),
                Arc::new(old),
                Arc::new(bad),
                Arc::new(good),
            ],
        );
        for id in ["acme", "acme.notes", "old", "bad"] {
            let s = summary_of(&st, id);
            assert_eq!(s.state, "not_running", "{id}");
            assert!(s.reason.is_some(), "{id}");
            assert!(!s.needs_consent, "{id}");
        }
        assert!(summary_of(&st, "acme").reason.unwrap().contains("overlaps"));
        assert!(
            summary_of(&st, "old")
                .reason
                .unwrap()
                .contains("update Demios")
        );
        assert_eq!(summary_of(&st, "good").state, "running");
        let exec = st.executor.clone().unwrap();
        assert!(exec.resolve("plugin.acme.look").is_none());
        assert!(exec.resolve("plugin.good.look").is_some());
    });
}

#[test]
fn turning_an_add_on_off_takes_its_tools_away_and_a_panic_stops_it_until_restart() {
    with_state(async |st| {
        let quiet = TestAddon::new("quiet", vec![], Does::Nothing);
        let wild = TestAddon::new("wild", vec![], Does::Panic);
        Addons::install(&st, vec![Arc::new(quiet), Arc::new(wild)]);
        let exec = st.executor.clone().unwrap();
        st.addons.set_enabled(&st, "quiet", false).unwrap();
        assert!(exec.resolve("plugin.quiet.look").is_none());
        assert_eq!(summary_of(&st, "quiet").state, "off");
        st.addons.set_enabled(&st, "quiet", true).unwrap();
        assert!(exec.resolve("plugin.quiet.look").is_some());

        let e = run_tool(&st, "plugin.wild.look").await.unwrap_err();
        assert!(e.to_string().contains("panicked"), "{e}");
        let s = summary_of(&st, "wild");
        assert_eq!(s.state, "not_running");
        assert!(s.reason.unwrap().contains("until Demios restarts"));
        assert!(exec.resolve("plugin.wild.look").is_none());
    });
}

#[test]
fn a_jobs_tool_call_is_held_and_runs_once_after_the_person_allows_it() {
    with_state(async |st| {
        let a = TestAddon::new(
            "notes",
            vec![PermissionRequest::Jobs],
            Does::CallMailFromJob,
        );
        let calls = Arc::clone(&a.calls);
        let manifest = a.m.clone();
        Addons::install(&st, vec![Arc::new(a)]);
        let grants = st.addons.loaded()[0].1.clone();
        let host = Addons::job_host(&st, &manifest, grants, "daily", Id::new());
        let e = host
            .tools()
            .call("plugin.notes.save", serde_json::json!({"x": 1}))
            .await
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Held);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "held, not run");
        let d = st.addons.detail(&st, "notes").unwrap();
        assert_eq!(d.held.len(), 1);
        assert_eq!(d.summary.waiting, 1);
        let h = d.held[0].clone();
        // An answer for other arguments is refused.
        assert!(
            st.addons
                .jobs
                .answer_held(&st, h.id, true, "0".repeat(64))
                .await
                .is_err()
        );
        // Turned off meanwhile: allowing it doesn't run it.
        st.addons.set_enabled(&st, "notes", false).unwrap();
        let key = dn_jobs::JobKey::new(manifest.id.clone(), "daily");
        st.addons.jobs.set_enabled(&st, &key, true).unwrap();
        let e = st
            .addons
            .jobs
            .answer_held(&st, h.id, true, h.arguments_sha256.clone())
            .await
            .unwrap_err();
        assert!(e.contains("is off"), "{e}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        st.addons.set_enabled(&st, "notes", true).unwrap();
        // A fresh held call, allowed while everything is on, runs once.
        let host = Addons::job_host(
            &st,
            &manifest,
            st.addons.loaded()[0].1.clone(),
            "daily",
            Id::new(),
        );
        let _ = host
            .tools()
            .call("plugin.notes.save", serde_json::json!({"x": 1}))
            .await;
        let h = st.addons.detail(&st, "notes").unwrap().held[0].clone();
        st.addons.jobs.set_enabled(&st, &key, true).unwrap();
        st.addons
            .jobs
            .answer_held(&st, h.id, true, h.arguments_sha256.clone())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Answered once: a second answer is refused and nothing runs again.
        assert!(
            st.addons
                .jobs
                .answer_held(&st, h.id, true, h.arguments_sha256)
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(st.addons.detail(&st, "notes").unwrap().held.is_empty());
    });
}

#[test]
fn delete_all_revokes_first_lets_the_add_on_delete_and_then_wipes_the_rest() {
    with_state(async |st| {
        let a = TestAddon::new("notes", vec![PermissionRequest::ChatsRead], Does::Nothing);
        let deleted = Arc::clone(&a.deleted);
        let manifest = a.m.clone();
        Addons::install(&st, vec![Arc::new(a)]);
        st.addons
            .consent(
                &st,
                "notes",
                &["chats.read"],
                ConsentAnswer {
                    allowed: vec!["chats.read".into()],
                    ..Default::default()
                },
            )
            .unwrap();
        let grants = st.addons.loaded()[0].1.clone();
        let old = Addons::job_host(&st, &manifest, grants, "daily", Id::new());
        old.settings()
            .set("tone", serde_json::json!("short"))
            .await
            .unwrap();
        let failed = st.delete_all_local_data().await;
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(deleted.load(Ordering::SeqCst), 1);
        // Every Host handed out earlier is revoked.
        assert_eq!(
            old.settings().get("tone").await.unwrap_err().kind,
            ErrorKind::Revoked
        );
        let db = st.db().unwrap();
        assert!(store::known_plugins(&db).unwrap().is_empty());
        assert_eq!(store::setting_get(&db, &manifest.id, "tone").unwrap(), None);
        drop(db);
        let s = summary_of(&st, "notes");
        assert_eq!(s.state, "not_running");
        assert!(
            st.executor
                .clone()
                .unwrap()
                .resolve("plugin.notes.look")
                .is_none()
        );
    });
}

#[test]
fn raising_a_tools_tier_makes_it_ask_more_but_never_widens_its_handlers_access() {
    struct Writes;
    impl ToolHandler for Writes {
        fn call<'a>(
            &'a self,
            host: &'a Host,
            _input: ToolInput,
        ) -> BoxFuture<'a, Result<ToolResult, PluginError>> {
            Box::pin(async move {
                host.settings().set("x", serde_json::json!(1)).await?;
                Ok(ToolResult::text("wrote"))
            })
        }
    }
    struct ReadTool(PluginManifest);
    impl Plugin for ReadTool {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn register(&self, r: &mut Registrar) -> Result<(), PluginError> {
            r.tool(
                ToolDecl::new(
                    "peek",
                    "Peek",
                    "Reads",
                    serde_json::json!({"type": "object"}),
                    RiskTier::Read,
                ),
                Arc::new(Writes),
            )?;
            Ok(())
        }
        fn forget_conversation<'a>(
            &'a self,
            _: &'a Host,
            _: Id,
        ) -> BoxFuture<'a, Result<(), PluginError>> {
            Box::pin(async { Ok(()) })
        }
        fn delete_all<'a>(&'a self, _: &'a Host) -> BoxFuture<'a, Result<(), PluginError>> {
            Box::pin(async { Ok(()) })
        }
    }
    with_state(async |st| {
        let m = PluginManifest::new(PluginId::new("peek").unwrap(), "Peek", "1.0.0");
        Addons::install(&st, vec![Arc::new(ReadTool(m))]);
        let exec = st.executor.clone().unwrap();
        // The person raises the tool to "always ask" at a write tier.
        let mut ov = std::collections::HashMap::new();
        ov.insert(
            "plugin.peek.peek".to_owned(),
            dn_tools::CapabilityOverride {
                risk: Some(RiskTier::ExternalWrite),
                approval: Some(dn_core::capability::ApprovalPolicy::Auto),
                ..Default::default()
            },
        );
        exec.set_overrides(ov);
        assert_eq!(
            exec.resolve("plugin.peek.peek").unwrap().spec.risk,
            RiskTier::ExternalWrite
        );
        // Its handler is still read-only: the write is refused.
        let e = run_tool(&st, "plugin.peek.peek").await.unwrap_err();
        assert!(e.to_string().contains("can only read"), "{e}");
    });
}
