//! Add a model from Hugging Face (T-INF-10, issue #48): search GGUF repos, see each
//! quantisation with its fit, the licence and the model card, then download (verified,
//! resumable) into the models folder. Built on `dn_models::hub` (W-inference).
//!
//! Nothing here sends chat content anywhere: search and file requests go to Hugging Face
//! (or `HF_ENDPOINT`), with the user's token only for gated repos and only to huggingface.co
//! unless the user opted in for a mirror (dn-models' rule).

use crate::error::{IpcError, IpcResult};
use crate::local::{FitView, LocalModels};
use crate::secrets::Keys;
use dn_models::hub::{
    self, GgufVariant, HubClient, HubConfig, HubError, LicenseInfo, PullProgress, RepoInfo,
    SearchPage, SearchQuery, SearchSort, VariantKind,
};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One quantisation as the screen shows it.
#[derive(Clone, Debug, Serialize)]
pub struct VariantView {
    pub name: String,
    pub quant: Option<String>,
    pub total_size: u64,
    pub parts: usize,
    /// Every part has a published sha256: only then can it be downloaded (verified).
    pub verifiable: bool,
    /// `None` when the fit couldn't be worked out (header unreadable, offline).
    pub fit: Option<FitView>,
    /// Already downloaded: its `local:` model ref.
    pub downloaded: Option<String>,
    /// Downloading now (started from this screen earlier): the latest progress.
    pub progress: Option<PullProgress>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RepoView {
    pub id: String,
    pub sha: String,
    pub gated: bool,
    pub license: LicenseInfo,
    pub card_url: String,
    pub variants: Vec<VariantView>,
    /// The variant a `demios://` link chose, if it chose exactly one.
    pub selected: Option<String>,
    /// The fit couldn't be worked out (the GGUF header wasn't readable).
    pub fit_unknown: bool,
    /// What the model is for, from the same header (T-INF-26); `None` when it wasn't readable.
    pub traits: Option<dn_models::kind::TraitsView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PullEvent {
    Progress(PullProgress),
}

/// Running downloads, by `repo@sha/variant`: one at a time per file, cancellable, with the
/// last progress so a screen that comes back can show it.
///
/// Like the catalogue downloads: a run is registered with a generation token *before* its
/// task starts (a second start is refused), a finishing old run can't remove a newer run's
/// entry, and a resumed run waits on a per-file lock until the paused one has let go of its
/// files (and dn-models' `.lock`).
#[derive(Default)]
pub struct HubPulls {
    running: Mutex<HashMap<String, (u64, Option<tokio::task::AbortHandle>)>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    progress: Mutex<HashMap<String, PullProgress>>,
    next: std::sync::atomic::AtomicU64,
}

/// Why a run didn't finish.
#[derive(Debug, PartialEq, Eq)]
pub enum RunEnd {
    Busy,
    Cancelled,
    Failed,
}

pub fn pull_key(repo: &str, sha: &str, variant: &str) -> String {
    format!("{repo}@{sha}/{variant}")
}

fn guard<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl HubPulls {
    /// Runs `work` as the download for `key`.
    pub async fn run<T: Send + 'static>(
        &self,
        key: &str,
        work: impl std::future::Future<Output = T> + Send + 'static,
    ) -> Result<T, RunEnd> {
        let generation = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        {
            let mut running = guard(&self.running);
            if running.contains_key(key) {
                return Err(RunEnd::Busy);
            }
            running.insert(key.to_owned(), (generation, None));
        }
        let lock = Arc::clone(guard(&self.locks).entry(key.to_owned()).or_default());
        let task = tokio::spawn(async move {
            // Wait until a paused run of the same file has stopped.
            let _held = lock.lock_owned().await;
            work.await
        });
        {
            let mut running = guard(&self.running);
            match running.get_mut(key) {
                Some((g, h)) if *g == generation => *h = Some(task.abort_handle()),
                // Cancelled before the task was registered: stop it now.
                _ => task.abort(),
            }
        }
        let result = task.await;
        {
            let mut running = guard(&self.running);
            if running.get(key).is_some_and(|(g, _)| *g == generation) {
                running.remove(key);
                guard(&self.progress).remove(key);
            }
        }
        match result {
            Ok(v) => Ok(v),
            Err(e) if e.is_cancelled() => Err(RunEnd::Cancelled),
            Err(e) => {
                eprintln!("hub pull task: {e}");
                Err(RunEnd::Failed)
            }
        }
    }

    pub fn cancel(&self, key: &str) -> bool {
        match guard(&self.running).remove(key) {
            Some((_, h)) => {
                if let Some(h) = h {
                    h.abort();
                }
                true
            }
            None => false,
        }
    }

    pub fn record(&self, key: &str, p: PullProgress) {
        if guard(&self.running).contains_key(key) {
            guard(&self.progress).insert(key.to_owned(), p);
        }
    }

    /// Progress of a running download (zero until the first update), or `None`.
    pub fn progress_of(&self, key: &str) -> Option<PullProgress> {
        if !guard(&self.running).contains_key(key) {
            return None;
        }
        Some(
            guard(&self.progress)
                .get(key)
                .cloned()
                .unwrap_or(PullProgress {
                    part: 0,
                    parts: 1,
                    done_bytes: 0,
                    total_bytes: 0,
                }),
        )
    }
}

/// Lets progress through at most every `every`, and always the last update of a part.
pub struct Throttle {
    every: std::time::Duration,
    last: Option<std::time::Instant>,
}

impl Throttle {
    pub fn new(every: std::time::Duration) -> Self {
        Self { every, last: None }
    }
    pub fn pass(&mut self, p: &PullProgress, now: std::time::Instant) -> bool {
        let done = p.done_bytes >= p.total_bytes;
        if done
            || self
                .last
                .is_none_or(|t| now.duration_since(t) >= self.every)
        {
            self.last = Some(now);
            true
        } else {
            false
        }
    }
}

/// User words for a Hub error, with the recovery dn-models attaches.
pub fn hub_error(e: HubError) -> IpcError {
    eprintln!("hub: {e}");
    crate::agent::model_error(e.into())
}

/// A Hub client for this user: `HF_ENDPOINT` if set, and the stored token. Blocking
/// (keychain).
pub fn client(keys: &Keys) -> IpcResult<HubClient> {
    let mut cfg = HubConfig::from_env();
    if let Ok(Some(t)) = keys.get(hub::HF_TOKEN_SECRET) {
        cfg = cfg.with_token(t);
    }
    HubClient::new(cfg).map_err(hub_error)
}

pub async fn search(
    hub: &HubClient,
    text: &str,
    sort: SearchSort,
    cursor: Option<&str>,
) -> IpcResult<SearchPage> {
    if text.chars().count() > 200 {
        return Err(IpcError::invalid("Search text is too long."));
    }
    match cursor {
        Some(c) => hub.search_next(c).await,
        None => {
            hub.search(&SearchQuery {
                text: text.trim().to_string(),
                sort,
                limit: Some(20),
            })
            .await
        }
    }
    .map_err(hub_error)
}

/// The base variant whose header is read for the fit: the smallest model variant (cheapest
/// range reads; the others are scaled from it).
fn base_variant(variants: &[GgufVariant]) -> Option<&GgufVariant> {
    variants
        .iter()
        .filter(|v| v.kind == VariantKind::Model)
        .min_by_key(|v| v.total_size)
}

/// Repo details: licence, card, and each model quantisation with its fit (read from one
/// header, scaled for the others and marked estimated) and whether it's downloaded.
pub async fn repo(
    hub: &HubClient,
    local: &LocalModels,
    pulls: &HubPulls,
    repo: &str,
) -> IpcResult<RepoView> {
    hub::validate_repo_id(repo).map_err(hub_error)?;
    let info = hub.repo(repo, None).await.map_err(hub_error)?;
    let variants: Vec<GgufVariant> = hub
        .variants(&info.id, &info.sha)
        .await
        .map_err(hub_error)?
        .into_iter()
        .filter(|v| v.kind == VariantKind::Model)
        .collect();
    let inspected = match base_variant(&variants) {
        Some(b) => hub
            .inspect(&info, b)
            .await
            .ok()
            .map(|(fp, t)| (b.clone(), fp, t)),
        None => None,
    };
    let traits = inspected.as_ref().map(|(_, _, t)| (*t).into());
    let base_fp = inspected.map(|(b, fp, _)| (b, fp));
    let fits = variants
        .iter()
        .map(|v| {
            base_fp.as_ref().map(|(b, fp)| {
                let est = hub::estimate_variant(fp, b, v).fit(local.machine());
                local.fit_view_for(&est.value, est.estimated)
            })
        })
        .collect();
    let fit_unknown = base_fp.is_none();
    let mut view = build_view(&info, &variants, fits, None, fit_unknown, local, pulls);
    view.traits = traits;
    Ok(view)
}

/// The screen's view of a repo pinned to a commit: model variants with fit, downloaded and
/// running state.
fn build_view(
    info: &RepoInfo,
    variants: &[GgufVariant],
    fits: Vec<Option<FitView>>,
    selected: Option<String>,
    fit_unknown: bool,
    local: &LocalModels,
    pulls: &HubPulls,
) -> RepoView {
    let downloaded: HashMap<String, String> = local
        .rows()
        .into_iter()
        .filter(|r| r.origin == crate::local::ModelOrigin::Hub && r.repo == info.id)
        .map(|r| {
            (
                r.id.rsplit('/').next().unwrap_or("").to_string(),
                r.model_ref,
            )
        })
        .collect();
    let views = variants
        .iter()
        .zip(fits)
        .filter(|(v, _)| v.kind == VariantKind::Model)
        .map(|(v, fit)| VariantView {
            name: v.name.clone(),
            quant: v.quant.clone(),
            total_size: v.total_size,
            parts: v.files.len(),
            verifiable: v.files.iter().all(|f| f.sha256.is_some()),
            fit,
            downloaded: downloaded.get(&v.name).cloned(),
            progress: pulls.progress_of(&pull_key(&info.id, &info.sha, &v.name)),
        })
        .collect();
    RepoView {
        id: info.id.clone(),
        sha: info.sha.clone(),
        gated: info.gated,
        license: info.license.clone(),
        card_url: info.card_url.clone(),
        variants: views,
        selected,
        fit_unknown,
        traits: None,
    }
}

/// A `demios://` link's model, as the confirmation screen shows it: pinned to the commit the
/// link's revision resolves to, with the variant it chose (if one) and fits. Downloads
/// nothing; the user starts the download from the screen.
pub async fn resolve_link(
    hub: &HubClient,
    local: &LocalModels,
    pulls: &HubPulls,
    link: &dn_models::link::AddModelLink,
) -> IpcResult<RepoView> {
    let link = crate::deeplink::revalidate(link)
        .map_err(|_| IpcError::invalid("That link isn't a valid Demios model link."))?;
    let plan = link
        .resolve(hub, local.machine())
        .await
        .map_err(hub_error)?;
    let fits = plan
        .fits
        .iter()
        .map(|f| {
            f.as_ref()
                .map(|e| local.fit_view_for(&e.value, e.estimated))
        })
        .collect();
    let selected = plan.selected_variant().map(|v| v.name.clone());
    let fit_unknown = plan
        .warnings
        .contains(&dn_models::link::PlanWarning::FitUnknown);
    Ok(build_view(
        &plan.info,
        &plan.variants,
        fits,
        selected,
        fit_unknown,
        local,
        pulls,
    ))
}

/// Downloads one quantisation of `repo` pinned at `sha` (the 40-hex commit from
/// [`repo`]) into the models folder, reporting progress (at most every 250 ms), then lists
/// it as a local model. Returns its `local:` model ref.
pub async fn pull(
    hub: HubClient,
    local: Arc<LocalModels>,
    pulls: Arc<HubPulls>,
    repo: String,
    sha: String,
    variant: String,
    mut on_progress: impl FnMut(PullProgress) + Send + 'static,
) -> IpcResult<String> {
    hub::validate_repo_id(&repo).map_err(hub_error)?;
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(IpcError::invalid(
            "That model version isn't valid. Open the model again.",
        ));
    }
    let key = pull_key(&repo, &sha, &variant);
    let (k2, p2) = (key.clone(), Arc::clone(&pulls));
    let work = async move {
        let info: RepoInfo = hub.repo(&repo, Some(&sha)).await?;
        let v = hub
            .variants(&info.id, &info.sha)
            .await?
            .into_iter()
            .find(|x| x.name == variant && x.kind == VariantKind::Model)
            .ok_or_else(|| HubError::NotFound(format!("{repo}: {variant}")))?;
        let mut throttle = Throttle::new(std::time::Duration::from_millis(250));
        let path = hub
            .pull(&info, &v, local.dir(), &mut |p| {
                p2.record(&k2, p.clone());
                if throttle.pass(&p, std::time::Instant::now()) {
                    on_progress(p);
                }
            })
            .await?;
        let l2 = Arc::clone(&local);
        tokio::task::spawn_blocking(move || l2.refresh_externals())
            .await
            .map_err(|e| HubError::Parse(e.to_string()))?;
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok::<String, HubError>(
            local
                .rows()
                .into_iter()
                .find(|r| r.location.as_deref() == Some(&path.display().to_string()))
                .map(|r| r.model_ref)
                .unwrap_or_else(|| format!("local:hf/{}/{stem}", info.id)),
        )
    };
    match pulls.run(&key, work).await {
        Ok(r) => r.map_err(hub_error),
        Err(RunEnd::Busy) => Err(IpcError::new("busy", "That model is already downloading.")),
        Err(RunEnd::Cancelled) => Err(IpcError::new(
            "cancelled",
            "Paused. Resume to continue where it stopped.",
        )),
        Err(RunEnd::Failed) => Err(IpcError::new(
            "internal",
            "The download stopped. Try again.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(name: &str, size: u64, kind: VariantKind) -> GgufVariant {
        GgufVariant {
            name: name.into(),
            quant: hub::quant_label(name),
            kind,
            files: vec![],
            total_size: size,
        }
    }

    #[test]
    fn the_smallest_model_variant_is_the_fit_base() {
        let vs = [
            v("m-Q8_0", 8, VariantKind::Model),
            v("mmproj", 1, VariantKind::Projector),
            v("m-Q4_K_M", 4, VariantKind::Model),
        ];
        assert_eq!(base_variant(&vs).unwrap().name, "m-Q4_K_M");
        assert!(base_variant(&[]).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pause_resume_pause_waits_for_the_paused_run_and_keeps_the_new_one() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let pulls = Arc::new(HubPulls::default());
        // A run that holds "the file" until it is dropped (aborted).
        struct Holds(Arc<AtomicBool>);
        impl Drop for Holds {
            fn drop(&mut self) {
                // Letting go takes a moment, like closing files.
                std::thread::sleep(std::time::Duration::from_millis(30));
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let holding = Arc::new(AtomicBool::new(false));
        let first = {
            let (p, h) = (Arc::clone(&pulls), Arc::clone(&holding));
            tokio::spawn(async move {
                p.run("k", async move {
                    h.store(true, Ordering::SeqCst);
                    let _held = Holds(h);
                    std::future::pending::<()>().await
                })
                .await
            })
        };
        while !holding.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        // A second start while it runs is refused (a timeout, so a missing busy check fails
        // instead of waiting on the file lock forever).
        let busy =
            tokio::time::timeout(std::time::Duration::from_secs(2), pulls.run("k", async {}))
                .await
                .expect("a second start must be refused at once, not wait");
        assert_eq!(busy, Err(RunEnd::Busy));

        // Pause, then resume at once: the new run starts only after the old one let go.
        assert!(pulls.cancel("k"));
        let overlapped = Arc::new(AtomicBool::new(false));
        let second = {
            let (p, h, o) = (
                Arc::clone(&pulls),
                Arc::clone(&holding),
                Arc::clone(&overlapped),
            );
            tokio::spawn(async move {
                p.run("k", async move {
                    o.store(h.load(Ordering::SeqCst), Ordering::SeqCst);
                    std::future::pending::<()>().await
                })
                .await
            })
        };
        assert_eq!(first.await.unwrap(), Err(RunEnd::Cancelled));
        // The old run finishing didn't remove the new registration: pause works on it.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert!(
            !overlapped.load(Ordering::SeqCst),
            "never ran while the old one held the file"
        );
        assert!(pulls.cancel("k"), "the resumed run is still registered");
        assert_eq!(second.await.unwrap(), Err(RunEnd::Cancelled));
        assert!(!pulls.cancel("k"));
    }

    #[test]
    fn progress_is_throttled_but_the_last_update_always_passes() {
        let mut t = Throttle::new(std::time::Duration::from_millis(250));
        let p = |done| PullProgress {
            part: 0,
            parts: 1,
            done_bytes: done,
            total_bytes: 100,
        };
        let t0 = std::time::Instant::now();
        let ms = |n| t0 + std::time::Duration::from_millis(n);
        assert!(t.pass(&p(1), ms(0)));
        assert!(!t.pass(&p(2), ms(100)));
        assert!(t.pass(&p(3), ms(260)));
        assert!(t.pass(&p(100), ms(270)), "the final update always passes");
    }

    #[tokio::test]
    async fn long_search_text_is_refused_before_any_request() {
        // An endpoint that would fail if contacted.
        let hub = HubClient::new(HubConfig {
            endpoint: Some("http://127.0.0.1:9".into()),
            ..Default::default()
        })
        .unwrap();
        let long = "x".repeat(201);
        let err = search(&hub, &long, SearchSort::Downloads, None)
            .await
            .unwrap_err();
        assert!(err.message.contains("too long"));
    }

    #[tokio::test]
    async fn a_pull_needs_the_pinned_commit_not_a_branch_name() {
        // An endpoint that would fail if contacted: the check comes first.
        let hub = || {
            HubClient::new(HubConfig {
                endpoint: Some("http://127.0.0.1:9".into()),
                ..Default::default()
            })
            .unwrap()
        };
        let dir = std::env::temp_dir().join(format!("dn-hub-sha-{}", dn_core::Id::new()));
        let local = Arc::new(LocalModels::new(
            dir,
            dn_models::Catalog {
                version: 1,
                models: vec![],
            },
            dn_models::Machine::cpu_only(1 << 30),
        ));
        for bad in ["main", "", "abc", &"g".repeat(40), &"a".repeat(41)] {
            let err = pull(
                hub(),
                Arc::clone(&local),
                Arc::new(HubPulls::default()),
                "org/Tiny-GGUF".into(),
                bad.to_string(),
                "Tiny-Q4_K_M".into(),
                |_| {},
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "invalid_input", "{bad:?}");
        }
    }
}
