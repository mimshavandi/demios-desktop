//! Local models (T-SHELL-07): the curated catalogue (`dn-models`), resumable verified
//! downloads with progress, fit status per row, and llama.cpp (`dn-inference`) as the
//! `ChatModel` behind `local:<catalogue id>`.
//!
//! Files live in `dn_models::download::default_models_dir()` (shared with the `dn` CLI, so a
//! model pulled there shows up here). One local model is loaded at a time.

use dn_core::chat::ChatModel;
use dn_core::model::{Capability, FitStatus, ModelRef};
use dn_core::{CoreError, RecoveryAction};
use dn_inference::{
    InferenceBackend, KvCacheQuant, LlamaCppBackend, LoadOptions, LoadedModel, ModelSpec,
    MoeOffload,
};
use dn_models::download::DownloadError;
use dn_models::{Catalog, CatalogEntry, FitReport, Gguf, Machine, ModelFootprint};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

/// The catalogue id in `local:<id>`, if `model` is a local model.
pub fn catalogue_id(model: &ModelRef) -> Option<&str> {
    match model {
        ModelRef::Local { model_id } => Some(model_id.as_str()),
        _ => None,
    }
}

/// Where a catalogue model is (design-system §4, Download row).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DownloadState {
    NotDownloaded,
    /// Waiting to start. `resuming`: a partial file is re-checked first.
    Queued {
        resuming: bool,
    },
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

/// Where a local model comes from (T-INF-10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelOrigin {
    /// The built-in catalogue (downloadable from Settings).
    Catalog,
    /// Downloaded from Hugging Face by Demios or the `dn` CLI (`models_dir/hf/…`, verified).
    Hub,
    /// Found in the user's own Hugging Face cache; used in place, never changed.
    HfCache,
}

/// A model outside the catalogue: a Hub download or a Hugging Face cache file.
#[derive(Clone, Debug)]
pub struct External {
    pub entry: CatalogEntry,
    pub path: PathBuf,
    pub origin: ModelOrigin,
    pub card_url: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LocalModelRow {
    pub id: String,
    pub origin: ModelOrigin,
    /// Hub and cache models: where the file is.
    pub location: Option<String>,
    pub model_ref: String,
    pub name: String,
    pub repo: String,
    pub quant: String,
    pub size: u64,
    pub params_b: f64,
    pub capabilities: Vec<Capability>,
    pub license: String,
    /// Missing, non-commercial or custom ("other") licence: the UI warns.
    pub license_warning: bool,
    pub card_url: String,
    pub notes: String,
    pub state: DownloadState,
    pub fit: FitView,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalModelEvent {
    Changed {
        row: Box<LocalModelRow>,
    },
    /// An external model went away (deleted, cache cleaned).
    Removed {
        id: String,
    },
}

type Sink = Arc<dyn Fn(LocalModelEvent) + Send + Sync>;

/// Progress callback handed to a fetch: (bytes done, bytes total).
pub type ProgressFn = Box<dyn FnMut(u64, u64) + Send>;

/// How a catalogue file is fetched. The app uses `dn_models::download::pull`; tests swap in
/// a scripted one.
pub type FetchFn = Arc<
    dyn Fn(
            CatalogEntry,
            PathBuf,
            ProgressFn,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<PathBuf, DownloadError>> + Send>,
        > + Send
        + Sync,
>;

fn pull_fetch() -> FetchFn {
    Arc::new(|entry, dir, mut progress| {
        Box::pin(async move {
            let client = dn_models::download::client()?;
            dn_models::download::pull(&client, &entry, &dir, &mut *progress).await
        })
    })
}

/// A running download. `generation` identifies it: callbacks and the finishing task of an
/// older (paused) run must not touch a newer one.
struct Task {
    generation: u64,
    handle: tauri::async_runtime::JoinHandle<()>,
}

/// A loaded model that is unloaded from its backend when its last user lets go. A switch
/// replaces the app's reference at once; a reply still streaming from the old model keeps it
/// until that reply ends, then its weights are freed (no copies pile up).
pub struct Resident<B: InferenceBackend> {
    model: Option<LoadedModel<B>>,
}

impl<B: InferenceBackend> Resident<B> {
    pub fn new(model: LoadedModel<B>) -> Self {
        Self { model: Some(model) }
    }

    /// Unloads now and waits for it (used when nothing else holds the model).
    pub async fn unload_now(mut self) {
        if let Some(m) = self.model.take()
            && let Err(e) = m.unload().await
        {
            eprintln!("could not unload a local model: {e}");
        }
    }
}

impl<B: InferenceBackend> ChatModel for Resident<B> {
    fn chat(&self, req: dn_core::chat::ChatRequest) -> dn_core::chat::EventStream {
        self.model
            .as_ref()
            .expect("a Resident holds its model until dropped")
            .chat(req)
    }
}

impl<B: InferenceBackend> Drop for Resident<B> {
    fn drop(&mut self) {
        if let Some(m) = self.model.take() {
            tauri::async_runtime::spawn(async move {
                if let Err(e) = m.unload().await {
                    eprintln!("could not unload a local model: {e}");
                }
            });
        }
    }
}

pub struct LocalModels<B: InferenceBackend = LlamaCppBackend> {
    dir: PathBuf,
    catalog: Catalog,
    machine: Machine,
    state: Mutex<HashMap<String, DownloadState>>,
    fits: Mutex<HashMap<String, FitView>>,
    tasks: Mutex<HashMap<String, Task>>,
    /// One download per model at a time across pause/resume: a resumed run waits until the
    /// paused one has fully stopped (and released the file lock).
    model_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    next_generation: AtomicU64,
    fetch: FetchFn,
    sink: Mutex<Option<Sink>>,
    backend: Arc<B>,
    /// Whether `dir` is the app's own default folder. A folder the user chose
    /// (`$DN_MODELS_DIR`) is never removed: "Delete all" deletes only our files in it.
    owns_dir: bool,
    /// The one local model the app holds (others may live on briefly in running replies).
    loaded: tokio::sync::Mutex<Option<(String, Arc<Resident<B>>)>>,
    /// The embedding model for knowledge search (#116), in its own slot so it never evicts
    /// the chat model.
    embedding: tokio::sync::Mutex<Option<(String, Arc<LoadedModel<B>>)>>,
    /// Hub downloads and Hugging Face cache models (see [`Self::refresh_externals`]).
    externals: Mutex<Vec<External>>,
    /// The user's Hugging Face cache, read for models to reuse; `None` = don't look (tests).
    hf_cache: Option<PathBuf>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Total size of the files under `dir` (symlinks are not followed). Missing → 0.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

/// Hub downloads in `models_dir/hf/` (verified; Demios's or the `dn` CLI's), as externals.
/// Id: `hf/<owner>/<name>/<file stem>`.
pub fn hub_downloads(models_dir: &Path) -> Vec<External> {
    dn_models::cache::list_downloaded(models_dir)
        .into_iter()
        .filter_map(|d| {
            let stem = d.path.file_stem()?.to_string_lossy().into_owned();
            if stem.to_ascii_lowercase().contains("mmproj") {
                return None; // vision projectors load next to a model, not as one
            }
            let m = &d.manifest;
            let size = m.files.iter().map(|f| f.size).sum();
            Some(External {
                entry: external_entry(
                    format!("hf/{}/{}", m.repo, split_stem(&stem)),
                    &m.repo,
                    &m.revision,
                    size,
                    m.license.id.clone().unwrap_or_default(),
                    "Downloaded from Hugging Face.",
                ),
                path: d.path,
                origin: ModelOrigin::Hub,
                card_url: m.card_url.clone(),
            })
        })
        .collect()
}

/// GGUF models in the user's Hugging Face cache, used in place (read-only).
/// Id: `hfcache/<owner>/<name>/<variant>`.
pub fn cache_models(found: &[dn_models::cache::CachedModel]) -> Vec<External> {
    found
        .iter()
        .filter(|c| c.variant.kind == dn_models::hub::VariantKind::Model)
        .map(|c| External {
            entry: external_entry(
                format!("hfcache/{}/{}", c.repo, c.variant.name),
                &c.repo,
                &c.revision,
                c.variant.total_size,
                String::new(),
                "From your Hugging Face cache. Demios uses the file where it is and never changes it.",
            ),
            path: c.path.clone(),
            origin: ModelOrigin::HfCache,
            card_url: format!("https://huggingface.co/{}", c.repo),
        })
        .collect()
}

/// What makes two listings the same model file: repo and file name (case-insensitive).
fn dedupe_key(x: &External) -> (String, String) {
    let file = x
        .path
        .file_name()
        .map(|f| f.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    (x.entry.repo.to_ascii_lowercase(), file)
}

/// `name-00001-of-00003` → `name`.
fn split_stem(stem: &str) -> &str {
    match stem.rsplit_once("-of-") {
        Some((head, tail)) if tail.len() == 5 && tail.chars().all(|c| c.is_ascii_digit()) => {
            head.rsplit_once('-').map_or(stem, |(h, _)| h)
        }
        _ => stem,
    }
}

fn external_entry(
    id: String,
    repo: &str,
    revision: &str,
    size: u64,
    license: String,
    notes: &str,
) -> CatalogEntry {
    let name = repo.rsplit('/').next().unwrap_or(repo).to_string();
    let variant = id.rsplit('/').next().unwrap_or("").to_string();
    CatalogEntry {
        quant: dn_models::hub::quant_label(&variant).unwrap_or_default(),
        id,
        name,
        repo: repo.to_string(),
        revision: revision.to_string(),
        file: variant,
        size,
        sha256: String::new(),
        params_b: 0.0,
        capabilities: vec![Capability::Chat],
        license,
        notes: notes.to_string(),
    }
}

/// Licence ids we warn about (any licence is allowed): unknown, non-commercial, or a custom "other" licence.
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
    if report.status != FitStatus::MemoryBlocked {
        if report.moe_preset.is_some() {
            // Mixture-of-experts preset (T-INF-20): experts in RAM, attention on the GPU.
            reason.push_str(
                " To fit, part of this model stays in regular memory instead of on the \
                 graphics chip, so replies are a little slower.",
            );
        } else if report.recommended.placement.is_split() {
            reason.push_str(
                " To fit, part of this model runs on the processor instead of the graphics \
                 chip, so replies are slower.",
            );
        }
    }
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

/// Load settings for the GGUF at `path` on `machine`. llama.cpp's defaults, unless the fit
/// estimator splits the model between the GPU and the CPU (MoE experts in RAM, or some of a
/// dense model's layers on the CPU); then its recommended context, KV type and placement,
/// which were estimated together (T-INF-20).
pub fn load_options(path: &Path, machine: &Machine) -> LoadOptions {
    let Ok(g) = Gguf::open(path) else {
        return LoadOptions::default();
    };
    let fp = ModelFootprint::from_gguf(&g);
    options_for(&dn_models::fit(&fp, machine).recommended, fp.n_layer)
}

fn options_for(rec: &dn_models::fit::Estimate, n_layer: u32) -> LoadOptions {
    if !rec.placement.is_split() {
        return LoadOptions::default();
    }
    LoadOptions {
        n_ctx: Some(rec.n_ctx),
        gpu_layers: rec.placement.gpu_layers,
        moe_offload: match rec.placement.moe_cpu_layers {
            0 => MoeOffload::None,
            k if k >= n_layer => MoeOffload::All,
            k => MoeOffload::FirstLayers(k),
        },
        kv_cache: match rec.kv {
            dn_models::fit::KvType::F16 => KvCacheQuant::F16,
            dn_models::fit::KvType::Q8_0 => KvCacheQuant::Q8_0,
        },
        ..LoadOptions::default()
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
        DownloadError::Status(s) if *s == 401 || *s == 403 => {
            "This model needs access: accept its licence on the model page, or add a Hugging Face token, then retry.".into()
        }
        DownloadError::Status(s) if *s == 429 => {
            "Hugging Face is limiting downloads right now. Wait a few minutes, then retry.".into()
        }
        DownloadError::Status(_) | DownloadError::Http(_) => {
            "Couldn't reach Hugging Face. Check your connection, then retry.".into()
        }
        DownloadError::Io(e) if e.kind() == std::io::ErrorKind::StorageFull => {
            "The disk filled up during the download. Free up space, then retry; it resumes where it stopped.".into()
        }
        DownloadError::Io(_) => "Couldn't write the model file. Check the disk, then retry.".into(),
    }
}

impl LocalModels {
    pub fn new(dir: PathBuf, catalog: Catalog, machine: Machine) -> Self {
        Self::with_fetch(dir, catalog, machine, pull_fetch())
    }

    pub fn with_fetch(dir: PathBuf, catalog: Catalog, machine: Machine, fetch: FetchFn) -> Self {
        Self::with_parts(
            dir,
            catalog,
            machine,
            fetch,
            Arc::new(LlamaCppBackend::new()),
        )
    }

    /// The app's instance: built-in catalogue, shared models dir, this machine. Exact fits
    /// (GGUF header reads) are computed in the background, off the setup thread.
    pub fn open() -> Arc<Self> {
        let mut lm = Self::new(
            dn_models::download::default_models_dir(),
            Catalog::builtin(),
            dn_models::detect_machine(),
        );
        lm.owns_dir = std::env::var_os("DN_MODELS_DIR").is_none();
        lm.hf_cache = dn_models::cache::hf_cache_dir();
        let lm = Arc::new(lm);
        let bg = Arc::clone(&lm);
        tauri::async_runtime::spawn_blocking(move || {
            bg.refresh_exact_fits();
            bg.refresh_externals();
        });
        lm
    }
}

impl<B: InferenceBackend> LocalModels<B> {
    pub fn with_parts(
        dir: PathBuf,
        catalog: Catalog,
        machine: Machine,
        fetch: FetchFn,
        backend: Arc<B>,
    ) -> Self {
        let lm = Self {
            dir,
            catalog,
            machine,
            state: Mutex::new(HashMap::new()),
            fits: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
            model_locks: Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(1),
            fetch,
            sink: Mutex::new(None),
            backend,
            owns_dir: false,
            loaded: tokio::sync::Mutex::new(None),
            embedding: tokio::sync::Mutex::new(None),
            externals: Mutex::new(Vec::new()),
            hf_cache: None,
        };
        lm.scan();
        lm
    }

    fn path(&self, e: &CatalogEntry) -> PathBuf {
        match lock(&self.externals).iter().find(|x| x.entry.id == e.id) {
            Some(x) => x.path.clone(),
            None => self.dir.join(&e.file),
        }
    }

    /// A Hub download or Hugging Face cache model (not the catalogue).
    pub fn is_external(&self, id: &str) -> bool {
        lock(&self.externals).iter().any(|x| x.entry.id == id)
    }

    fn external(&self, id: &str) -> Option<External> {
        lock(&self.externals)
            .iter()
            .find(|x| x.entry.id == id)
            .cloned()
    }

    /// A catalogue entry or an external model.
    pub fn find(&self, id: &str) -> Option<CatalogEntry> {
        self.catalog
            .get(id)
            .cloned()
            .or_else(|| self.external(id).map(|x| x.entry))
    }

    /// Replaces the external models (Hub downloads, cache files): each is ready, with an
    /// exact fit from its GGUF header when it can be read. Blocking (header reads).
    pub fn set_externals(&self, mut list: Vec<External>) {
        let new_ids: std::collections::HashSet<String> =
            list.iter().map(|x| x.entry.id.clone()).collect();
        let gone: Vec<String> = lock(&self.externals)
            .iter()
            .map(|x| x.entry.id.clone())
            .filter(|id| !new_ids.contains(id))
            .collect();
        for id in &gone {
            lock(&self.state).remove(id);
            lock(&self.fits).remove(id);
        }
        for x in &mut list {
            lock(&self.state).insert(x.entry.id.clone(), DownloadState::Ready);
            let g = Gguf::open(&x.path).ok();
            let mut fit = g
                .as_ref()
                .map(|g| {
                    fit_view(
                        &dn_models::fit(&ModelFootprint::from_gguf(g), &self.machine),
                        false,
                    )
                })
                .unwrap_or_else(|| self.estimated_fit(&x.entry));
            // Every local chat model renders with its own template (T-INF-23): one without
            // a template is never offered for chat.
            if let Some(g) = &g {
                if dn_models::template::is_embedding(g) {
                    x.entry.capabilities = vec![Capability::Embedding];
                } else if dn_models::template::source(g, &x.path).is_none() {
                    x.entry.capabilities = vec![];
                    fit.reason = format!(
                        "{}, so it can't be used for chat.",
                        dn_models::template::NO_TEMPLATE_MESSAGE
                    );
                }
            }
            lock(&self.fits).insert(x.entry.id.clone(), fit);
        }
        *lock(&self.externals) = list;
        let rows = self.rows();
        let sink = lock(&self.sink).clone();
        if let Some(sink) = sink {
            // Only models that went away are removed, and before the updates, so a row that
            // is still there is never dropped.
            for id in gone {
                sink(LocalModelEvent::Removed { id });
            }
            for row in rows
                .into_iter()
                .filter(|r| r.origin != ModelOrigin::Catalog)
            {
                sink(LocalModelEvent::Changed { row: Box::new(row) });
            }
        }
    }

    /// Re-reads Hub downloads in the models folder (verified, from Demios or the `dn` CLI)
    /// and GGUF models in the Hugging Face cache. A file that's already listed (same repo and
    /// file name) is listed once: a downloaded catalogue model first, then Hub downloads,
    /// then the cache. Blocking.
    pub fn refresh_externals(&self) {
        let cached = self
            .hf_cache
            .as_deref()
            .map(dn_models::cache::discover)
            .unwrap_or_default();
        let mut seen: std::collections::HashSet<(String, String)> = self
            .catalog
            .models
            .iter()
            .filter(|e| self.is_ready(&e.id))
            .map(|e| (e.repo.to_ascii_lowercase(), e.file.to_ascii_lowercase()))
            .collect();
        let list = hub_downloads(&self.dir)
            .into_iter()
            .chain(cache_models(&cached))
            .filter(|x| seen.insert(dedupe_key(x)))
            .collect();
        self.set_externals(list);
    }

    fn partial_len(&self, e: &CatalogEntry) -> u64 {
        std::fs::metadata(suffixed(&self.path(e), ".part"))
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// Reads what is on disk (cheap: sizes and checksum markers, no hashing): verified file →
    /// Ready; partial file → Paused. Fits start as estimates.
    fn scan(&self) {
        let mut state = lock(&self.state);
        let mut fits = lock(&self.fits);
        for e in &self.catalog.models {
            if !e.has_plain_file_name() {
                continue;
            }
            let p = self.path(e);
            let verified = std::fs::metadata(&p).is_ok_and(|m| m.len() == e.size)
                && std::fs::read_to_string(suffixed(&p, ".sha256"))
                    .is_ok_and(|m| m.trim().eq_ignore_ascii_case(&e.sha256));
            let partial = self.partial_len(e);
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
            fits.insert(e.id.clone(), self.estimated_fit(e));
        }
    }

    fn estimated_fit(&self, e: &CatalogEntry) -> FitView {
        fit_view(
            &dn_models::fit(&estimated_footprint(e), &self.machine),
            true,
        )
    }

    /// Exact fit from the GGUF header (blocking file read).
    fn exact_fit(&self, e: &CatalogEntry) -> Option<FitView> {
        Gguf::open(&self.path(e)).ok().map(|g| {
            fit_view(
                &dn_models::fit(&ModelFootprint::from_gguf(&g), &self.machine),
                false,
            )
        })
    }

    /// Replaces the estimates of downloaded models with exact fits. Blocking.
    pub fn refresh_exact_fits(&self) {
        for e in &self.catalog.models {
            if self.is_ready(&e.id)
                && let Some(view) = self.exact_fit(e)
            {
                lock(&self.fits).insert(e.id.clone(), view);
                self.emit(e);
            }
        }
    }

    fn row(&self, e: &CatalogEntry) -> LocalModelRow {
        let ext = self.external(&e.id);
        let state = lock(&self.state)
            .get(&e.id)
            .cloned()
            .unwrap_or(DownloadState::NotDownloaded);
        let fit = lock(&self.fits)
            .get(&e.id)
            .cloned()
            .unwrap_or_else(|| self.estimated_fit(e));
        LocalModelRow {
            id: e.id.clone(),
            origin: ext.as_ref().map_or(ModelOrigin::Catalog, |x| x.origin),
            location: ext.as_ref().map(|x| x.path.display().to_string()),
            model_ref: format!("local:{}", e.id),
            name: e.name.clone(),
            repo: e.repo.clone(),
            quant: e.quant.clone(),
            size: e.size,
            params_b: e.params_b,
            capabilities: e.capabilities.clone(),
            license: e.license.clone(),
            license_warning: license_needs_warning(&e.license),
            card_url: ext
                .as_ref()
                .map(|x| x.card_url.clone())
                .unwrap_or_else(|| format!("https://huggingface.co/{}", e.repo)),
            notes: e.notes.clone(),
            state,
            fit,
        }
    }

    pub fn rows(&self) -> Vec<LocalModelRow> {
        self.entries().iter().map(|e| self.row(e)).collect()
    }

    pub fn is_ready(&self, id: &str) -> bool {
        matches!(lock(&self.state).get(id), Some(DownloadState::Ready))
    }

    /// The model is too big for this machine: never auto-picked, never loaded.
    pub fn wont_fit(&self, id: &str) -> bool {
        lock(&self.fits)
            .get(id)
            .is_some_and(|f| f.status == FitStatus::MemoryBlocked)
    }

    /// Downloaded models, for the resolver and the slot pickers.
    pub fn ready(&self) -> Vec<CatalogEntry> {
        self.entries()
            .into_iter()
            .filter(|e| self.is_ready(&e.id))
            .collect()
    }

    /// Whether this backend emits tool calls (decides the artifact protocol, T-ART-06).
    pub fn tool_calls(&self) -> bool {
        self.backend.capabilities().tool_calls
    }

    /// The catalogue followed by the external models.
    pub fn entries(&self) -> Vec<CatalogEntry> {
        let mut v = self.catalog.models.clone();
        v.extend(lock(&self.externals).iter().map(|x| x.entry.clone()));
        v
    }

    pub fn name(&self, id: &str) -> Option<String> {
        self.find(id).map(|e| e.name)
    }

    /// Settings › Models opened: subscribe, then re-read Hub downloads (e.g. made by the CLI)
    /// and the Hugging Face cache in the background; changed rows arrive as events.
    pub fn watch_and_refresh(self: &Arc<Self>, sink: Sink) -> Vec<LocalModelRow> {
        let rows = self.watch(sink);
        let me = Arc::clone(self);
        tauri::async_runtime::spawn_blocking(move || me.refresh_externals());
        rows
    }

    pub fn watch(&self, sink: Sink) -> Vec<LocalModelRow> {
        *lock(&self.sink) = Some(sink);
        self.rows()
    }

    fn emit(&self, e: &CatalogEntry) {
        let row = self.row(e);
        let sink = lock(&self.sink).clone();
        if let Some(sink) = sink {
            sink(LocalModelEvent::Changed { row: Box::new(row) });
        }
    }

    fn set(&self, id: &str, s: DownloadState) {
        lock(&self.state).insert(id.to_string(), s);
        if let Some(e) = self.catalog.get(id) {
            self.emit(e);
        }
    }

    fn is_current(&self, id: &str, generation: u64) -> bool {
        lock(&self.tasks)
            .get(id)
            .is_some_and(|t| t.generation == generation)
    }

    /// Updates the row only while `generation` is still the model's running download, so a
    /// paused run's late progress never overwrites "Paused".
    fn set_if_current(&self, id: &str, generation: u64, s: DownloadState) {
        // Check and write under the tasks lock, so a pause can't slip in between.
        {
            let tasks = lock(&self.tasks);
            if !tasks.get(id).is_some_and(|t| t.generation == generation) {
                return;
            }
            lock(&self.state).insert(id.to_string(), s);
        }
        if let Some(e) = self.catalog.get(id) {
            self.emit(e);
        }
    }

    /// Ends `generation`'s registration if it is still the current run; returns whether it
    /// was. Atomic, so a finishing old run can never remove a newer run's task.
    fn finish_run(&self, id: &str, generation: u64) -> bool {
        let mut tasks = lock(&self.tasks);
        if tasks.get(id).is_some_and(|t| t.generation == generation) {
            tasks.remove(id);
            true
        } else {
            false
        }
    }

    fn model_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(lock(&self.model_locks).entry(id.to_string()).or_default())
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
        // Held across spawn + insert, so the task can't check `is_current` before it exists.
        let mut tasks = lock(&self.tasks);
        if tasks.contains_key(id) {
            return Ok(());
        }
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let resuming = self.partial_len(&e) > 0;
        lock(&self.state).insert(id.to_string(), DownloadState::Queued { resuming });
        let this = Arc::clone(self);
        let model_lock = self.model_lock(id);
        let entry = e.clone();
        let handle = tauri::async_runtime::spawn(async move {
            let e = entry;
            // Wait until an earlier (paused) run of this model has fully stopped.
            let _one_at_a_time = model_lock.lock_owned().await;
            if !this.is_current(&e.id, generation) {
                return;
            }
            let result = this.run_download(&e, generation).await;
            // Decided right here, with no await in between (an aborted run can still get
            // this far if the abort lands after its last await).
            let current = this.finish_run(&e.id, generation);
            match result {
                Ok(_) => {
                    // A finished file is finished even if a pause raced it.
                    let lm = Arc::clone(&this);
                    let entry = e.clone();
                    let exact =
                        tauri::async_runtime::spawn_blocking(move || lm.exact_fit(&entry)).await;
                    if let Ok(Some(view)) = exact {
                        lock(&this.fits).insert(e.id.clone(), view);
                    }
                    this.set(&e.id, DownloadState::Ready);
                }
                Err(err) if current => {
                    eprintln!("download of `{}` failed: {err}", e.id);
                    this.set(
                        &e.id,
                        DownloadState::Failed {
                            message: download_error_message(&err),
                            done: this.partial_len(&e),
                            total: e.size,
                        },
                    );
                }
                // Paused (or superseded) while failing: the pause already set the row.
                Err(_) => {}
            }
        });
        tasks.insert(id.to_string(), Task { generation, handle });
        drop(tasks);
        self.emit(&e);
        Ok(())
    }

    async fn run_download(
        self: &Arc<Self>,
        e: &CatalogEntry,
        generation: u64,
    ) -> Result<PathBuf, DownloadError> {
        let this = Arc::clone(self);
        let id = e.id.clone();
        let mut last_emit = Instant::now() - std::time::Duration::from_secs(1);
        let mut window: (Instant, u64) = (Instant::now(), 0);
        let mut rate = 0u64;
        let progress: ProgressFn = Box::new(move |done: u64, total: u64| {
            if done >= total {
                this.set_if_current(&id, generation, DownloadState::Verifying { total });
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
                this.set_if_current(
                    &id,
                    generation,
                    DownloadState::Downloading {
                        done,
                        total,
                        bytes_per_sec: rate,
                    },
                );
            }
        });
        (self.fetch)(e.clone(), self.dir.clone(), progress).await
    }

    /// Pauses a running download: the task is aborted (which drops the transfer and its file
    /// lock) and the partial file is kept. A later download waits for the abort to finish.
    pub fn pause(&self, id: &str) {
        let Some(task) = lock(&self.tasks).remove(id) else {
            return;
        };
        task.handle.abort();
        let Some(e) = self.catalog.get(id) else {
            return;
        };
        self.set(
            id,
            DownloadState::Paused {
                done: self.partial_len(e),
                total: e.size,
            },
        );
    }

    /// Stops every download (app exit). Partial files stay for the next start.
    pub fn pause_all(&self) {
        let ids: Vec<String> = lock(&self.tasks).keys().cloned().collect();
        for id in ids {
            self.pause(&id);
        }
    }

    /// The models folder (shown in Settings › About).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// This computer, for fit checks.
    pub fn machine(&self) -> &Machine {
        &self.machine
    }

    /// Fit in the screen's terms.
    pub fn fit_view_for(&self, report: &FitReport, estimated: bool) -> FitView {
        fit_view(report, estimated)
    }

    /// Marks `dir` as the app's own folder (removed whole by [`Self::delete_all`]).
    pub fn set_owns_dir(&mut self, owns: bool) {
        self.owns_dir = owns;
    }

    /// What the app wrote under the models folder: catalogue files with their partial,
    /// checksum and lock files, and the `hf/` tree of Hub downloads (with sidecar
    /// manifests). Anything else in a user-chosen folder is not ours.
    fn our_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in &self.catalog.models {
            if !e.has_plain_file_name() {
                continue;
            }
            let p = self.path(e);
            for suffix in [".part", ".sha256", ".lock"] {
                out.push(suffixed(&p, suffix));
            }
            out.push(p);
        }
        out.push(self.dir.join("hf"));
        out
    }

    /// Bytes "Delete all" would free: the whole folder when it is ours, else our files in
    /// it. Blocking.
    pub fn disk_usage(&self) -> u64 {
        if self.owns_dir {
            return dir_size(&self.dir);
        }
        self.our_paths()
            .iter()
            .map(|p| match std::fs::symlink_metadata(p) {
                Ok(m) if m.is_dir() => dir_size(p),
                Ok(m) if m.is_file() => m.len(),
                _ => 0,
            })
            .sum()
    }

    /// "Delete all local data": stops every download (waiting until each has let go of its
    /// files), lets go of the loaded model, deletes the models folder (only our files in it
    /// when the user chose the folder) and resets every row to "not downloaded". A reply still streaming keeps its model until it ends; the
    /// caller cancels replies first.
    pub async fn delete_all(&self) -> std::io::Result<()> {
        self.pause_all();
        let locks: Vec<_> = lock(&self.model_locks).values().cloned().collect();
        for l in locks {
            drop(l.lock().await);
        }
        if let Some((_, m)) = self.loaded.lock().await.take() {
            match Arc::try_unwrap(m) {
                Ok(idle) => idle.unload_now().await,
                Err(in_use) => drop(in_use),
            }
        }
        if let Some((_, m)) = self.embedding.lock().await.take()
            && let Ok(idle) = Arc::try_unwrap(m)
            && let Err(e) = idle.unload().await
        {
            eprintln!("could not unload the embedding model: {e}");
        }
        let ignore_missing = |r: std::io::Result<()>| match r {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
        if self.owns_dir {
            ignore_missing(std::fs::remove_dir_all(&self.dir))?;
        } else {
            for p in self.our_paths() {
                match std::fs::symlink_metadata(&p) {
                    Ok(m) if m.is_dir() => ignore_missing(std::fs::remove_dir_all(&p))?,
                    Ok(_) => ignore_missing(std::fs::remove_file(&p))?,
                    Err(_) => {}
                }
            }
        }
        self.scan();
        // Hub downloads went with the folder; cache models (not ours) stay listed.
        self.refresh_externals();
        for e in &self.catalog.models {
            self.emit(e);
        }
        Ok(())
    }

    /// The `ChatModel` for `local:<id>`: loads the file on first use. Switching models drops
    /// the app's reference to the previous one; it is unloaded as soon as no reply uses it.
    pub async fn chat_model(&self, id: &str) -> Result<Arc<dyn ChatModel>, CoreError> {
        let e = self.find(id).ok_or_else(|| CoreError::ModelUnavailable {
            user_message: format!("`local:{id}` isn't a model this app knows."),
            developer_message: format!("unknown catalogue id {id}"),
            recovery: vec![],
        })?;
        if !e.capabilities.contains(&Capability::Chat) {
            return Err(CoreError::ModelUnavailable {
                user_message: format!("{} can't chat. Choose a chat model.", e.name),
                developer_message: format!("{id}: not a chat model ({:?})", e.capabilities),
                recovery: vec![RecoveryAction::ChooseModelForSlot {
                    agent_id: crate::agent::AGENT_ID.into(),
                    slot_id: "primary".into(),
                }],
            });
        }
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
        if self.wont_fit(id) {
            return Err(CoreError::ModelUnavailable {
                user_message: format!(
                    "{} won't fit in this computer's memory. Choose a smaller model.",
                    e.name
                ),
                developer_message: format!("{id}: fit MemoryBlocked"),
                recovery: vec![RecoveryAction::ChooseModelForSlot {
                    agent_id: crate::agent::AGENT_ID.into(),
                    slot_id: "primary".into(),
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
        // Let go of the previous model first. If no reply holds it, unload it and wait, so
        // its weights are freed before the new ones are read; otherwise it unloads when the
        // last reply using it ends (Resident's Drop).
        if let Some((_, old)) = loaded.take() {
            match Arc::try_unwrap(old) {
                Ok(idle) => idle.unload_now().await,
                Err(in_use) => drop(in_use),
            }
        }
        let path = self.path(&e);
        let opts = {
            let (path, machine) = (path.clone(), self.machine);
            tauri::async_runtime::spawn_blocking(move || load_options(&path, &machine))
                .await
                .unwrap_or_default()
        };
        let m = LoadedModel::load(Arc::clone(&self.backend), &ModelSpec::gguf(path), &opts)
            .await
            .map_err(|err| CoreError::ModelUnavailable {
                user_message: format!("{} couldn't be loaded. It may not fit in memory.", e.name),
                developer_message: format!("load {id}: {err}"),
                recovery: vec![RecoveryAction::ChooseModelForSlot {
                    agent_id: crate::agent::AGENT_ID.into(),
                    slot_id: "primary".into(),
                }],
            })?;
        let m = Arc::new(Resident::new(m));
        *loaded = Some((id.to_string(), Arc::clone(&m)));
        let m: Arc<dyn ChatModel> = m;
        Ok(m)
    }

    /// The embedding model `local:<id>` for knowledge search (#116): loaded on first use and
    /// kept in its own slot, so it never evicts the chat model. Same refusals as
    /// [`Self::chat_model`]: not an embedding model, not downloaded, or won't fit.
    pub async fn embedding_model(&self, id: &str) -> Result<Arc<LoadedModel<B>>, CoreError> {
        let unavailable = |user: String, dev: String| CoreError::ModelUnavailable {
            user_message: user,
            developer_message: dev,
            recovery: vec![RecoveryAction::ChooseModelForSlot {
                agent_id: crate::agent::AGENT_ID.into(),
                slot_id: "embed".into(),
            }],
        };
        let e = self.find(id).ok_or_else(|| {
            unavailable(
                format!("`local:{id}` isn't a model this app knows."),
                format!("unknown catalogue id {id}"),
            )
        })?;
        if !e.capabilities.contains(&Capability::Embedding) {
            return Err(unavailable(
                format!(
                    "{} isn't an embedding model. Choose one for Embeddings.",
                    e.name
                ),
                format!("{id}: not an embedding model ({:?})", e.capabilities),
            ));
        }
        if !self.is_ready(id) {
            return Err(unavailable(
                format!(
                    "{} isn't downloaded yet. Download it in Settings → Models.",
                    e.name
                ),
                format!("{id} not downloaded"),
            ));
        }
        if self.wont_fit(id) {
            return Err(unavailable(
                format!("{} won't fit in this computer's memory.", e.name),
                format!("{id}: fit MemoryBlocked"),
            ));
        }
        let mut slot = self.embedding.lock().await;
        if let Some((cur, m)) = slot.as_ref()
            && cur == id
        {
            return Ok(Arc::clone(m));
        }
        if let Some((_, old)) = slot.take()
            && let Ok(idle) = Arc::try_unwrap(old)
            && let Err(err) = idle.unload().await
        {
            eprintln!("could not unload the embedding model: {err}");
        }
        let path = self.path(&e);
        let opts = {
            let (path, machine) = (path.clone(), self.machine);
            tauri::async_runtime::spawn_blocking(move || load_options(&path, &machine))
                .await
                .unwrap_or_default()
        };
        let m = LoadedModel::load(Arc::clone(&self.backend), &ModelSpec::gguf(path), &opts)
            .await
            .map_err(|err| {
                unavailable(
                    format!("{} couldn't be loaded.", e.name),
                    format!("load {id}: {err}"),
                )
            })?;
        let m = Arc::new(m);
        *slot = Some((id.to_string(), Arc::clone(&m)));
        Ok(m)
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

    /// Qwen3-30B-A3B-like MoE footprint (18.6 GB, 48 layers, ~94% experts).
    fn moe_30b() -> ModelFootprint {
        ModelFootprint {
            weight_bytes: 18_600_000_000,
            expert_bytes: 17_500_000_000,
            n_params: 30_500_000_000,
            n_layer: 48,
            n_ctx_train: 40_960,
            n_vocab: 151_936,
            n_embd: 2048,
            kv_elems_per_token: 48 * 4 * 256,
            swa_kv_elems_per_token: 0,
            sliding_window: 0,
            quant: Some("Q4_K_M".into()),
        }
    }

    #[test]
    fn moe_preset_sets_load_options_and_explains_itself() {
        let gib = dn_models::fit::GIB;
        let m = moe_30b();
        // 8 GB GPU + 32 GB RAM: some experts in RAM.
        let r = dn_models::fit(&m, &Machine::discrete(32 * gib, 8 * gib));
        let o = options_for(&r.recommended, m.n_layer);
        assert_eq!(
            o.moe_offload,
            MoeOffload::FirstLayers(r.recommended.placement.moe_cpu_layers)
        );
        assert_eq!(o.n_ctx, Some(r.recommended.n_ctx));
        assert_eq!(o.gpu_layers, None);
        assert!(fit_view(&r, false).reason.contains("regular memory"));
        // 2 GB GPU: every layer's experts in RAM.
        let r = dn_models::fit(&m, &Machine::discrete(32 * gib, 2 * gib));
        assert_eq!(
            options_for(&r.recommended, m.n_layer).moe_offload,
            MoeOffload::All
        );
        // All on the GPU: llama.cpp defaults, nothing extra to explain.
        let r = dn_models::fit(&m, &Machine::unified(64 * gib));
        assert_eq!(
            options_for(&r.recommended, m.n_layer),
            LoadOptions::default()
        );
        assert!(!fit_view(&r, false).reason.contains("regular memory"));
        // A model that won't fit isn't explained as a split.
        let r = dn_models::fit(&m, &Machine::unified(8 * gib));
        assert_eq!(r.status, FitStatus::MemoryBlocked);
        assert!(!fit_view(&r, false).reason.contains("To fit"));
    }

    #[test]
    fn unreadable_gguf_loads_with_defaults() {
        let gib = dn_models::fit::GIB;
        assert_eq!(
            load_options(Path::new("/nonexistent.gguf"), &Machine::unified(16 * gib)),
            LoadOptions::default()
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

    // ---- review fixes: model unload, download races, fit, errors

    use dn_inference::{BackendCaps, ModelHandle, TokenStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    /// Backend that counts loads and unloads.
    #[derive(Default)]
    struct Counting {
        loads: AtomicUsize,
        unloads: AtomicUsize,
    }

    impl InferenceBackend for Counting {
        fn name(&self) -> &'static str {
            "counting"
        }
        fn capabilities(&self) -> BackendCaps {
            BackendCaps::default()
        }
        async fn load(
            &self,
            spec: &ModelSpec,
            _: &LoadOptions,
        ) -> dn_inference::Result<ModelHandle> {
            let n = self.loads.fetch_add(1, Ordering::SeqCst) as u64;
            Ok(ModelHandle::new(n + 1, spec.model_id.clone()))
        }
        async fn chat(
            &self,
            h: &ModelHandle,
            _: dn_core::chat::ChatRequest,
        ) -> dn_inference::Result<TokenStream> {
            Err(dn_inference::InferenceError::UnknownHandle(h.id()))
        }
        async fn embed(
            &self,
            _: &ModelHandle,
            _: &[String],
        ) -> dn_inference::Result<Vec<Vec<f32>>> {
            Ok(vec![])
        }
        async fn unload(&self, _: ModelHandle) -> dn_inference::Result<()> {
            self.unloads.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_switched_model_unloads_once_its_last_reply_ends() {
        let backend = Arc::new(Counting::default());
        let first = LoadedModel::load(
            Arc::clone(&backend),
            &ModelSpec::gguf("a.gguf"),
            &LoadOptions::default(),
        )
        .await
        .unwrap();
        let app_ref: Arc<dyn ChatModel> = Arc::new(Resident::new(first));
        // A reply in flight holds its own reference.
        let in_flight = Arc::clone(&app_ref);
        // The user switches models: the app lets go of the old one.
        drop(app_ref);
        settle().await;
        assert_eq!(
            backend.unloads.load(Ordering::SeqCst),
            0,
            "still in use by the reply"
        );
        drop(in_flight);
        settle().await;
        assert_eq!(
            backend.unloads.load(Ordering::SeqCst),
            1,
            "unloaded when the reply ended"
        );

        // Switching back and forth never accumulates loaded copies.
        for _ in 0..3 {
            let m = LoadedModel::load(
                Arc::clone(&backend),
                &ModelSpec::gguf("b.gguf"),
                &LoadOptions::default(),
            )
            .await
            .unwrap();
            drop(Arc::new(Resident::new(m)));
        }
        settle().await;
        assert_eq!(
            backend.loads.load(Ordering::SeqCst),
            backend.unloads.load(Ordering::SeqCst),
            "every load is matched by an unload"
        );
    }

    // Deterministic interleavings: each test fails if its fix is removed.

    type Behaviour = Arc<
        dyn Fn(
                u64,
                CatalogEntry,
                PathBuf,
                ProgressFn,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<PathBuf, DownloadError>> + Send>,
            > + Send
            + Sync,
    >;

    /// A fetch whose n-th call (1-based) behaves as `behaviour` says.
    fn scripted(behaviour: Behaviour) -> FetchFn {
        let calls = Arc::new(AtomicU64::new(0));
        Arc::new(move |e, dir, progress| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            behaviour(n, e, dir, progress)
        })
    }

    fn racing(fetch: FetchFn) -> (Dir, Arc<LocalModels>) {
        let dir = Dir(std::env::temp_dir().join(format!("dn-local-race-{}", dn_core::Id::new())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let catalog = Catalog {
            version: 1,
            models: vec![entry("m", 1_000, "aa")],
        };
        let lm = Arc::new(LocalModels::with_fetch(
            dir.0.clone(),
            catalog,
            Machine::cpu_only(16 << 30),
            fetch,
        ));
        (dir, lm)
    }

    fn state_of(lm: &LocalModels) -> DownloadState {
        lm.rows()[0].state.clone()
    }

    async fn until(mut f: impl FnMut() -> bool) {
        for _ in 0..200 {
            if f() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("condition not reached");
    }

    /// Fails without the generation check in `set_if_current`.
    #[tokio::test(flavor = "multi_thread")]
    async fn progress_delivered_after_pause_never_overwrites_paused() {
        let release = Arc::new(tokio::sync::Notify::new());
        let r = Arc::clone(&release);
        let (_d, lm) = racing(scripted(Arc::new(move |_, e, _, mut progress| {
            let r = Arc::clone(&r);
            Box::pin(async move {
                // The callback outlives the (aborted) transfer and fires only after the pause.
                tokio::spawn(async move {
                    r.notified().await;
                    progress(e.size / 2, e.size);
                });
                std::future::pending().await
            })
        })));
        lm.download("m").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        lm.pause("m");
        assert!(matches!(state_of(&lm), DownloadState::Paused { .. }));
        release.notify_one();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert!(
            matches!(state_of(&lm), DownloadState::Paused { .. }),
            "{:?}",
            state_of(&lm)
        );
    }

    /// File lock like dn_models::download's; dropping it takes a while (as releasing a real
    /// lock and closing a file can), so a resume that doesn't wait sees it still held.
    struct SlowLock(Arc<AtomicBool>);
    impl Drop for SlowLock {
        fn drop(&mut self) {
            std::thread::sleep(std::time::Duration::from_millis(200));
            self.0.store(false, Ordering::SeqCst);
        }
    }

    /// Fails without the per-model lock wait.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_resume_right_after_pause_waits_for_the_file_lock() {
        let held = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));
        let (h, b) = (Arc::clone(&held), Arc::clone(&busy));
        let (_d, lm) = racing(scripted(Arc::new(move |n, e, dir, mut progress| {
            let (held, busy) = (Arc::clone(&h), Arc::clone(&b));
            Box::pin(async move {
                if held.swap(true, Ordering::SeqCst) {
                    busy.store(true, Ordering::SeqCst);
                    return Err(DownloadError::Busy(dir.join(&e.file)));
                }
                let _lock = SlowLock(held);
                if n == 1 {
                    std::future::pending::<()>().await;
                }
                progress(e.size, e.size);
                Ok(dir.join(&e.file))
            })
        })));
        lm.download("m").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        lm.pause("m");
        lm.download("m").unwrap();
        until(|| {
            state_of(&lm) == DownloadState::Ready
                || matches!(state_of(&lm), DownloadState::Failed { .. })
        })
        .await;
        assert!(
            !busy.load(Ordering::SeqCst),
            "resume hit the old run's file lock"
        );
        assert_eq!(state_of(&lm), DownloadState::Ready);
    }

    /// Fails without the generation check when a finishing run removes its task.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_paused_run_that_finishes_late_leaves_the_new_run_registered() {
        let at_end = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(std::sync::Barrier::new(2));
        let (a, g) = (Arc::clone(&at_end), Arc::clone(&gate));
        let (_d, lm) = racing(scripted(Arc::new(move |n, e, dir, _progress| {
            let (at_end, gate) = (Arc::clone(&a), Arc::clone(&g));
            Box::pin(async move {
                if n == 1 {
                    // Finish synchronously (no await), so an abort can't stop it.
                    at_end.store(true, Ordering::SeqCst);
                    gate.wait();
                    return Ok(dir.join(&e.file));
                }
                std::future::pending().await
            })
        })));
        lm.download("m").unwrap();
        until(|| at_end.load(Ordering::SeqCst)).await;
        lm.pause("m");
        lm.download("m").unwrap();
        let second = lock(&lm.tasks)
            .get("m")
            .map(|t| t.generation)
            .expect("new run");
        let g = Arc::clone(&gate);
        tokio::task::spawn_blocking(move || g.wait()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            lock(&lm.tasks).get("m").map(|t| t.generation),
            Some(second),
            "the old run removed the new run's task"
        );
        lm.pause("m");
    }

    /// Records what the backend did, in order.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl InferenceBackend for Recorder {
        fn name(&self) -> &'static str {
            "recorder"
        }
        fn capabilities(&self) -> BackendCaps {
            BackendCaps::default()
        }
        async fn load(
            &self,
            spec: &ModelSpec,
            _: &LoadOptions,
        ) -> dn_inference::Result<ModelHandle> {
            let mut v = lock(&self.0);
            v.push(format!("load {}", spec.model_id));
            Ok(ModelHandle::new(v.len() as u64, spec.model_id.clone()))
        }
        async fn chat(
            &self,
            h: &ModelHandle,
            _: dn_core::chat::ChatRequest,
        ) -> dn_inference::Result<TokenStream> {
            Err(dn_inference::InferenceError::UnknownHandle(h.id()))
        }
        async fn embed(
            &self,
            _: &ModelHandle,
            _: &[String],
        ) -> dn_inference::Result<Vec<Vec<f32>>> {
            Ok(vec![])
        }
        async fn unload(&self, h: ModelHandle) -> dn_inference::Result<()> {
            lock(&self.0).push(format!("unload {}", h.model_id));
            Ok(())
        }
    }

    fn two_ready_models() -> (Dir, Arc<Recorder>, LocalModels<Recorder>) {
        let dir = Dir(std::env::temp_dir().join(format!("dn-local-switch-{}", dn_core::Id::new())));
        std::fs::create_dir_all(&dir.0).unwrap();
        for id in ["a", "b"] {
            std::fs::write(dir.0.join(format!("{id}.gguf")), b"12345").unwrap();
            std::fs::write(dir.0.join(format!("{id}.gguf.sha256")), "aa").unwrap();
        }
        let rec = Arc::new(Recorder::default());
        let lm = LocalModels::with_parts(
            dir.0.clone(),
            Catalog {
                version: 1,
                models: vec![entry("a", 5, "aa"), entry("b", 5, "aa")],
            },
            Machine::cpu_only(16 << 30),
            pull_fetch(),
            Arc::clone(&rec),
        );
        (dir, rec, lm)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn switching_from_an_idle_model_unloads_it_before_loading_the_next() {
        let (_d, rec, lm) = two_ready_models();
        drop(lm.chat_model("a").await.unwrap());
        drop(lm.chat_model("b").await.unwrap());
        assert_eq!(*lock(&rec.0), ["load a", "unload a", "load b"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_all_unloads_and_removes_the_models_folder() {
        let (d, rec, mut lm) = two_ready_models();
        lm.set_owns_dir(true);
        std::fs::create_dir_all(d.0.join("hf")).unwrap();
        std::fs::write(d.0.join("hf").join("x.gguf"), b"123").unwrap();
        assert_eq!(lm.disk_usage(), 5 + 2 + 5 + 2 + 3);
        drop(lm.chat_model("a").await.unwrap());
        lm.delete_all().await.unwrap();
        assert_eq!(lm.disk_usage(), 0);
        assert_eq!(*lock(&rec.0), ["load a", "unload a"]);
        assert!(!d.0.exists());
        assert!(
            lm.rows()
                .iter()
                .all(|r| r.state == DownloadState::NotDownloaded)
        );
        // Deleting again (nothing there) is fine.
        lm.delete_all().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_all_in_a_user_chosen_folder_keeps_their_files() {
        // `$DN_MODELS_DIR`: the folder is the user's, not ours.
        let (d, _rec, lm) = two_ready_models();
        std::fs::write(d.0.join("a.gguf.part"), b"12").unwrap();
        std::fs::create_dir_all(d.0.join("hf").join("org--m")).unwrap();
        std::fs::write(d.0.join("hf").join("org--m").join("m.gguf"), b"123").unwrap();
        std::fs::write(d.0.join("my-notes.txt"), b"keep me").unwrap();
        std::fs::create_dir_all(d.0.join("other")).unwrap();
        std::fs::write(d.0.join("other").join("x.gguf"), b"keep").unwrap();
        // Ours: a, b (5 each) + 2 checksum files (2 each) + a.part (2) + hf (3).
        assert_eq!(lm.disk_usage(), 5 + 5 + 2 + 2 + 2 + 3);

        lm.delete_all().await.unwrap();
        assert!(d.0.join("my-notes.txt").exists());
        assert!(d.0.join("other").join("x.gguf").exists());
        for gone in ["a.gguf", "a.gguf.sha256", "a.gguf.part", "b.gguf", "hf"] {
            assert!(!d.0.join(gone).exists(), "{gone}");
        }
        assert_eq!(lm.disk_usage(), 0);
        assert!(
            lm.rows()
                .iter()
                .all(|r| r.state == DownloadState::NotDownloaded)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn switching_during_a_reply_unloads_the_old_model_when_the_reply_ends() {
        let (_d, rec, lm) = two_ready_models();
        let in_reply = lm.chat_model("a").await.unwrap();
        drop(lm.chat_model("b").await.unwrap());
        assert_eq!(*lock(&rec.0), ["load a", "load b"], "a is still answering");
        drop(in_reply);
        settle().await;
        assert_eq!(*lock(&rec.0), ["load a", "load b", "unload a"]);
    }

    #[test]
    fn an_embedding_model_is_never_loaded_for_chat() {
        let dir = Dir(std::env::temp_dir().join(format!("dn-local-emb-{}", dn_core::Id::new())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let mut emb = entry("emb", 5, "aa");
        emb.capabilities = vec![Capability::Embedding];
        std::fs::write(dir.0.join("emb.gguf"), b"12345").unwrap();
        std::fs::write(dir.0.join("emb.gguf.sha256"), "aa").unwrap();
        let lm = LocalModels::new(
            dir.0.clone(),
            Catalog {
                version: 1,
                models: vec![emb],
            },
            Machine::cpu_only(16 << 30),
        );
        assert!(lm.is_ready("emb"));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(lm.chat_model("emb")).err().expect("refused");
        assert!(
            matches!(err, CoreError::ModelUnavailable { ref user_message, .. } if user_message.contains("can't chat"))
        );
        // The resolver never picks it for chat, even as the only ready local model.
        let mut reg = crate::models::ModelRegistry::default();
        reg.local = Some(Arc::new(lm));
        let keys = crate::secrets::Keys::memory();
        let src = reg.sources(&keys);
        let s = src
            .iter()
            .find(|s| s.model.to_string() == "local:emb")
            .unwrap();
        assert_eq!(s.capabilities, [Capability::Embedding]);
    }

    #[test]
    fn a_model_that_wont_fit_is_not_offered_or_loaded() {
        let dir = Dir(std::env::temp_dir().join(format!("dn-local-fit-{}", dn_core::Id::new())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let mut huge = entry("huge", 5, "aa");
        huge.params_b = 400.0;
        huge.size = 5;
        std::fs::write(dir.0.join("huge.gguf"), b"12345").unwrap();
        std::fs::write(dir.0.join("huge.gguf.sha256"), "aa").unwrap();
        let lm = LocalModels::new(
            dir.0.clone(),
            Catalog {
                version: 1,
                models: vec![huge],
            },
            Machine::cpu_only(1 << 30),
        );
        // Force the exact-fit outcome a real 400B GGUF would give.
        lock(&lm.fits).insert(
            "huge".into(),
            FitView {
                status: FitStatus::MemoryBlocked,
                estimated: false,
                reason: String::new(),
                suggested_quant: None,
            },
        );
        assert!(lm.is_ready("huge"));
        assert!(lm.wont_fit("huge"));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(lm.chat_model("huge")).err().expect("refused");
        assert!(
            matches!(err, CoreError::ModelUnavailable { ref user_message, .. } if user_message.contains("won't fit"))
        );

        let mut reg = crate::models::ModelRegistry::default();
        reg.local = Some(Arc::new(lm));
        let keys = crate::secrets::Keys::memory();
        let src = reg.sources(&keys);
        let s = src
            .iter()
            .find(|s| s.model.to_string() == "local:huge")
            .unwrap();
        assert_eq!(s.readiness, dn_core::model::Readiness::Disabled);
    }

    #[test]
    fn access_and_storage_errors_say_what_to_do() {
        assert!(download_error_message(&DownloadError::Status(401)).contains("Hugging Face token"));
        assert!(download_error_message(&DownloadError::Status(403)).contains("licence"));
        let full = std::io::Error::new(std::io::ErrorKind::StorageFull, "full");
        assert!(download_error_message(&DownloadError::Io(full)).contains("disk filled up"));
    }

    // ---- Hub downloads and Hugging Face cache models (T-INF-10)

    /// Writes a verified Hub download the way `HubClient::pull` leaves it.
    fn fake_hub_download(dir: &Path, repo: &str, file: &str, bytes: &[u8]) -> PathBuf {
        let sha = "ab".repeat(32);
        let path = dn_models::hub::HubClient::local_path(dir, repo, file).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        std::fs::write(suffixed(&path, ".sha256"), &sha).unwrap();
        let manifest = dn_models::hub::SourceManifest {
            source: "huggingface".into(),
            endpoint: "https://huggingface.co".into(),
            repo: repo.into(),
            revision: "c".repeat(40),
            files: vec![dn_models::hub::RepoFile {
                path: file.into(),
                size: bytes.len() as u64,
                sha256: Some(sha),
            }],
            license: dn_models::hub::LicenseInfo::classify(Some("apache-2.0"), None, None),
            card_url: format!("https://huggingface.co/{repo}"),
            downloaded_at: 0,
            chat_template: None,
        };
        std::fs::write(
            dn_models::hub::SourceManifest::path_for(&path),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        path
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hub_downloads_from_the_app_or_cli_are_listed_and_loadable() {
        let (d, rec, mut lm) = two_ready_models();
        lm.set_owns_dir(true);
        let path = fake_hub_download(&d.0, "org/Tiny-GGUF", "Tiny-Q4_K_M.gguf", b"1234567");
        // An unverified file (no marker) next to it is ignored.
        let bad = dn_models::hub::HubClient::local_path(&d.0, "org/Tiny-GGUF", "Bad.gguf").unwrap();
        std::fs::write(&bad, b"x").unwrap();

        lm.refresh_externals();
        let row = lm
            .rows()
            .into_iter()
            .find(|r| r.origin == ModelOrigin::Hub)
            .expect("listed");
        assert_eq!(row.id, "hf/org/Tiny-GGUF/Tiny-Q4_K_M");
        assert_eq!(row.model_ref, "local:hf/org/Tiny-GGUF/Tiny-Q4_K_M");
        assert_eq!(row.state, DownloadState::Ready);
        assert_eq!(row.quant, "Q4_K_M");
        assert_eq!(row.license, "apache-2.0");
        assert_eq!(
            row.location.as_deref(),
            Some(path.display().to_string().as_str())
        );
        assert_eq!(
            lm.rows()
                .iter()
                .filter(|r| r.origin == ModelOrigin::Hub)
                .count(),
            1
        );
        assert!(
            lm.ready().iter().any(|e| e.id == row.id),
            "offered in the pickers"
        );

        drop(lm.chat_model(&row.id).await.unwrap());
        assert!(lock(&rec.0).iter().any(|l| l.starts_with("load ")));

        // "Delete all local data" removes the download and its row.
        lm.delete_all().await.unwrap();
        assert!(lm.rows().iter().all(|r| r.origin != ModelOrigin::Hub));
        assert!(lm.chat_model(&row.id).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_download_without_a_chat_template_is_not_a_chat_model() {
        use dn_models::gguf::write::{V, gguf};
        let tiny = |extra: Vec<(&'static str, V<'static>)>| {
            let mut kv = vec![("general.architecture", V::Str("llama"))];
            kv.extend(extra);
            gguf(&kv, &[("blk.0.attn_q.weight", &[8, 8], 0, 64)])
        };
        let (d, _rec, lm) = two_ready_models();
        fake_hub_download(&d.0, "org/NoTpl-GGUF", "NoTpl-Q4_K_M.gguf", &tiny(vec![]));
        fake_hub_download(
            &d.0,
            "org/Tpl-GGUF",
            "Tpl-Q4_K_M.gguf",
            &tiny(vec![("tokenizer.chat_template", V::Str("{{ messages }}"))]),
        );
        let side = fake_hub_download(&d.0, "org/Side-GGUF", "Side-Q4_K_M.gguf", &tiny(vec![]));
        std::fs::write(dn_models::template::sidecar_path(&side), "{{ messages }}").unwrap();
        fake_hub_download(
            &d.0,
            "org/Emb-GGUF",
            "Emb-Q8_0.gguf",
            &tiny(vec![("llama.pooling_type", V::U32(1))]),
        );
        lm.refresh_externals();
        let row = |id: &str| lm.rows().into_iter().find(|r| r.id == id).unwrap();
        let none = row("hf/org/NoTpl-GGUF/NoTpl-Q4_K_M");
        assert!(none.capabilities.is_empty());
        assert!(
            none.fit.reason.contains("no chat template"),
            "{}",
            none.fit.reason
        );
        let err = lm.chat_model(&none.id).await.err().expect("refused");
        assert!(matches!(err, CoreError::ModelUnavailable { .. }));
        assert_eq!(
            row("hf/org/Tpl-GGUF/Tpl-Q4_K_M").capabilities,
            [Capability::Chat]
        );
        assert_eq!(
            row("hf/org/Side-GGUF/Side-Q4_K_M").capabilities,
            [Capability::Chat]
        );
        assert_eq!(
            row("hf/org/Emb-GGUF/Emb-Q8_0").capabilities,
            [Capability::Embedding]
        );
    }

    #[test]
    fn template_file_names_agree() {
        assert_eq!(
            dn_models::template::SUFFIX,
            dn_inference::llamacpp::CHAT_TEMPLATE_SUFFIX
        );
    }

    #[test]
    fn cache_models_are_used_in_place_and_projectors_skipped() {
        let v = |name: &str, kind| dn_models::hub::GgufVariant {
            name: name.into(),
            quant: dn_models::hub::quant_label(name),
            kind,
            files: vec![],
            total_size: 42,
        };
        let found = vec![
            dn_models::cache::CachedModel {
                repo: "org/M".into(),
                revision: "d".repeat(40),
                refs: vec!["main".into()],
                variant: v("M-Q8_0", dn_models::hub::VariantKind::Model),
                path: PathBuf::from("/cache/models--org--M/snapshots/x/M-Q8_0.gguf"),
            },
            dn_models::cache::CachedModel {
                repo: "org/M".into(),
                revision: "d".repeat(40),
                refs: vec![],
                variant: v("mmproj-M", dn_models::hub::VariantKind::Projector),
                path: PathBuf::from("/cache/x/mmproj.gguf"),
            },
        ];
        let ext = cache_models(&found);
        assert_eq!(ext.len(), 1);
        assert_eq!(ext[0].entry.id, "hfcache/org/M/M-Q8_0");
        assert_eq!(ext[0].origin, ModelOrigin::HfCache);
        assert_eq!(ext[0].entry.quant, "Q8_0");
        assert!(ext[0].entry.notes.contains("never changes it"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_all_never_touches_the_hugging_face_cache() {
        let (d, _rec, mut lm) = two_ready_models();
        lm.set_owns_dir(true);
        let cache = Dir(std::env::temp_dir().join(format!("dn-hfcache-{}", dn_core::Id::new())));
        let file = cache.0.join("M-Q8_0.gguf");
        std::fs::create_dir_all(&cache.0).unwrap();
        std::fs::write(&file, b"weights").unwrap();
        lm.set_externals(cache_models(&[dn_models::cache::CachedModel {
            repo: "org/M".into(),
            revision: "d".repeat(40),
            refs: vec![],
            variant: dn_models::hub::GgufVariant {
                name: "M-Q8_0".into(),
                quant: Some("Q8_0".into()),
                kind: dn_models::hub::VariantKind::Model,
                files: vec![],
                total_size: 7,
            },
            path: file.clone(),
        }]));
        assert!(lm.is_ready("hfcache/org/M/M-Q8_0"));
        lm.delete_all().await.unwrap();
        assert!(!d.0.exists());
        assert!(file.exists(), "the user's cache is never changed");
    }

    /// Collects local-model events.
    fn recording(lm: &LocalModels<Recorder>) -> Arc<Mutex<Vec<LocalModelEvent>>> {
        let events = Arc::new(Mutex::new(Vec::new()));
        let e2 = Arc::clone(&events);
        lm.watch(Arc::new(move |ev| lock(&e2).push(ev)));
        events
    }

    #[test]
    fn refreshing_twice_keeps_every_row_and_removes_only_what_went() {
        let (d, _rec, lm) = two_ready_models();
        let a = fake_hub_download(&d.0, "org/Tiny-GGUF", "Tiny-Q4_K_M.gguf", b"1234567");
        fake_hub_download(&d.0, "org/Tiny-GGUF", "Tiny-Q8_0.gguf", b"12345678");
        let events = recording(&lm);
        lm.refresh_externals();
        lm.refresh_externals();
        let removed = |evs: &[LocalModelEvent]| -> Vec<String> {
            evs.iter()
                .filter_map(|e| match e {
                    LocalModelEvent::Removed { id } => Some(id.clone()),
                    _ => None,
                })
                .collect()
        };
        assert!(removed(&lock(&events)).is_empty(), "nothing went away");
        assert_eq!(
            lm.rows()
                .iter()
                .filter(|r| r.origin == ModelOrigin::Hub)
                .count(),
            2
        );

        // One file goes: only it is removed, before the updates of the others.
        std::fs::remove_file(&a).unwrap();
        lock(&events).clear();
        lm.refresh_externals();
        let evs = lock(&events);
        assert_eq!(removed(&evs), ["hf/org/Tiny-GGUF/Tiny-Q4_K_M"]);
        assert!(
            matches!(evs[0], LocalModelEvent::Removed { .. }),
            "removals come first"
        );
        assert!(evs.iter().any(|e| matches!(e, LocalModelEvent::Changed { row } if row.id == "hf/org/Tiny-GGUF/Tiny-Q8_0")));
    }

    #[test]
    fn a_file_listed_by_the_catalogue_isnt_listed_again_as_a_hub_download() {
        let (d, _rec, lm) = two_ready_models();
        // The catalogue's "a" is org/repo a.gguf and ready; the same file as a Hub download.
        fake_hub_download(&d.0, "org/repo", "a.gguf", b"12345");
        lm.refresh_externals();
        assert!(lm.rows().iter().all(|r| r.origin == ModelOrigin::Catalog));
    }

    #[cfg(unix)]
    #[test]
    fn a_hub_download_and_the_same_file_in_the_cache_are_listed_once() {
        let (d, _rec, mut lm) = two_ready_models();
        fake_hub_download(&d.0, "org/Tiny-GGUF", "Tiny-Q4_K_M.gguf", b"1234567");
        // The same repo and file in a Hugging Face cache layout (snapshot links into blobs),
        // plus another file only the cache has.
        let cache =
            Dir(std::env::temp_dir().join(format!("dn-hfcache-dedupe-{}", dn_core::Id::new())));
        let repo = cache.0.join("models--org--Tiny-GGUF");
        let snap = repo.join("snapshots").join("e".repeat(40));
        std::fs::create_dir_all(repo.join("blobs")).unwrap();
        std::fs::create_dir_all(&snap).unwrap();
        for (file, blob) in [
            ("Tiny-Q4_K_M.gguf", "1".repeat(64)),
            ("Tiny-Q8_0.gguf", "2".repeat(64)),
        ] {
            std::fs::write(repo.join("blobs").join(&blob), b"weights").unwrap();
            std::os::unix::fs::symlink(repo.join("blobs").join(&blob), snap.join(file)).unwrap();
        }
        lm.hf_cache = Some(cache.0.clone());
        lm.refresh_externals();
        let ids: Vec<String> = lm
            .rows()
            .into_iter()
            .filter(|r| r.origin != ModelOrigin::Catalog)
            .map(|r| r.id)
            .collect();
        assert_eq!(
            ids,
            [
                "hf/org/Tiny-GGUF/Tiny-Q4_K_M",
                "hfcache/org/Tiny-GGUF/Tiny-Q8_0"
            ],
            "the Hub copy wins; the cache adds only what's new"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn opening_settings_picks_up_models_the_cli_downloaded() {
        let (d, _rec, lm) = two_ready_models();
        let lm = Arc::new(lm);
        // Downloaded by the CLI while the app ran: not listed yet.
        fake_hub_download(&d.0, "org/Tiny-GGUF", "Tiny-Q4_K_M.gguf", b"1234567");
        let events = Arc::new(Mutex::new(Vec::new()));
        let e2 = Arc::clone(&events);
        let rows = lm.watch_and_refresh(Arc::new(move |ev| lock(&e2).push(ev)));
        assert!(rows.iter().all(|r| r.origin == ModelOrigin::Catalog));
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let seen = lock(&events).iter().any(|e| {
                matches!(e, LocalModelEvent::Changed { row } if row.id == "hf/org/Tiny-GGUF/Tiny-Q4_K_M")
            });
            if seen {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the refresh never reported the new model"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn a_hub_model_that_fails_to_load_is_re_read_and_dropped_when_gone() {
        let dir = Dir(std::env::temp_dir().join(format!("dn-local-gone-{}", dn_core::Id::new())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = fake_hub_download(&dir.0, "org/Tiny-GGUF", "Tiny-Q4_K_M.gguf", b"not a gguf");
        let lm = LocalModels::new(
            dir.0.clone(),
            Catalog {
                version: 1,
                models: vec![],
            },
            Machine::cpu_only(16 << 30),
        );
        lm.refresh_externals();
        let id = "hf/org/Tiny-GGUF/Tiny-Q4_K_M";
        assert!(lm.is_external(id));
        // The file goes away behind the app's back (deleted with the CLI, a cleaner…).
        std::fs::remove_file(&path).unwrap();
        let mut reg = crate::models::ModelRegistry::default();
        reg.local = Some(Arc::new(lm));
        let keys = crate::secrets::Keys::memory();
        assert!(
            reg.get(&format!("local:{id}").parse().unwrap(), &keys)
                .is_err()
        );
        assert!(
            !reg.local.as_ref().unwrap().is_external(id),
            "re-read after the failed load: the missing model is gone from the list"
        );
    }
}
