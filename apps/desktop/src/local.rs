//! Local models (T-SHELL-07): the curated catalogue (`dn-models`), resumable verified
//! downloads with progress, fit status per row, and llama.cpp (`dn-inference`) as the
//! `ChatModel` behind `local:<catalogue id>`.
//!
//! Files live in `dn_models::download::default_models_dir()` (shared with the `dn` CLI, so a
//! model pulled there shows up here). One local model is loaded at a time.

use dn_core::chat::ChatModel;
use dn_core::model::{Capability, FitStatus, ModelRef};
use dn_core::{CoreError, RecoveryAction};
use dn_inference::{LlamaCppBackend, LoadOptions, LoadedModel, ModelSpec};
use dn_models::download::DownloadError;
use dn_models::{Catalog, CatalogEntry, FitReport, Gguf, Machine, ModelFootprint};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

/// Where a catalogue model is (design-system §4, Download row).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DownloadState {
    NotDownloaded,
    Queued,
    Downloading {
        done: u64,
        total: u64,
        /// Smoothed over the last couple of seconds; 0 until known.
        bytes_per_sec: u64,
    },
    Paused {
        done: u64,
        total: u64,
    },
    Verifying {
        total: u64,
    },
    Ready,
    Failed {
        message: String,
        done: u64,
        total: u64,
    },
}

/// Fit status shown on the row (Ready / Caution / Memory pressure / Won't fit).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FitView {
    pub status: FitStatus,
    /// From the file size alone (before download); exact once the GGUF header is read.
    pub estimated: bool,
    /// One line, user words.
    pub reason: String,
    pub suggested_quant: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LocalModelRow {
    pub id: String,
    pub model_ref: String,
    pub name: String,
    pub repo: String,
    pub quant: String,
    pub size: u64,
    pub params_b: f64,
    pub capabilities: Vec<Capability>,
    pub license: String,
    /// Missing, non-commercial or custom ("other") licence: the UI warns (D-020).
    pub license_warning: bool,
    pub card_url: String,
    pub notes: String,
    pub state: DownloadState,
    pub fit: FitView,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalModelEvent {
    Changed { row: LocalModelRow },
}

type Sink = Arc<dyn Fn(LocalModelEvent) + Send + Sync>;

pub struct LocalModels {
    dir: PathBuf,
    catalog: Catalog,
    machine: Machine,
    state: Mutex<HashMap<String, DownloadState>>,
    fits: Mutex<HashMap<String, FitView>>,
    tasks: Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>,
    sink: Mutex<Option<Sink>>,
    backend: Arc<LlamaCppBackend>,
    /// The one loaded local model.
    loaded: tokio::sync::Mutex<Option<(String, Arc<LoadedModel<LlamaCppBackend>>)>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

/// Licence ids we warn about (D-020): unknown, non-commercial, or a custom "other" licence.
pub fn license_needs_warning(license: &str) -> bool {
    let l = license.trim().to_ascii_lowercase();
    l.is_empty()
        || l == "unknown"
        || l == "other"
        || l.contains("-nc")
        || l.contains("noncommercial")
        || l.contains("non-commercial")
}

fn fit_reason(status: FitStatus) -> &'static str {
    match status {
        FitStatus::Ready => "Fits this computer with room to spare.",
        FitStatus::Caution => "Fits, with little memory left for other apps.",
        FitStatus::MemoryPressure => "Runs slowly: needs about all of this computer's memory.",
        FitStatus::MemoryBlocked => "Won't fit in this computer's memory.",
    }
}

fn fit_view(report: &FitReport, estimated: bool) -> FitView {
    let mut reason = fit_reason(report.status).to_string();
    if let Some(q) = &report.suggested_quant {
        reason.push_str(&format!(" A smaller version ({q}) would fit better."));
    }
    FitView {
        status: report.status,
        estimated,
        reason,
        suggested_quant: report.suggested_quant.clone(),
    }
}

/// Before download only the file size and parameter count are known: estimate with the
/// weights and runtime overhead (the KV cache is left out, so this is optimistic).
fn estimated_footprint(e: &CatalogEntry) -> ModelFootprint {
    ModelFootprint {
        weight_bytes: e.size,
        expert_bytes: 0,
        n_params: (e.params_b * 1e9) as u64,
        n_layer: 0,
        n_ctx_train: 4096,
        n_vocab: 0,
        n_embd: 0,
        kv_elems_per_token: 0,
        swa_kv_elems_per_token: 0,
        sliding_window: 0,
        quant: Some(e.quant.clone()),
    }
}

/// User words for a failed download (design-system §6).
pub fn download_error_message(e: &DownloadError) -> String {
    let gb = |b: u64| format!("{:.1} GB", b as f64 / 1e9);
    match e {
        DownloadError::DiskFull { need, free } => format!(
            "Not enough disk space: needs {}, {} free. Free up space, then retry.",
            gb(*need),
            gb(*free)
        ),
        DownloadError::Hash { .. } | DownloadError::Size { .. } => {
            "The downloaded file didn't match its checksum, so it was deleted. Retry to download it again.".into()
        }
        DownloadError::Busy(_) => {
            "Another app (or the dn command) is downloading this model. Wait for it, then retry.".into()
        }
        DownloadError::Status(s) if *s == 404 => {
            "The model file isn't on Hugging Face at the pinned version anymore.".into()
        }
        DownloadError::Status(_) | DownloadError::Http(_) => {
            "Couldn't reach Hugging Face. Check your connection, then retry.".into()
        }
        DownloadError::Io(_) => "Couldn't write the model file. Check the disk, then retry.".into(),
    }
}

impl LocalModels {
    pub fn new(dir: PathBuf, catalog: Catalog, machine: Machine) -> Self {
        let lm = Self {
            dir,
            catalog,
            machine,
            state: Mutex::new(HashMap::new()),
            fits: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
            sink: Mutex::new(None),
            backend: Arc::new(LlamaCppBackend::new()),
            loaded: tokio::sync::Mutex::new(None),
        };
        lm.scan();
        lm
    }

    /// The app's instance: built-in catalogue, shared models dir, this machine.
    pub fn open() -> Self {
        Self::new(
            dn_models::download::default_models_dir(),
            Catalog::builtin(),
            dn_models::detect_machine(),
        )
    }

    fn path(&self, e: &CatalogEntry) -> PathBuf {
        self.dir.join(&e.file)
    }

    /// Reads what is on disk: verified file → Ready; partial file → Paused.
    fn scan(&self) {
        let mut state = lock(&self.state);
        for e in &self.catalog.models {
            if !e.has_plain_file_name() {
                continue;
            }
            let p = self.path(e);
            let verified = std::fs::metadata(&p).is_ok_and(|m| m.len() == e.size)
                && std::fs::read_to_string(suffixed(&p, ".sha256"))
                    .is_ok_and(|m| m.trim().eq_ignore_ascii_case(&e.sha256));
            let partial = std::fs::metadata(suffixed(&p, ".part"))
                .map(|m| m.len())
                .unwrap_or(0);
            let s = if verified {
                DownloadState::Ready
            } else if partial > 0 {
                DownloadState::Paused {
                    done: partial,
                    total: e.size,
                }
            } else {
                DownloadState::NotDownloaded
            };
            state.insert(e.id.clone(), s);
        }
        drop(state);
        for e in &self.catalog.models {
            self.refresh_fit(e);
        }
    }

    /// Exact fit from the GGUF header when the file is here, else an estimate.
    fn refresh_fit(&self, e: &CatalogEntry) {
        let ready = matches!(lock(&self.state).get(&e.id), Some(DownloadState::Ready));
        let exact = ready
            .then(|| Gguf::open(&self.path(e)).ok())
            .flatten()
            .map(|g| {
                fit_view(
                    &dn_models::fit(&ModelFootprint::from_gguf(&g), &self.machine),
                    false,
                )
            });
        let view = exact.unwrap_or_else(|| {
            fit_view(
                &dn_models::fit(&estimated_footprint(e), &self.machine),
                true,
            )
        });
        lock(&self.fits).insert(e.id.clone(), view);
    }

    fn row(&self, e: &CatalogEntry) -> LocalModelRow {
        let state = lock(&self.state)
            .get(&e.id)
            .cloned()
            .unwrap_or(DownloadState::NotDownloaded);
        let fit = lock(&self.fits).get(&e.id).cloned().unwrap_or_else(|| {
            fit_view(
                &dn_models::fit(&estimated_footprint(e), &self.machine),
                true,
            )
        });
        LocalModelRow {
            id: e.id.clone(),
            model_ref: format!("local:{}", e.id),
            name: e.name.clone(),
            repo: e.repo.clone(),
            quant: e.quant.clone(),
            size: e.size,
            params_b: e.params_b,
            capabilities: e.capabilities.clone(),
            license: e.license.clone(),
            license_warning: license_needs_warning(&e.license),
            card_url: format!("https://huggingface.co/{}", e.repo),
            notes: e.notes.clone(),
            state,
            fit,
        }
    }

    pub fn rows(&self) -> Vec<LocalModelRow> {
        self.catalog.models.iter().map(|e| self.row(e)).collect()
    }

    pub fn is_ready(&self, id: &str) -> bool {
        matches!(lock(&self.state).get(id), Some(DownloadState::Ready))
    }

    /// Ready models, for the resolver and the slot pickers.
    pub fn ready(&self) -> Vec<&CatalogEntry> {
        self.catalog
            .models
            .iter()
            .filter(|e| self.is_ready(&e.id))
            .collect()
    }

    pub fn entries(&self) -> &[CatalogEntry] {
        &self.catalog.models
    }

    pub fn watch(&self, sink: Sink) -> Vec<LocalModelRow> {
        *lock(&self.sink) = Some(sink);
        self.rows()
    }

    fn set(&self, id: &str, s: DownloadState) {
        lock(&self.state).insert(id.to_string(), s);
        if let Some(e) = self.catalog.get(id) {
            let row = self.row(e);
            let sink = lock(&self.sink).clone();
            if let Some(sink) = sink {
                sink(LocalModelEvent::Changed { row });
            }
        }
    }

    /// Starts (or resumes) a download. A no-op while one is running.
    pub fn download(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let e = self
            .catalog
            .get(id)
            .cloned()
            .ok_or_else(|| format!("Unknown model `{id}`."))?;
        if self.is_ready(id) {
            return Ok(());
        }
        let mut tasks = lock(&self.tasks);
        if tasks.contains_key(id) {
            return Ok(());
        }
        self.set(id, DownloadState::Queued);
        let this = Arc::clone(self);
        let handle = tauri::async_runtime::spawn(async move {
            let result = this.run_download(&e).await;
            lock(&this.tasks).remove(&e.id);
            match result {
                Ok(()) => {
                    this.set(&e.id, DownloadState::Ready);
                    let lm = Arc::clone(&this);
                    let entry = e.clone();
                    let _ =
                        tauri::async_runtime::spawn_blocking(move || lm.refresh_fit(&entry)).await;
                    this.set(&e.id, DownloadState::Ready);
                }
                Err(err) => {
                    eprintln!("download of `{}` failed: {err}", e.id);
                    let done = std::fs::metadata(suffixed(&this.path(&e), ".part"))
                        .map(|m| m.len())
                        .unwrap_or(0);
                    this.set(
                        &e.id,
                        DownloadState::Failed {
                            message: download_error_message(&err),
                            done,
                            total: e.size,
                        },
                    );
                }
            }
        });
        tasks.insert(id.to_string(), handle);
        Ok(())
    }

    async fn run_download(self: &Arc<Self>, e: &CatalogEntry) -> Result<(), DownloadError> {
        let client = dn_models::download::client()?;
        let this = Arc::clone(self);
        let id = e.id.clone();
        let mut last_emit = Instant::now() - std::time::Duration::from_secs(1);
        let mut window: (Instant, u64) = (Instant::now(), 0);
        let mut rate = 0u64;
        let mut progress = move |done: u64, total: u64| {
            if done >= total {
                this.set(&id, DownloadState::Verifying { total });
                return;
            }
            let now = Instant::now();
            let dt = now.duration_since(window.0).as_secs_f64();
            if dt >= 1.0 {
                let r = (done.saturating_sub(window.1)) as f64 / dt;
                rate = if rate == 0 {
                    r as u64
                } else {
                    ((rate as f64) * 0.6 + r * 0.4) as u64
                };
                window = (now, done);
            }
            if now.duration_since(last_emit).as_millis() >= 250 {
                last_emit = now;
                this.set(
                    &id,
                    DownloadState::Downloading {
                        done,
                        total,
                        bytes_per_sec: rate,
                    },
                );
            }
        };
        dn_models::download::pull(&client, e, &self.dir, &mut progress).await?;
        Ok(())
    }

    /// Pauses a running download: the task is dropped (which stops the transfer) and the
    /// partial file is kept, so the next download resumes from it.
    pub fn pause(&self, id: &str) {
        let Some(handle) = lock(&self.tasks).remove(id) else {
            return;
        };
        handle.abort();
        let total = self.catalog.get(id).map_or(0, |e| e.size);
        let done = self
            .catalog
            .get(id)
            .and_then(|e| std::fs::metadata(suffixed(&self.path(e), ".part")).ok())
            .map_or(0, |m| m.len());
        self.set(id, DownloadState::Paused { done, total });
    }

    /// Stops every download (app exit). Partial files stay for the next start.
    pub fn pause_all(&self) {
        let ids: Vec<String> = lock(&self.tasks).keys().cloned().collect();
        for id in ids {
            self.pause(&id);
        }
    }

    /// The `ChatModel` for `local:<id>`: loads the file (unloading the previous local
    /// model) on first use. Async; callers on a blocking thread use `block_on`.
    pub async fn chat_model(&self, id: &str) -> Result<Arc<dyn ChatModel>, CoreError> {
        let e = self
            .catalog
            .get(id)
            .ok_or_else(|| CoreError::ModelUnavailable {
                user_message: format!("`local:{id}` isn't a model this app knows."),
                developer_message: format!("unknown catalogue id {id}"),
                recovery: vec![],
            })?;
        if !self.is_ready(id) {
            return Err(CoreError::ModelUnavailable {
                user_message: format!(
                    "{} isn't downloaded yet. Download it in Settings → Models.",
                    e.name
                ),
                developer_message: format!("{id} not downloaded"),
                recovery: vec![RecoveryAction::DownloadModel {
                    capability: Capability::Chat,
                }],
            });
        }
        let mut loaded = self.loaded.lock().await;
        if let Some((cur, m)) = loaded.as_ref()
            && cur == id
        {
            let m: Arc<dyn ChatModel> = m.clone();
            return Ok(m);
        }
        // One local model in memory at a time.
        if let Some((_, old)) = loaded.take()
            && let Ok(old) = Arc::try_unwrap(old)
        {
            let _ = old.unload().await;
        }
        let m = LoadedModel::load(
            Arc::clone(&self.backend),
            &ModelSpec::gguf(self.path(e)),
            &LoadOptions::default(),
        )
        .await
        .map_err(|err| CoreError::ModelUnavailable {
            user_message: format!("{} couldn't be loaded. It may not fit in memory.", e.name),
            developer_message: format!("load {id}: {err}"),
            recovery: vec![RecoveryAction::ChooseModelForSlot {
                agent_id: crate::agent::AGENT_ID.into(),
                slot_id: "primary".into(),
            }],
        })?;
        let m = Arc::new(m);
        *loaded = Some((id.to_string(), Arc::clone(&m)));
        let m: Arc<dyn ChatModel> = m;
        Ok(m)
    }

    /// `local:<id>` for a catalogue id, if it is one.
    pub fn catalogue_id(model: &ModelRef) -> Option<&str> {
        match model {
            ModelRef::Local { model_id } => Some(model_id.as_str()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, size: u64, sha: &str) -> CatalogEntry {
        CatalogEntry {
            id: id.into(),
            name: format!("Model {id}"),
            repo: "org/repo".into(),
            revision: "abc".into(),
            file: format!("{id}.gguf"),
            size,
            sha256: sha.into(),
            quant: "Q4_K_M".into(),
            params_b: 0.5,
            capabilities: vec![Capability::Chat],
            license: "apache-2.0".into(),
            notes: String::new(),
        }
    }

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scan_reads_ready_paused_and_missing() {
        let dir = Dir(std::env::temp_dir().join(format!("dn-local-{}", dn_core::Id::new())));
        std::fs::create_dir_all(&dir.0).unwrap();
        std::fs::write(dir.0.join("ready.gguf"), b"12345").unwrap();
        std::fs::write(dir.0.join("ready.gguf.sha256"), "aa\n").unwrap();
        std::fs::write(dir.0.join("half.gguf.part"), b"12").unwrap();
        let catalog = Catalog {
            version: 1,
            models: vec![
                entry("ready", 5, "AA"),
                entry("half", 10, "bb"),
                entry("none", 10, "cc"),
            ],
        };
        let lm = LocalModels::new(dir.0.clone(), catalog, Machine::cpu_only(16 << 30));
        let rows = lm.rows();
        assert_eq!(rows[0].state, DownloadState::Ready);
        assert_eq!(rows[1].state, DownloadState::Paused { done: 2, total: 10 });
        assert_eq!(rows[2].state, DownloadState::NotDownloaded);
        assert_eq!(rows[2].model_ref, "local:none");
        assert!(rows[2].fit.estimated);
        assert_eq!(lm.ready().len(), 1);
    }

    #[test]
    fn small_models_fit_and_huge_ones_do_not() {
        let mut small = entry("s", 500_000_000, "x");
        small.params_b = 0.5;
        let mut huge = entry("h", 80_000_000_000, "x");
        huge.params_b = 140.0;
        let m = Machine::cpu_only(16 << 30);
        assert_eq!(
            dn_models::fit(&estimated_footprint(&small), &m).status,
            FitStatus::Ready
        );
        assert_eq!(
            dn_models::fit(&estimated_footprint(&huge), &m).status,
            FitStatus::MemoryBlocked
        );
    }

    #[test]
    fn licence_warnings() {
        for l in [
            "",
            "unknown",
            "other",
            "cc-by-nc-4.0",
            "CC-BY-NC-SA-4.0",
            "Non-Commercial",
        ] {
            assert!(license_needs_warning(l), "{l}");
        }
        for l in ["apache-2.0", "mit", "llama3.2", "gemma"] {
            assert!(!license_needs_warning(l), "{l}");
        }
    }

    #[test]
    fn errors_are_user_words() {
        let m = download_error_message(&DownloadError::DiskFull {
            need: 4_000_000_000,
            free: 1_000_000_000,
        });
        assert!(m.contains("4.0 GB") && m.contains("1.0 GB"));
        assert!(download_error_message(&DownloadError::Http("x".into())).contains("connection"));
    }

    #[test]
    fn the_builtin_catalogue_loads_with_rows() {
        let lm = LocalModels::new(
            std::env::temp_dir().join("dn-local-empty-dir-that-does-not-exist"),
            Catalog::builtin(),
            Machine::cpu_only(16 << 30),
        );
        let rows = lm.rows();
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| r.state == DownloadState::NotDownloaded));
        assert!(
            rows.iter()
                .all(|r| r.card_url.starts_with("https://huggingface.co/"))
        );
    }
}
