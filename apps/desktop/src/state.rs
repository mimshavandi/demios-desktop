//! Process-wide state shared by IPC commands.

use crate::error::{IpcError, IpcResult};
use crate::models::ModelRegistry;
use crate::secrets::{KeyStore, MemoryKeyStore};
use crate::settings::{self, Settings};
use dn_core::Id;
use dn_store::{Connection, MigrationSet, chat as store};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Every migration set the app composes into its database. Add a crate's
/// `migrations()` here when the app starts using that crate's tables.
pub fn migration_sets() -> Vec<MigrationSet> {
    vec![MigrationSet::new("app", settings::migrations())]
}

/// How long a removed provider key can be restored with Undo (it is gone from the key
/// store immediately and held only in memory meanwhile).
pub const KEY_UNDO_WINDOW: Duration = Duration::from_secs(15);

/// A running answer in one conversation. `token` identifies this run, so a finished
/// run never releases the slot of a newer one.
struct Running {
    token: u64,
    stop: oneshot::Sender<()>,
}

/// Text streamed so far for an assistant message, so reopening a chat mid-stream shows it.
#[derive(Default)]
struct Live {
    text: String,
    utf16_len: u32,
}

struct RemovedKey {
    secret: String,
    slots: Vec<(String, String)>,
    at: Instant,
}

pub struct AppState {
    db: Mutex<Connection>,
    pub models: ModelRegistry,
    pub keys: Arc<dyn KeyStore>,
    running: Mutex<HashMap<Id, Running>>,
    live: Mutex<HashMap<Id, Live>>,
    removed_keys: Mutex<HashMap<String, RemovedKey>>,
    next_token: AtomicU64,
}

/// A reserved generation slot. Dropping it without [`Reservation::into_parts`] releases
/// the slot, so an early return (failed DB write) never leaves a chat stuck "busy".
pub struct Reservation {
    state: Arc<AppState>,
    pub conversation_id: Id,
    pub token: u64,
    stop: Option<oneshot::Receiver<()>>,
}

impl Reservation {
    /// Hands the stop receiver to the running task; the task must call
    /// [`AppState::release`] with `token` when it ends.
    pub fn into_parts(mut self) -> (u64, oneshot::Receiver<()>) {
        let rx = self.stop.take().expect("receiver taken once");
        (self.token, rx)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.stop.is_some() {
            self.state.release(self.conversation_id, self.token);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // These maps only hold handles and buffers; recover rather than fail chats.
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl AppState {
    pub fn new(conn: Connection, models: ModelRegistry, keys: Arc<dyn KeyStore>) -> Self {
        Self {
            db: Mutex::new(conn),
            models,
            keys,
            running: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            removed_keys: Mutex::new(HashMap::new()),
            next_token: AtomicU64::new(1),
        }
    }

    /// Opens the database at `path` (creating it), marks interrupted streams cancelled and
    /// purges chats deleted in an earlier session (their Undo window has passed).
    pub fn open(path: &std::path::Path) -> dn_store::Result<Self> {
        let conn = dn_store::open(path, &migration_sets())?;
        let n = store::recover_interrupted(&conn)?;
        if n > 0 {
            eprintln!("marked {n} interrupted answer(s) as cancelled");
        }
        store::purge_deleted(&conn, dn_store::now_ms())?;
        Ok(Self::new(
            conn,
            ModelRegistry::with_builtin(),
            Arc::new(MemoryKeyStore::default()),
        ))
    }

    #[cfg(test)]
    pub fn in_memory() -> dn_store::Result<Self> {
        Ok(Self::new(
            dn_store::open_in_memory(&migration_sets())?,
            ModelRegistry::with_builtin(),
            Arc::new(MemoryKeyStore::default()),
        ))
    }

    pub fn db(&self) -> IpcResult<MutexGuard<'_, Connection>> {
        self.db
            .lock()
            .map_err(|_| IpcError::new("storage", "Local database is unavailable."))
    }

    pub fn settings(&self) -> IpcResult<Settings> {
        Ok(settings::load(&*self.db()?)?)
    }

    pub fn save_settings(&self, s: &Settings) -> IpcResult<()> {
        s.validate().map_err(IpcError::invalid)?;
        Ok(settings::save(&*self.db()?, s)?)
    }

    pub fn create_conversation(&self) -> IpcResult<store::Conversation> {
        Ok(store::create_conversation(&*self.db()?, "New chat")?)
    }

    // ---- generations

    /// Reserves the conversation's single generation slot *before* anything is written.
    pub fn reserve(self: &Arc<Self>, conversation_id: Id) -> IpcResult<Reservation> {
        let mut running = lock(&self.running);
        if running.contains_key(&conversation_id) {
            return Err(IpcError::new(
                "busy",
                "An answer is already streaming in this chat.",
            ));
        }
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        running.insert(conversation_id, Running { token, stop: tx });
        Ok(Reservation {
            state: Arc::clone(self),
            conversation_id,
            token,
            stop: Some(rx),
        })
    }

    /// Frees the slot if it still belongs to `token`.
    pub fn release(&self, conversation_id: Id, token: u64) {
        let mut running = lock(&self.running);
        if running
            .get(&conversation_id)
            .is_some_and(|r| r.token == token)
        {
            running.remove(&conversation_id);
        }
    }

    /// Stops the running answer. The slot is freed at once so a new message can be sent;
    /// the stopped run still stores its partial text.
    pub fn stop(&self, conversation_id: Id) -> bool {
        lock(&self.running)
            .remove(&conversation_id)
            .is_some_and(|r| r.stop.send(()).is_ok())
    }

    pub fn is_generating(&self, conversation_id: Id) -> bool {
        lock(&self.running).contains_key(&conversation_id)
    }

    /// Appends streamed text; returns the UTF-16 length before it (the UI's string unit),
    /// which lets the UI skip deltas it already has after reopening a chat.
    pub fn append_live(&self, message_id: Id, text: &str) -> u32 {
        let mut live = lock(&self.live);
        let l = live.entry(message_id).or_default();
        let before = l.utf16_len;
        l.text.push_str(text);
        l.utf16_len += text.encode_utf16().count() as u32;
        before
    }

    pub fn end_live(&self, message_id: Id) {
        lock(&self.live).remove(&message_id);
    }

    /// Messages of a conversation, with streaming answers filled in from memory.
    pub fn list_messages(&self, conversation_id: Id) -> IpcResult<Vec<store::StoredMessage>> {
        let mut msgs = store::list_messages(&*self.db()?, conversation_id)?;
        let live = lock(&self.live);
        for m in &mut msgs {
            if m.status == store::MessageStatus::Streaming {
                if let Some(l) = live.get(&m.id) {
                    m.content = vec![dn_core::chat::ContentPart::Text {
                        text: l.text.clone(),
                    }];
                }
            }
        }
        Ok(msgs)
    }

    // ---- provider keys with Undo

    pub fn remember_removed_key(
        &self,
        provider_id: &str,
        secret: String,
        slots: Vec<(String, String)>,
    ) {
        let mut m = lock(&self.removed_keys);
        m.retain(|_, k| k.at.elapsed() < KEY_UNDO_WINDOW);
        m.insert(
            provider_id.to_string(),
            RemovedKey {
                secret,
                slots,
                at: Instant::now(),
            },
        );
    }

    /// The removed key and the slots it was cleared from, if still within the window.
    pub fn take_removed_key(&self, provider_id: &str) -> Option<(String, Vec<(String, String)>)> {
        let mut m = lock(&self.removed_keys);
        m.retain(|_, k| k.at.elapsed() < KEY_UNDO_WINDOW);
        m.remove(provider_id).map(|k| (k.secret, k.slots))
    }
}
