//! Sync with the paired Node, in the background (docs/design/dn-sync.md §9, §14 step 4).
//!
//! - Sync runs only while a Node is paired: it starts when pairing finishes and when the
//!   app opens with a Node paired, and stops on Unpair, Forget and Delete all, which also
//!   end change logging and delete the group keys and what sync kept ([`leave`]).
//! - A round runs every [`INTERVAL_SECS`], and at once on "Sync now".
//! - A failed round is shown in Settings › Node. It's tried again after a growing pause,
//!   at most [`MAX_FAILURES`] times in a row; then sync stops until "Sync now". A failure
//!   that waiting won't fix (this computer was removed, the Node's key or version doesn't
//!   match, the Node refused) stops it at once.
//! - The round lets go of the database between batches, so a first sync of a long history
//!   doesn't block the app.

use crate::state::AppState;
use dn_sync::SyncError;
use dn_sync::identity::DeviceKeys;
use dn_sync::link::{self, NodeLink};
use dn_sync::records::store_collections;
use dn_sync::session::{self, Device, Report};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Time between rounds while all is well.
pub const INTERVAL_SECS: u64 = 60;
/// Failed rounds in a row before sync stops and waits for "Sync now".
pub const MAX_FAILURES: u32 = 5;
/// The first pause after a failure; it doubles each time.
const FIRST_RETRY_SECS: u64 = 5;
/// The longest a round may take before it counts as failed.
const ROUND_SECS: u64 = 15 * 60;

/// Where sync stands, for Settings › Node.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    /// No Node paired.
    #[default]
    Off,
    Syncing,
    /// The last round worked; the next comes on its own.
    UpToDate,
    /// The last round failed; it's tried again after a pause.
    Retrying,
    /// Sync stopped after a failure; "Sync now" tries again.
    Stopped,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SyncStatus {
    pub state: SyncState,
    /// Unix milliseconds of the last round that worked.
    pub last_synced_at: Option<i64>,
    /// Failed rounds in a row.
    pub failures: u32,
    /// Why the last round failed, in the user's words.
    pub error: Option<String>,
    /// Changes from other devices applied in the last round that worked.
    pub applied: usize,
}

/// The background task and its status.
#[derive(Default)]
pub struct SyncControl {
    status: Mutex<SyncStatus>,
    wake: tokio::sync::Notify,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Bumped when sync ends, under the database lock: work of an earlier task that is
    /// still running (on a blocking thread) sees it and writes nothing.
    generation: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl SyncControl {
    pub fn status(&self) -> SyncStatus {
        lock(&self.status).clone()
    }

    fn set(&self, f: impl FnOnce(&mut SyncStatus)) {
        f(&mut lock(&self.status));
    }

    /// Stops the task (between two awaits: never inside a database write).
    fn stop(&self) {
        if let Some(t) = lock(&self.task).take() {
            t.abort();
        }
        *lock(&self.status) = SyncStatus::default();
    }
}

/// Starts the background task if a Node is paired and it isn't running.
pub fn start(state: &Arc<AppState>) {
    let paired = state
        .db()
        .ok()
        .and_then(|db| link::get(&db).ok().flatten())
        .is_some();
    if !paired {
        return;
    }
    let control = &state.node.sync;
    let mut task = lock(&control.task);
    if task.is_some() {
        return;
    }
    let st = state.clone();
    let generation = control.generation.load(Ordering::SeqCst);
    *task = Some(tauri::async_runtime::spawn(async move {
        run_loop(st, generation).await
    }));
}

/// "Sync now": a round at once; after sync stopped, it starts again.
pub fn sync_now(state: &Arc<AppState>) {
    let running = lock(&state.node.sync.task).is_some();
    if running {
        state.node.sync.wake.notify_one();
    } else {
        start(state);
    }
}

/// Stops the task and waits until it has ended, so nothing of a round still running
/// (a key kept, a table written) lands after [`leave`]. Call it before `leave`.
pub async fn stop_and_wait(state: &AppState) {
    let control = &state.node.sync;
    control.generation.fetch_add(1, Ordering::SeqCst);
    let task = lock(&control.task).take();
    if let Some(t) = task {
        t.abort();
        let _ = t.await;
    }
    *lock(&control.status) = SyncStatus::default();
}

/// Ends sync with the Node: stops the task, stops change logging, and deletes the group
/// keys and what sync kept. Call it before the link is forgotten, after
/// [`stop_and_wait`].
pub fn leave(state: &AppState) -> Result<(), String> {
    state.node.sync.stop();
    let db = state.db().map_err(|e| e.message)?;
    // Under the lock: a round's blocking step that starts after this does nothing.
    state.node.sync.generation.fetch_add(1, Ordering::SeqCst);
    if let Some(l) = link::get(&db).map_err(|e| e.to_string())? {
        session::reset(&db, &*state.keys.store, &l.node.keys).map_err(|e| e.to_string())?;
    }
    dn_store::sync_rows::disable(&db).map_err(|e| e.to_string())
}

/// Whether waiting won't help: sync stops at once.
fn lasting(e: &SyncError) -> bool {
    matches!(
        e,
        SyncError::Removed
            | SyncError::Refused(_)
            | SyncError::NodeKeyMismatch
            | SyncError::VersionMismatch(_)
            | SyncError::ForkedChain
            | SyncError::Equivocation(_)
            | SyncError::DamagedKeys(_)
    )
}

fn message(e: &SyncError) -> String {
    match e {
        SyncError::Removed => {
            "Your Node removed this computer. Forget the Node here, then pair again.".into()
        }
        SyncError::Refused(reason) => format!("Your Node refused to sync: {reason}."),
        SyncError::NodeKeyMismatch => "The device at your Node's address isn't your Node. \
             Sync stopped; check the Node, then choose Sync now."
            .into(),
        SyncError::Io(_) => {
            "Couldn't reach your Node. Check that it's running and on the same network or VPN."
                .into()
        }
        other => other.to_string(),
    }
}

async fn run_loop(state: Arc<AppState>, generation: u64) {
    let control = &state.node.sync;
    loop {
        control.set(|s| s.state = SyncState::Syncing);
        let result =
            tokio::time::timeout(Duration::from_secs(ROUND_SECS), round(&state, generation))
                .await
                .unwrap_or(Err(SyncError::Io(std::io::ErrorKind::TimedOut.into())));
        let pause = match result {
            Ok(report) => {
                control.set(|s| {
                    *s = SyncStatus {
                        state: SyncState::UpToDate,
                        last_synced_at: Some(dn_store::now_ms()),
                        failures: 0,
                        error: None,
                        applied: report.applied,
                    }
                });
                after_purge(&state, &report).await;
                Some(Duration::from_secs(INTERVAL_SECS))
            }
            Err(e) => {
                tracing::warn!("sync with the Node failed: {e}");
                let mut failures = 0;
                control.set(|s| {
                    s.failures += 1;
                    failures = s.failures;
                    s.error = Some(message(&e));
                    s.state = if lasting(&e) || s.failures >= MAX_FAILURES {
                        SyncState::Stopped
                    } else {
                        SyncState::Retrying
                    };
                });
                if lasting(&e) || failures >= MAX_FAILURES {
                    None
                } else {
                    Some(Duration::from_secs(FIRST_RETRY_SECS << (failures - 1)))
                }
            }
        };
        match pause {
            Some(d) => {
                let _ = tokio::time::timeout(d, control.wake.notified()).await;
            }
            None => {
                // Stopped: only "Sync now" starts it again, from no failures.
                control.wake.notified().await;
                control.set(|s| s.failures = 0);
            }
        }
    }
}

/// What a round needs from the app.
fn prepare(state: &AppState, generation: u64) -> Result<(DeviceKeys, NodeLink), SyncError> {
    let db = state
        .db()
        .map_err(|e| SyncError::Store(e.message.to_string()))?;
    // Sync ended while this waited for the lock: write nothing, make no keys.
    if state.node.sync.generation.load(Ordering::SeqCst) != generation {
        return Err(SyncError::Store("sync was stopped".into()));
    }
    let l = link::get(&db)?.ok_or(SyncError::Store("no Node is paired".into()))?;
    // Change logging is on while a Node is paired (it seeds what's already here once).
    dn_store::sync_rows::enable(&db).map_err(|e| SyncError::Store(e.to_string()))?;
    let keys = DeviceKeys::load_or_create(&*state.keys.store)?;
    drop(db);
    Ok((keys, l))
}

async fn round(state: &Arc<AppState>, generation: u64) -> Result<Report, SyncError> {
    let st = state.clone();
    let (keys, l) = tauri::async_runtime::spawn_blocking(move || prepare(&st, generation))
        .await
        .map_err(|e| SyncError::Store(e.to_string()))??;
    let mut ch = crate::node::connect_member(&keys, &l).await?;
    let cols = store_collections();
    let now = || dn_store::now_ms() as u64;
    let dev = Device {
        db: state.db_mutex(),
        store: &*state.keys.store,
        me: &keys,
        node: &l.node.keys,
        collections: &cols,
        now_ms: &now,
    };
    let report = session::run(&mut ch, &dev).await;
    let _ = ch.close().await;
    report
}

/// Records another device deleted for good are gone here too; what the app keeps about
/// them outside the store goes with them.
async fn after_purge(state: &Arc<AppState>, report: &Report) {
    let chats: Vec<String> = report
        .purged
        .iter()
        .filter(|(collection, _)| collection == "chat")
        .map(|(_, id)| id.clone())
        .collect();
    if chats.is_empty() {
        return;
    }
    let st = state.clone();
    let done = tauri::async_runtime::spawn_blocking(move || {
        for id in chats {
            // Ids from other devices were checked when the purge was applied; one that
            // isn't canonical here is a bug, said out loud rather than skipped.
            match dn_store::sync_rows::canonical_id(&id) {
                Ok(chat) => {
                    let left = st.forget_purged_chat(chat);
                    if !left.is_empty() {
                        tracing::warn!(
                            "sync: chat {id} was deleted on another device; couldn't remove its {}",
                            left.join(", ")
                        );
                    }
                }
                Err(e) => tracing::warn!("sync: a purged chat has a bad id {id:?}: {e}"),
            }
        }
    })
    .await;
    if let Err(e) = done {
        tracing::warn!("sync: cleaning up after purged chats failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A round's blocking step that started before sync ended writes nothing after it:
    /// no change logging switched back on, no device keys made.
    #[tokio::test(flavor = "multi_thread")]
    async fn work_of_a_stopped_round_writes_nothing() {
        let state = Arc::new(AppState::in_memory().unwrap());
        let before = state.node.sync.generation.load(Ordering::SeqCst);
        stop_and_wait(&state).await;
        leave(&state).unwrap();
        let err = prepare(&state, before).unwrap_err();
        assert!(err.to_string().contains("sync was stopped"), "{err}");
        assert!(!dn_store::sync_rows::enabled(&state.db().unwrap()).unwrap());
        assert!(
            state
                .keys
                .store
                .get(&dn_sync::identity::secret_id())
                .unwrap()
                .is_none(),
            "no device keys were made"
        );
        assert_eq!(state.node.sync.status(), SyncStatus::default());
    }
}
