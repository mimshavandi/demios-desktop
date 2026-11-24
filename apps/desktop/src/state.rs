//! Process-wide state shared by IPC commands.

use crate::error::{IpcError, IpcResult};
use crate::models::ModelRegistry;
use crate::secrets::{KeyStore, MemoryKeyStore};
use crate::settings::{self, Settings};
use dn_core::Id;
use dn_store::{Connection, MigrationSet, chat as store};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::oneshot;

/// Every migration set the app composes into its database. Add a crate's
/// `migrations()` here when the app starts using that crate's tables.
pub fn migration_sets() -> Vec<MigrationSet> {
    vec![MigrationSet::new("app", settings::migrations())]
}

pub struct AppState {
    db: Mutex<Connection>,
    pub models: ModelRegistry,
    pub keys: Arc<dyn KeyStore>,
    generations: Mutex<HashMap<Id, oneshot::Sender<()>>>,
}

impl AppState {
    pub fn new(conn: Connection, models: ModelRegistry, keys: Arc<dyn KeyStore>) -> Self {
        Self {
            db: Mutex::new(conn),
            models,
            keys,
            generations: Mutex::new(HashMap::new()),
        }
    }

    /// Opens the database at `path` (creating it), recovers interrupted streams.
    pub fn open(path: &std::path::Path) -> dn_store::Result<Self> {
        let conn = dn_store::open(path, &migration_sets())?;
        let n = store::recover_interrupted(&conn)?;
        if n > 0 {
            eprintln!("marked {n} interrupted answer(s) as cancelled");
        }
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

    fn gens(&self) -> MutexGuard<'_, HashMap<Id, oneshot::Sender<()>>> {
        // A poisoned map only loses stop handles; recover it rather than failing chats.
        self.generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Registers a running answer; false if one is already running in the conversation.
    pub fn begin_generation(&self, conversation_id: Id, stop: oneshot::Sender<()>) -> bool {
        let mut g = self.gens();
        if g.contains_key(&conversation_id) {
            return false;
        }
        g.insert(conversation_id, stop);
        true
    }

    pub fn take_generation(&self, conversation_id: Id) -> Option<oneshot::Sender<()>> {
        self.gens().remove(&conversation_id)
    }

    pub fn end_generation(&self, conversation_id: Id) {
        self.gens().remove(&conversation_id);
    }

    pub fn is_generating(&self, conversation_id: Id) -> bool {
        self.gens().contains_key(&conversation_id)
    }

    pub fn generating(&self) -> Vec<Id> {
        self.gens().keys().copied().collect()
    }
}
