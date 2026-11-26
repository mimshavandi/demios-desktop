//! Commands for Settings › Add-ons and the consent dialog.

use super::{AddonDetail, AddonSummary, ConsentAnswer, ConsentRequest};
use crate::commands::{AppStateRef, blocking};
use crate::error::{IpcError, IpcResult};
use dn_core::Id;
use dn_jobs::JobKey;
use dn_plugin_api::{MissedRunRule, PluginId};

fn bad(message: String) -> IpcError {
    IpcError::invalid(message)
}

fn key(addon: &str, job: &str) -> IpcResult<JobKey> {
    let id = PluginId::new(addon).map_err(|e| bad(e.message))?;
    Ok(JobKey::new(id, job))
}

/// The add-ons of this build. Empty in the regular build: the UI then shows no Add-ons page.
#[tauri::command]
pub async fn addons_list(state: AppStateRef<'_>) -> IpcResult<Vec<AddonSummary>> {
    blocking(&state, |s| s.addons.list(s).map_err(bad)).await
}

#[tauri::command]
pub async fn addon_detail(state: AppStateRef<'_>, id: String) -> IpcResult<AddonDetail> {
    blocking(&state, move |s| s.addons.detail(s, &id).map_err(bad)).await
}

#[tauri::command]
pub async fn addons_pending_consent(state: AppStateRef<'_>) -> IpcResult<Vec<ConsentRequest>> {
    blocking(&state, |s| s.addons.pending_consent(s).map_err(bad)).await
}

/// The consent dialog's answer: `asked` lists the permissions it showed, `answer` which of
/// them the person allowed and the scopes they chose.
#[tauri::command]
pub async fn addon_consent(
    state: AppStateRef<'_>,
    id: String,
    asked: Vec<String>,
    answer: ConsentAnswer,
) -> IpcResult<()> {
    blocking(&state, move |s| {
        let asked: Vec<&str> = asked.iter().map(String::as_str).collect();
        s.addons.consent(s, &id, &asked, answer).map_err(bad)
    })
    .await
}

#[tauri::command]
pub async fn addon_set_permission(
    state: AppStateRef<'_>,
    id: String,
    permission: String,
    on: bool,
) -> IpcResult<()> {
    blocking(&state, move |s| {
        s.addons
            .set_permission(s, &id, &permission, on)
            .map_err(bad)
    })
    .await
}

#[tauri::command]
pub async fn addon_set_enabled(state: AppStateRef<'_>, id: String, on: bool) -> IpcResult<()> {
    blocking(&state, move |s| {
        s.addons.set_enabled(s, &id, on).map_err(bad)
    })
    .await
}

/// The window's offset from UTC in minutes (JavaScript's `-getTimezoneOffset()`). Jobs
/// start once it is known.
#[tauri::command]
pub async fn addons_set_utc_offset(state: AppStateRef<'_>, minutes: i32) -> IpcResult<()> {
    if !(-14 * 60..=14 * 60).contains(&minutes) {
        return Err(bad(format!("{minutes} minutes isn't a time-zone offset")));
    }
    let s = std::sync::Arc::clone(state.inner());
    s.addons.jobs.set_utc_offset(&s, i64::from(minutes) * 60);
    Ok(())
}

#[tauri::command]
pub async fn addons_paused(state: AppStateRef<'_>) -> IpcResult<bool> {
    blocking(&state, |s| s.addons.jobs.paused(s).map_err(bad)).await
}

#[tauri::command]
pub async fn addons_set_paused(state: AppStateRef<'_>, paused: bool) -> IpcResult<()> {
    blocking(&state, move |s| {
        s.addons.jobs.set_paused(s, paused).map_err(bad)
    })
    .await
}

#[tauri::command]
pub async fn addon_set_job_enabled(
    state: AppStateRef<'_>,
    id: String,
    job: String,
    on: bool,
) -> IpcResult<()> {
    blocking(&state, move |s| {
        let k = key(&id, &job)?;
        s.addons.jobs.set_enabled(s, &k, on).map_err(bad)
    })
    .await
}

/// The person's missed-run rule; `None` goes back to the add-on's proposal.
#[tauri::command]
pub async fn addon_set_missed_rule(
    state: AppStateRef<'_>,
    id: String,
    job: String,
    rule: Option<MissedRunRule>,
) -> IpcResult<()> {
    blocking(&state, move |s| {
        let k = key(&id, &job)?;
        s.addons.jobs.set_missed_rule(s, &k, rule).map_err(bad)
    })
    .await
}

#[tauri::command]
pub async fn addon_answer_question(
    state: AppStateRef<'_>,
    id: String,
    job: String,
    run_now: bool,
) -> IpcResult<()> {
    let k = key(&id, &job)?;
    let s = std::sync::Arc::clone(state.inner());
    s.addons
        .jobs
        .answer_question(&s, &k, run_now)
        .await
        .map_err(bad)
}

/// "Allow once" or "Dismiss" on a held call, for the arguments the window showed.
#[tauri::command]
pub async fn addon_answer_held(
    state: AppStateRef<'_>,
    held_id: Id,
    approve: bool,
    arguments_sha256: String,
) -> IpcResult<()> {
    let s = std::sync::Arc::clone(state.inner());
    if !approve {
        return blocking(&state, move |s| {
            s.addons.jobs.dismiss_held(s, held_id).map_err(bad)
        })
        .await;
    }
    s.addons
        .jobs
        .answer_held(&s, held_id, true, arguments_sha256)
        .await
        .map_err(bad)
}

/// Calls `on_change` whenever a job ran, a call was held or something was answered, so
/// Settings › Add-ons can reload. A new subscription replaces the old one.
#[tauri::command]
pub async fn addons_watch(
    state: AppStateRef<'_>,
    on_change: tauri::ipc::Channel<()>,
) -> IpcResult<()> {
    state.addons.jobs.on_change(std::sync::Arc::new(move || {
        let _ = on_change.send(());
    }));
    Ok(())
}
