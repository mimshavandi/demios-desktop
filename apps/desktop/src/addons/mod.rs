//! Add-ons in the desktop app (ADR-0013 action item 3): the registry, the ownership table,
//! permissions and consent, and what Settings › Add-ons shows.
//!
//! The regular build links no add-ons: [`crate::run`] passes none, the registry stays
//! empty, and Settings › Add-ons doesn't appear. An add-on build passes its list to
//! [`crate::run_with_addons`].
//!
//! - **Loading** ([`Addons::install`]): each add-on's manifest is checked (API major, ids,
//!   permissions), ids that overlap are refused for both add-ons, `register` runs with a
//!   panic caught, and every tool and job is claimed in the ownership table. An add-on that
//!   fails any step isn't loaded, and Settings says why.
//! - **Consent**: an add-on runs whether or not the person answered; whatever wasn't
//!   allowed stays off and fails clearly when used. A new version that asks for more asks
//!   only for the new permissions.
//! - **Tools** go to the executor through [`backend::ToolBridge`], so they take the same
//!   path as tool-server tools. A handler that panics turns its add-on off until restart.
//! - **Jobs** run through `dn-jobs`' scheduler ([`jobs`]), which starts once the window has
//!   told the app its local time offset.

pub mod backend;
pub mod commands;
pub mod jobs;
pub mod store;

use crate::state::AppState;
use dn_plugin_api::{
    Consent, Grants, GrantsHandle, Permission, Plugin, PluginError, PluginId, PluginManifest,
    Registrar, Registrations,
};
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, Weak};

/// Why an add-on isn't running, in the person's words (AddonsSettings, "Why an add-on
/// isn't running").
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotRunning {
    /// Built for another interface version.
    Incompatible { message: String },
    /// Its manifest or `register` failed, or a claim was refused.
    FailedToStart { message: String },
    /// A handler panicked; off until restart.
    Stopped { message: String },
    /// "Delete all local data" ran; it starts again with the next launch.
    DataDeleted,
}

impl NotRunning {
    pub fn message(&self, name: &str) -> String {
        match self {
            Self::Incompatible { message } => message.clone(),
            Self::FailedToStart { message } => format!("{name} couldn't start: {message}"),
            Self::Stopped { .. } => {
                format!("{name} stopped after an error and is off until Demios restarts.")
            }
            Self::DataDeleted => {
                format!("{name}'s data was deleted. It starts again when Demios restarts.")
            }
        }
    }
}

/// One add-on as the registry holds it.
pub(crate) struct Entry {
    pub plugin: Arc<dyn Plugin>,
    pub manifest: PluginManifest,
    pub grants: GrantsHandle,
    pub registrations: Option<Registrations>,
    pub not_running: Option<NotRunning>,
}

impl Entry {
    fn loaded(&self) -> bool {
        self.not_running.is_none() && self.registrations.is_some()
    }
}

/// The add-ons of this build.
#[derive(Default)]
pub struct Addons {
    entries: Mutex<Vec<Entry>>,
    pub(crate) jobs: jobs::Jobs,
    state: Mutex<Weak<AppState>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Grants from what was stored, limited to what this version asks for (a permission the
/// add-on no longer asks for is dropped, never carried over).
fn grants_for(manifest: &PluginManifest, st: &store::AddonState) -> Grants {
    let allowed: Vec<Permission> = st
        .allowed
        .iter()
        .copied()
        .filter(|p| manifest.asks_for(*p))
        .collect();
    let consent = Consent::new(allowed)
        .with_chats(st.chats.clone())
        .with_knowledge_projects(st.knowledge.clone());
    Grants::new(manifest, consent).unwrap_or_else(|_| Grants::none())
}

/// Runs `register` with a panic caught: a panicking add-on isn't loaded.
fn register(plugin: &Arc<dyn Plugin>, manifest: &PluginManifest) -> Result<Registrations, String> {
    let mut r = Registrar::new(manifest);
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| plugin.register(&mut r)));
    match outcome {
        Ok(Ok(())) => Ok(r.finish()),
        Ok(Err(e)) => Err(e.message),
        Err(_) => Err("it stopped with an internal error while starting".into()),
    }
}

impl Addons {
    /// True when this build has add-ons (Settings › Add-ons is shown).
    pub fn any(&self) -> bool {
        !lock(&self.entries).is_empty()
    }

    fn app(&self) -> Option<Arc<AppState>> {
        lock(&self.state).upgrade()
    }

    /// Loads `plugins` into the registry. Called once, after the app state is open.
    pub fn install(state: &Arc<AppState>, plugins: Vec<Arc<dyn Plugin>>) {
        let me = &state.addons;
        *lock(&me.state) = Arc::downgrade(state);
        let mut entries: Vec<Entry> = plugins
            .into_iter()
            .map(|plugin| {
                let manifest = plugin.manifest().clone();
                let not_running = manifest.validate().err().map(|e| match e.kind {
                    dn_plugin_api::ErrorKind::Incompatible => {
                        NotRunning::Incompatible { message: e.message }
                    }
                    _ => NotRunning::FailedToStart { message: e.message },
                });
                Entry {
                    plugin,
                    manifest,
                    grants: GrantsHandle::new(Grants::none()),
                    registrations: None,
                    not_running,
                }
            })
            .collect();
        // Overlapping ids: refuse both, so neither can look like the other.
        for i in 0..entries.len() {
            for j in i + 1..entries.len() {
                if entries[i]
                    .manifest
                    .id
                    .conflicts_with(&entries[j].manifest.id)
                {
                    let msg = format!(
                        "its id overlaps with the add-on `{}`",
                        entries[j].manifest.id
                    );
                    let other = format!(
                        "its id overlaps with the add-on `{}`",
                        entries[i].manifest.id
                    );
                    entries[i]
                        .not_running
                        .get_or_insert(NotRunning::FailedToStart { message: msg });
                    entries[j]
                        .not_running
                        .get_or_insert(NotRunning::FailedToStart { message: other });
                }
            }
        }
        let db = state.db().ok();
        for e in &mut entries {
            if e.not_running.is_some() {
                continue;
            }
            let regs = match register(&e.plugin, &e.manifest) {
                Ok(r) => r,
                Err(message) => {
                    e.not_running = Some(NotRunning::FailedToStart { message });
                    continue;
                }
            };
            let Some(db) = db.as_deref() else {
                e.not_running = Some(NotRunning::FailedToStart {
                    message: "the local database is unavailable".into(),
                });
                continue;
            };
            let claims = regs
                .tools
                .iter()
                .map(|t| (store::Owned::Tool, t.capability_id.clone()))
                .chain(regs.jobs.iter().map(|j| {
                    (
                        store::Owned::Job,
                        format!("{}/{}", e.manifest.id, j.decl.id),
                    )
                }));
            let mut refused = None;
            for (kind, key) in claims {
                if let Err(m) = store::claim(db, kind, &key, &e.manifest.id) {
                    refused = Some(m);
                    break;
                }
            }
            if let Some(message) = refused {
                e.not_running = Some(NotRunning::FailedToStart { message });
                continue;
            }
            match store::state(db, &e.manifest.id) {
                Ok(st) => e.grants = GrantsHandle::new(grants_for(&e.manifest, &st)),
                Err(err) => {
                    e.not_running = Some(NotRunning::FailedToStart {
                        message: format!("its saved choices can't be read ({err})"),
                    });
                    continue;
                }
            }
            e.registrations = Some(regs);
        }
        drop(db);
        *lock(&me.entries) = entries;
        me.sync_tools(state);
        me.jobs.rebuild(state);
    }

    /// Puts the tools of every loaded, enabled add-on into the executor, and takes out the
    /// rest.
    fn sync_tools(&self, state: &Arc<AppState>) {
        let Some(exec) = state.executor.clone() else {
            return;
        };
        let enabled = |id: &PluginId| {
            state
                .db()
                .ok()
                .and_then(|db| store::state(&db, id).ok())
                .is_some_and(|s| s.enabled)
        };
        let entries = lock(&self.entries);
        for e in entries.iter() {
            exec.remove_plugin(e.manifest.id.as_str());
            if !e.loaded() || !enabled(&e.manifest.id) {
                continue;
            }
            let Some(regs) = &e.registrations else {
                continue;
            };
            let floor = dn_plugin_api::risk_floor(&e.manifest);
            let tools = regs
                .tools
                .iter()
                .map(|t| dn_tools::PluginTool {
                    name: t.decl.name.clone(),
                    title: t.decl.title.clone(),
                    description: t.decl.description.clone(),
                    input_schema: t.decl.input_schema.clone(),
                    output_schema: t.decl.output_schema.clone(),
                    risk: t.decl.risk,
                    floor,
                    idempotency_key_field: t.decl.idempotency_key_field.clone(),
                    handler: Arc::new(backend::ToolBridge::new(
                        state,
                        e.manifest.clone(),
                        t.capability_id.clone(),
                        Arc::clone(&t.handler),
                        e.grants.clone(),
                    )),
                })
                .collect();
            if let Err(err) = exec.add_plugin(e.manifest.id.as_str(), tools) {
                eprintln!("could not add the tools of {}: {err}", e.manifest.id);
            }
        }
    }

    /// Turns an add-on off until restart after its handler panicked.
    pub(crate) fn stop_after_panic(&self, plugin: &PluginId) {
        {
            let mut entries = lock(&self.entries);
            if let Some(e) = entries.iter_mut().find(|e| &e.manifest.id == plugin) {
                e.not_running = Some(NotRunning::Stopped {
                    message: "a handler panicked".into(),
                });
                e.grants.close();
            }
        }
        if let Some(state) = self.app() {
            if let Some(exec) = &state.executor {
                exec.remove_plugin(plugin.as_str());
            }
            self.jobs.rebuild(&state);
        }
    }

    /// Calls `f` with the entry of `plugin`.
    pub(crate) fn with_entry<T>(
        &self,
        plugin: &PluginId,
        f: impl FnOnce(&Entry) -> T,
    ) -> Option<T> {
        lock(&self.entries)
            .iter()
            .find(|e| &e.manifest.id == plugin)
            .map(f)
    }

    /// Every loaded add-on's id, manifest, grants and registrations.
    pub(crate) fn loaded(&self) -> Vec<(PluginManifest, GrantsHandle, Registrations)> {
        lock(&self.entries)
            .iter()
            .filter(|e| e.loaded())
            .filter_map(|e| {
                e.registrations
                    .clone()
                    .map(|r| (e.manifest.clone(), e.grants.clone(), r))
            })
            .collect()
    }

    fn find(&self, id: &str) -> Result<(PluginManifest, GrantsHandle, bool), String> {
        lock(&self.entries)
            .iter()
            .find(|e| e.manifest.id.as_str() == id)
            .map(|e| (e.manifest.clone(), e.grants.clone(), e.loaded()))
            .ok_or_else(|| format!("There is no add-on `{id}` in this copy of Demios."))
    }

    /// Settings › Add-ons: the list.
    pub fn list(&self, state: &AppState) -> Result<Vec<AddonSummary>, String> {
        let db = state.db().map_err(|e| e.message)?;
        let waiting = jobs::waiting_counts(state);
        let entries = lock(&self.entries);
        let mut v = Vec::new();
        for e in entries.iter() {
            let st = store::state(&db, &e.manifest.id).map_err(|e| e.to_string())?;
            v.push(summary(
                e,
                &st,
                waiting.get(e.manifest.id.as_str()).copied().unwrap_or(0),
            ));
        }
        v.sort_by_key(|a| a.name.to_lowercase());
        Ok(v)
    }

    /// Settings › Add-ons: one add-on's page.
    pub fn detail(&self, state: &AppState, id: &str) -> Result<AddonDetail, String> {
        let db = state.db().map_err(|e| e.message)?;
        let waiting = jobs::waiting_counts(state);
        let entries = lock(&self.entries);
        let e = entries
            .iter()
            .find(|e| e.manifest.id.as_str() == id)
            .ok_or_else(|| format!("There is no add-on `{id}` in this copy of Demios."))?;
        let st = store::state(&db, &e.manifest.id).map_err(|e| e.to_string())?;
        drop(db);
        let permissions = e
            .manifest
            .permissions
            .iter()
            .map(|r| {
                let p = r.permission();
                PermissionView {
                    permission: p.as_str(),
                    asked: st.asked.contains(&p),
                    allowed: st.allowed.contains(&p),
                    tools: match r {
                        dn_plugin_api::PermissionRequest::ToolsCall { tools } => tools.clone(),
                        _ => Vec::new(),
                    },
                    hosts: match r {
                        dn_plugin_api::PermissionRequest::Network { hosts } => hosts.clone(),
                        _ => Vec::new(),
                    },
                }
            })
            .collect();
        let jobs = match &e.registrations {
            Some(r) => jobs::views(state, &e.manifest.id, r),
            None => Ok(Vec::new()),
        }?;
        Ok(AddonDetail {
            summary: summary(
                e,
                &st,
                waiting.get(e.manifest.id.as_str()).copied().unwrap_or(0),
            ),
            chats_all: matches!(st.chats, dn_plugin_api::ChatScope::All),
            chats_projects: match &st.chats {
                dn_plugin_api::ChatScope::Projects(p) => p.clone(),
                _ => Vec::new(),
            },
            knowledge_projects: st.knowledge.clone(),
            permissions,
            jobs,
            held: jobs::held_for(state, &e.manifest.id)?,
        })
    }

    /// The add-ons whose consent dialog should open: never answered, or a new version asks
    /// for more. Only loaded add-ons ask.
    pub fn pending_consent(&self, state: &AppState) -> Result<Vec<ConsentRequest>, String> {
        let db = state.db().map_err(|e| e.message)?;
        let mut out = Vec::new();
        for e in lock(&self.entries).iter().filter(|e| e.loaded()) {
            let st = store::state(&db, &e.manifest.id).map_err(|e| e.to_string())?;
            let new: Vec<Permission> = store::not_asked(&st, &e.manifest);
            if new.is_empty() && st.asked_version.is_some() {
                continue;
            }
            out.push(ConsentRequest {
                id: e.manifest.id.to_string(),
                name: e.manifest.name.clone(),
                version: e.manifest.version.clone(),
                first: st.asked_version.is_none(),
                permissions: e
                    .manifest
                    .permissions
                    .iter()
                    .filter(|r| new.contains(&r.permission()))
                    .map(|r| PermissionView {
                        permission: r.permission().as_str(),
                        asked: false,
                        allowed: false,
                        tools: match r {
                            dn_plugin_api::PermissionRequest::ToolsCall { tools } => tools.clone(),
                            _ => Vec::new(),
                        },
                        hosts: match r {
                            dn_plugin_api::PermissionRequest::Network { hosts } => hosts.clone(),
                            _ => Vec::new(),
                        },
                    })
                    .collect(),
            });
        }
        Ok(out)
    }

    /// Saves the consent dialog's answer for the permissions it showed, and applies it at
    /// once to every `Host` of the add-on.
    pub fn consent(
        &self,
        state: &Arc<AppState>,
        id: &str,
        asked: &[&str],
        consent: ConsentAnswer,
    ) -> Result<(), String> {
        let (manifest, grants, _) = self.find(id)?;
        let parse = |s: &&str| {
            Permission::ALL
                .into_iter()
                .find(|p| p.as_str() == *s)
                .filter(|p| manifest.asks_for(*p))
                .ok_or_else(|| format!("`{s}` isn't a permission {} asks for", manifest.name))
        };
        let asked: Vec<Permission> = asked.iter().map(parse).collect::<Result<_, _>>()?;
        let allowed: BTreeSet<Permission> = consent
            .allowed
            .iter()
            .map(|s| parse(&s.as_str()))
            .collect::<Result<_, _>>()?;
        if let Some(p) = allowed.iter().find(|p| !asked.contains(p)) {
            return Err(format!("`{}` wasn't part of this question", p.as_str()));
        }
        let c = Consent::new(allowed)
            .with_chats(if consent.chats_all {
                dn_plugin_api::ChatScope::All
            } else {
                dn_plugin_api::ChatScope::Projects(consent.chats_projects)
            })
            .with_knowledge_projects(consent.knowledge_projects);
        let db = state.db().map_err(|e| e.message)?;
        store::save_consent(&db, &manifest, &asked, &c).map_err(|e| e.to_string())?;
        let st = store::state(&db, &manifest.id).map_err(|e| e.to_string())?;
        drop(db);
        grants.replace(grants_for(&manifest, &st));
        self.jobs.rebuild(state);
        Ok(())
    }

    /// Turns one permission off (at once, for every `Host`), or back on.
    pub fn set_permission(
        &self,
        state: &Arc<AppState>,
        id: &str,
        permission: &str,
        on: bool,
    ) -> Result<(), String> {
        let (manifest, grants, _) = self.find(id)?;
        let p = Permission::ALL
            .into_iter()
            .find(|p| p.as_str() == permission && manifest.asks_for(*p))
            .ok_or_else(|| {
                format!(
                    "`{permission}` isn't a permission {} asks for",
                    manifest.name
                )
            })?;
        let db = state.db().map_err(|e| e.message)?;
        store::set_permission(&db, &manifest.id, p, on)?;
        let st = store::state(&db, &manifest.id).map_err(|e| e.to_string())?;
        drop(db);
        grants.replace(grants_for(&manifest, &st));
        if !on {
            // Marked as turned off, so the add-on's next use says so, not "not granted".
            grants.revoke(p);
        }
        self.jobs.rebuild(state);
        Ok(())
    }

    /// "Use ‹add-on›": off takes its tools out of the model's list and stops its jobs; its
    /// data stays.
    pub fn set_enabled(&self, state: &Arc<AppState>, id: &str, on: bool) -> Result<(), String> {
        let (manifest, _, _) = self.find(id)?;
        let db = state.db().map_err(|e| e.message)?;
        store::set_enabled(&db, &manifest.id, on).map_err(|e| e.to_string())?;
        drop(db);
        self.sync_tools(state);
        self.jobs.rebuild(state);
        Ok(())
    }

    /// "Delete all local data", step by step as ADR-0013 §5 orders it: stop the scheduler
    /// and revoke every `Host` first; then each add-on's own `delete_all`, with a `Host`
    /// that can only delete its own data; then the host deletes the rest whether or not that
    /// worked. Returns what couldn't be deleted, in the person's words.
    pub async fn delete_all(&self, state: &Arc<AppState>) -> Vec<String> {
        let mut failed = Vec::new();
        self.jobs.stop_for_deletion(state);
        let loaded: Vec<(Arc<dyn Plugin>, PluginManifest)> = {
            let mut entries = lock(&self.entries);
            for e in entries.iter_mut() {
                e.grants.close();
                if e.not_running.is_none() {
                    e.not_running = Some(NotRunning::DataDeleted);
                }
            }
            entries
                .iter()
                .filter(|e| e.registrations.is_some())
                .map(|e| (Arc::clone(&e.plugin), e.manifest.clone()))
                .collect()
        };
        if let Some(exec) = &state.executor {
            for (_, m) in &loaded {
                exec.remove_plugin(m.id.as_str());
            }
        }
        for (plugin, manifest) in loaded {
            let host = backend::deletion_host(state, &manifest);
            let run = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                backend::guarded(plugin.delete_all(&host)),
            )
            .await;
            match run {
                Ok(Some(Ok(()))) => {}
                Ok(None) => {
                    eprintln!(
                        "{} stopped with an internal error while deleting",
                        manifest.id
                    );
                    failed.push(format!("part of {}'s data", manifest.name));
                }
                Ok(Some(Err(e))) => {
                    eprintln!("{} couldn't delete its data: {e}", manifest.id);
                    failed.push(format!("part of {}'s data", manifest.name));
                }
                Err(_) => {
                    eprintln!("{} didn't finish deleting its data in time", manifest.id);
                    failed.push(format!("part of {}'s data", manifest.name));
                }
            }
        }
        // Then everything the host keeps for add-ons, known to this build or not.
        let st = Arc::clone(state);
        let more = tokio::task::spawn_blocking(move || wipe(&st)).await;
        match more {
            Ok(v) => failed.extend(v),
            Err(_) => failed.push("add-on data".into()),
        }
        failed
    }
}

/// The host's own part of "Delete all" for add-ons: their secrets, settings, consent,
/// ownership, jobs and held calls. Blocking.
pub fn wipe(state: &AppState) -> Vec<String> {
    let mut failed = Vec::new();
    let Ok(db) = state.db() else {
        return vec!["add-on data".into()];
    };
    let plugins = store::known_plugins(&db).unwrap_or_default();
    for p in plugins {
        if let Ok(id) = PluginId::new(&p) {
            for name in store::owned(&db, store::Owned::Secret, &id).unwrap_or_default() {
                let sid = dn_secrets::SecretId::addon(id.as_str(), &name);
                if let Err(e) = state.keys.store.delete(&sid) {
                    eprintln!("could not delete an add-on secret: {e}");
                    failed.push("add-on secrets in the system keychain".into());
                }
            }
        }
    }
    if let Err(e) = store::delete_all(&db) {
        eprintln!("could not delete add-on settings: {e}");
        failed.push("add-on settings".into());
    }
    if let Err(e) = dn_jobs::store::delete_all(&db).and_then(|()| dn_jobs::held::delete_all(&db)) {
        eprintln!("could not delete background jobs: {e}");
        failed.push("background jobs and waiting calls".into());
    }
    failed.dedup();
    failed
}

/// The answer from the consent dialog.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct ConsentAnswer {
    pub allowed: Vec<String>,
    #[serde(default)]
    pub chats_all: bool,
    #[serde(default)]
    pub chats_projects: Vec<dn_core::Id>,
    #[serde(default)]
    pub knowledge_projects: Vec<dn_core::Id>,
}

/// A row of the list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AddonSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    /// `running`, `off` or `not_running`.
    pub state: &'static str,
    /// Why it isn't running, in words, when `state` is `not_running`.
    pub reason: Option<String>,
    /// Never answered, or a new version asks for more.
    pub needs_consent: bool,
    /// Held calls and "Run now?" questions.
    pub waiting: usize,
    pub has_jobs: bool,
}

fn summary(e: &Entry, st: &store::AddonState, waiting: usize) -> AddonSummary {
    let state = if e.not_running.is_some() {
        "not_running"
    } else if !st.enabled {
        "off"
    } else {
        "running"
    };
    AddonSummary {
        id: e.manifest.id.to_string(),
        name: e.manifest.name.clone(),
        version: e.manifest.version.clone(),
        state,
        reason: e.not_running.as_ref().map(|r| r.message(&e.manifest.name)),
        needs_consent: e.loaded()
            && (st.asked_version.is_none() || !store::not_asked(st, &e.manifest).is_empty()),
        waiting,
        has_jobs: e.registrations.as_ref().is_some_and(|r| !r.jobs.is_empty()),
    }
}

/// One permission on the add-on's page or in the consent dialog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PermissionView {
    pub permission: &'static str,
    pub asked: bool,
    pub allowed: bool,
    /// For `tools.call`: the tools the add-on listed.
    pub tools: Vec<String>,
    /// For `network`: the hosts the add-on listed.
    pub hosts: Vec<String>,
}

/// An add-on's page.
#[derive(Clone, Debug, Serialize)]
pub struct AddonDetail {
    pub summary: AddonSummary,
    pub permissions: Vec<PermissionView>,
    pub chats_all: bool,
    pub chats_projects: Vec<dn_core::Id>,
    pub knowledge_projects: Vec<dn_core::Id>,
    pub jobs: Vec<jobs::JobView>,
    pub held: Vec<jobs::HeldView>,
}

/// One consent dialog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConsentRequest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// First start (true) or a new version asking for more (false).
    pub first: bool,
    pub permissions: Vec<PermissionView>,
}

/// A clear message for a host service this version doesn't offer add-ons yet.
pub(crate) fn not_offered(caller: &dn_plugin_api::Caller, what: &str) -> PluginError {
    PluginError::unavailable(format!(
        "{} can't use {what}: this version of Demios doesn't offer it to add-ons yet",
        caller.plugin_name
    ))
}

#[cfg(test)]
mod tests;
