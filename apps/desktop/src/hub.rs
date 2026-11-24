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
}

#[derive(Clone, Debug, Serialize)]
pub struct RepoView {
    pub id: String,
    pub sha: String,
    pub gated: bool,
    pub license: LicenseInfo,
    pub card_url: String,
    pub variants: Vec<VariantView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PullEvent {
    Progress(PullProgress),
}

/// Running downloads, by `repo@sha/variant`, so the screen can cancel one.
#[derive(Default)]
pub struct HubPulls {
    running: Mutex<HashMap<String, tokio::task::AbortHandle>>,
}

pub fn pull_key(repo: &str, sha: &str, variant: &str) -> String {
    format!("{repo}@{sha}/{variant}")
}

impl HubPulls {
    pub fn cancel(&self, key: &str) -> bool {
        match self.running.lock().ok().and_then(|mut m| m.remove(key)) {
            Some(h) => {
                h.abort();
                true
            }
            None => false,
        }
    }
    fn start(&self, key: String, h: tokio::task::AbortHandle) -> IpcResult<()> {
        let mut m = self
            .running
            .lock()
            .map_err(|_| IpcError::new("internal", "Something went wrong. Try again."))?;
        if m.contains_key(&key) {
            return Err(IpcError::new("busy", "That model is already downloading."));
        }
        m.insert(key, h);
        Ok(())
    }
    fn finish(&self, key: &str) {
        if let Ok(mut m) = self.running.lock() {
            m.remove(key);
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
pub async fn repo(hub: &HubClient, local: &LocalModels, repo: &str) -> IpcResult<RepoView> {
    hub::validate_repo_id(repo).map_err(hub_error)?;
    let info = hub.repo(repo, None).await.map_err(hub_error)?;
    let variants: Vec<GgufVariant> = hub
        .variants(&info.id, &info.sha)
        .await
        .map_err(hub_error)?
        .into_iter()
        .filter(|v| v.kind == VariantKind::Model)
        .collect();
    let base_fp = match base_variant(&variants) {
        Some(b) => hub.footprint(&info, b).await.ok().map(|fp| (b.clone(), fp)),
        None => None,
    };
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
        .map(|v| {
            let fit = base_fp.as_ref().map(|(b, fp)| {
                let est = hub::estimate_variant(fp, b, v).fit(local.machine());
                local.fit_view_for(&est.value, est.estimated)
            });
            VariantView {
                name: v.name.clone(),
                quant: v.quant.clone(),
                total_size: v.total_size,
                parts: v.files.len(),
                verifiable: v.files.iter().all(|f| f.sha256.is_some()),
                fit,
                downloaded: downloaded.get(&v.name).cloned(),
            }
        })
        .collect();
    Ok(RepoView {
        id: info.id.clone(),
        sha: info.sha.clone(),
        gated: info.gated,
        license: info.license.clone(),
        card_url: info.card_url.clone(),
        variants: views,
    })
}

/// Downloads one quantisation of `repo` pinned at `sha` into the models folder, reporting
/// progress, then lists it as a local model. Returns its `local:` model ref.
pub async fn pull(
    hub: HubClient,
    local: Arc<LocalModels>,
    pulls: &HubPulls,
    repo: String,
    sha: String,
    variant: String,
    mut on_progress: impl FnMut(PullProgress) + Send + 'static,
) -> IpcResult<String> {
    hub::validate_repo_id(&repo).map_err(hub_error)?;
    let key = pull_key(&repo, &sha, &variant);
    let task = tokio::spawn(async move {
        let info: RepoInfo = hub.repo(&repo, Some(&sha)).await?;
        let v = hub
            .variants(&info.id, &info.sha)
            .await?
            .into_iter()
            .find(|x| x.name == variant && x.kind == VariantKind::Model)
            .ok_or_else(|| HubError::NotFound(format!("{repo}: {variant}")))?;
        let path = hub
            .pull(&info, &v, local.dir(), &mut |p| on_progress(p))
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
    });
    pulls.start(key.clone(), task.abort_handle())?;
    let result = task.await;
    pulls.finish(&key);
    match result {
        Ok(r) => r.map_err(hub_error),
        Err(e) if e.is_cancelled() => Err(IpcError::new(
            "cancelled",
            "Download paused. Downloading it again resumes where it stopped.",
        )),
        Err(e) => {
            eprintln!("hub pull task: {e}");
            Err(IpcError::new(
                "internal",
                "The download stopped. Try again.",
            ))
        }
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

    #[tokio::test]
    async fn a_second_pull_of_the_same_file_is_refused_and_cancel_stops_it() {
        let pulls = HubPulls::default();
        let task = tokio::spawn(std::future::pending::<()>());
        pulls.start("k".into(), task.abort_handle()).unwrap();
        let other = tokio::spawn(std::future::pending::<()>());
        let err = pulls.start("k".into(), other.abort_handle()).unwrap_err();
        assert!(err.message.contains("already downloading"));
        other.abort();
        assert!(pulls.cancel("k"));
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!pulls.cancel("k"));
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
}
