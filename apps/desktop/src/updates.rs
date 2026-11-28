//! Signed updates for direct downloads (T-STORE-08, ADR-0009 "Tauri updater with signature
//! verification").
//!
//! - Only builds with the `updater` feature contain the updater, and `build.rs` refuses that
//!   feature for Microsoft Store (MSIX) and Flathub builds ([`crate::distribution`]): their
//!   store updates them.
//! - Nothing happens on its own. [`check_for_update`] runs when the user selects "Check for
//!   updates"; [`install_update`] runs only after they confirm the version they were shown.
//! - The download is checked against the build's ed25519 public key (minisign) before it is
//!   installed, and `requireSignedVersion` binds the signature to the announced version, so a
//!   tampered `latest.json` can't pair a new version number with an older release.
//! - The public key is not secret. It comes from `plugins.updater.pubkey` in
//!   `tauri.conf.json`, or from `DEMIOS_UPDATER_PUBKEY` at build time. A build with neither
//!   doesn't check ([`UpdateMode::NoKey`]). The private key never comes near the app.

use crate::distribution::Distribution;
use crate::error::{IpcError, IpcResult};
use serde::Serialize;
use tauri::ipc::Channel;

/// Where direct-download builds look for updates: the `latest.json` that the release job
/// uploads to the newest GitHub release. Must match `plugins.updater.endpoints` in
/// `tauri.conf.json` (a test checks).
pub const ENDPOINT: &str =
    "https://github.com/mimshavandi/demios-desktop/releases/latest/download/latest.json";

/// The update public key given at build time, if any (it wins over `tauri.conf.json`).
pub const BUILD_PUBKEY: Option<&str> = option_env!("DEMIOS_UPDATER_PUBKEY");

/// Release notes are shown as plain text, cut to this many characters.
pub const NOTES_MAX: usize = 20_000;

/// How this build gets updates, for Settings › About.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    /// Checks GitHub Releases when asked and installs a signed update after the user confirms.
    SelfUpdate,
    /// Has the updater but no public key, so it couldn't verify an update and doesn't check.
    NoKey,
    /// The Microsoft Store updates it.
    MicrosoftStore,
    /// Flathub (Flatpak) updates it.
    Flathub,
    /// Built without the updater: a development or source build.
    Off,
}

/// The build's distribution (`build.rs` has already rejected unknown values).
pub fn distribution() -> Distribution {
    Distribution::parse(option_env!("DEMIOS_DISTRIBUTION")).unwrap_or(Distribution::Direct)
}

/// How a build with these settings gets updates. Store and Flathub builds never update
/// themselves, whatever else is set.
pub fn mode(updater_feature: bool, distribution: Distribution, pubkey: &str) -> UpdateMode {
    match distribution {
        Distribution::MicrosoftStore => UpdateMode::MicrosoftStore,
        Distribution::Flathub => UpdateMode::Flathub,
        Distribution::Direct if !updater_feature => UpdateMode::Off,
        Distribution::Direct if pubkey.trim().is_empty() => UpdateMode::NoKey,
        Distribution::Direct => UpdateMode::SelfUpdate,
    }
}

/// The public key the updater uses: the build-time one, else `plugins.updater.pubkey`.
pub fn pubkey(config: &tauri::Config) -> String {
    match BUILD_PUBKEY.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => k.to_owned(),
        None => config
            .plugins
            .0
            .get("updater")
            .and_then(|u| u.get("pubkey"))
            .and_then(|k| k.as_str())
            .unwrap_or_default()
            .trim()
            .to_owned(),
    }
}

/// This build's update mode.
pub fn current_mode(config: &tauri::Config) -> UpdateMode {
    mode(cfg!(feature = "updater"), distribution(), &pubkey(config))
}

/// What "Check for updates" found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UpdateCheck {
    UpToDate { current: String },
    Available(UpdateInfo),
}

/// A newer version, as announced by `latest.json`. Nothing is downloaded yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current: String,
    /// Release notes as plain text (the UI never renders them as Markdown or HTML).
    pub notes: String,
    /// `pub_date` from `latest.json` (RFC 3339), if given.
    pub date: Option<String>,
}

impl UpdateInfo {
    /// Builds the info shown to the user from what `latest.json` announced.
    pub fn new(version: &str, current: &str, notes: Option<&str>, date: Option<&str>) -> Self {
        Self {
            version: crate::tool_review::plain_text(version, 64),
            current: current.to_owned(),
            notes: crate::tool_review::plain_text(notes.unwrap_or_default(), NOTES_MAX),
            date: date
                .map(|d| crate::tool_review::plain_text(d, 64))
                .filter(|d| !d.is_empty()),
        }
    }
}

/// Progress of "Install and restart".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateProgress {
    Downloading {
        downloaded: u64,
        total: Option<u64>,
    },
    /// Downloaded and the signature checked; installing, then the app restarts.
    Installing,
}

fn not_available(mode: UpdateMode) -> IpcError {
    IpcError::new(
        "update_not_available",
        match mode {
            UpdateMode::MicrosoftStore => "The Microsoft Store updates this copy of Almend.",
            UpdateMode::Flathub => "Flathub updates this copy of Almend.",
            UpdateMode::NoKey => {
                "This build has no update key, so it can't check for updates. Download the latest version from GitHub."
            }
            UpdateMode::Off | UpdateMode::SelfUpdate => {
                "This build doesn't update itself. Download the latest version from GitHub."
            }
        },
    )
}

/// Whether `pending` (what the last check announced) is the version the user confirmed.
pub fn confirmed(pending: Option<&str>, version: &str) -> IpcResult<()> {
    match pending {
        Some(p) if p == version => Ok(()),
        _ => Err(IpcError::new(
            "update_stale",
            "That update is no longer the one on offer. Check for updates again.",
        )),
    }
}

/// Sends download progress at most every 256 KiB, plus the first and the last chunk.
pub struct Throttle {
    downloaded: u64,
    sent: Option<u64>,
}

impl Throttle {
    const STEP: u64 = 256 * 1024;

    pub fn new() -> Self {
        Self {
            downloaded: 0,
            sent: None,
        }
    }

    /// Adds a chunk; returns the progress to send, if it is time to send one.
    pub fn chunk(&mut self, len: usize, total: Option<u64>) -> Option<UpdateProgress> {
        self.downloaded += len as u64;
        let done = total.is_some_and(|t| self.downloaded >= t);
        let due = match self.sent {
            None => true,
            Some(s) => self.downloaded - s >= Self::STEP,
        };
        (due || done).then(|| {
            self.sent = Some(self.downloaded);
            UpdateProgress::Downloading {
                downloaded: self.downloaded,
                total,
            }
        })
    }
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

/// The update the last check found, kept until the user confirms or checks again.
#[derive(Default)]
pub struct Updates {
    #[cfg(feature = "updater")]
    pending: std::sync::Mutex<Option<tauri_plugin_updater::Update>>,
    /// The launch check already ran (or was skipped) in this process.
    launch_done: std::sync::atomic::AtomicBool,
}

impl Updates {
    /// Claims this process's one launch check: true the first time only.
    fn claim_launch(&self) -> bool {
        !self
            .launch_done
            .swap(true, std::sync::atomic::Ordering::SeqCst)
    }
}

/// Whether the launch check runs (#95): only in a self-updating build, only when the user
/// turned it on, and only once per launch (a reloaded window doesn't ask GitHub again).
pub fn launch_check_due(mode: UpdateMode, turned_on: bool, first_in_process: bool) -> bool {
    mode == UpdateMode::SelfUpdate && turned_on && first_in_process
}

/// The opt-in check at launch (#95). Returns what it found, or nothing when it didn't run
/// or failed: a failed launch check is silent, and the next launch tries again. It only
/// checks; installing still needs "Install and restart" and its confirm.
#[tauri::command]
pub async fn launch_update_check(
    app: tauri::AppHandle,
    updates: tauri::State<'_, Updates>,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
) -> IpcResult<Option<UpdateCheck>> {
    let mode = current_mode(app.config());
    if mode != UpdateMode::SelfUpdate {
        return Ok(None);
    }
    let st = std::sync::Arc::clone(state.inner());
    let turned_on = tauri::async_runtime::spawn_blocking(move || st.settings())
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|s| s.check_updates_at_launch);
    if !launch_check_due(mode, turned_on, updates.claim_launch()) {
        return Ok(None);
    }
    #[cfg(feature = "updater")]
    {
        Ok(imp::check(&app, &updates).await.ok())
    }
    #[cfg(not(feature = "updater"))]
    {
        Ok(None)
    }
}

/// "Check for updates": asks GitHub Releases whether a newer version exists. Downloads
/// nothing.
#[tauri::command]
pub async fn check_for_update(
    app: tauri::AppHandle,
    updates: tauri::State<'_, Updates>,
) -> IpcResult<UpdateCheck> {
    let mode = current_mode(app.config());
    if mode != UpdateMode::SelfUpdate {
        return Err(not_available(mode));
    }
    #[cfg(feature = "updater")]
    {
        imp::check(&app, &updates).await
    }
    #[cfg(not(feature = "updater"))]
    {
        let _ = updates;
        Err(not_available(UpdateMode::Off))
    }
}

/// "Install and restart", after the user confirmed `version`: downloads the update, checks its
/// signature, installs it and restarts the app. Only the version the last check announced is
/// installed.
#[tauri::command]
pub async fn install_update(
    app: tauri::AppHandle,
    updates: tauri::State<'_, Updates>,
    version: String,
    on_progress: Channel<UpdateProgress>,
) -> IpcResult<()> {
    let mode = current_mode(app.config());
    if mode != UpdateMode::SelfUpdate {
        return Err(not_available(mode));
    }
    #[cfg(feature = "updater")]
    {
        imp::install(&app, &updates, &version, on_progress).await
    }
    #[cfg(not(feature = "updater"))]
    {
        let _ = (updates, version, on_progress);
        Err(not_available(UpdateMode::Off))
    }
}

/// The updater plugin for `lib.rs`, with the build-time public key if one was given.
#[cfg(feature = "updater")]
pub fn plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R, tauri_plugin_updater::Config> {
    let builder = tauri_plugin_updater::Builder::new();
    match BUILD_PUBKEY.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => builder.pubkey(k),
        None => builder,
    }
    .build()
}

#[cfg(feature = "updater")]
mod imp {
    use super::*;
    use tauri_plugin_updater::{Error as E, UpdaterExt};

    /// A short, user-facing error for an updater failure; the detail goes to the log.
    pub fn failure(e: &E) -> IpcError {
        tracing::warn!("update: {e}");
        match e {
            E::Reqwest(r) if r.is_connect() || r.is_timeout() => IpcError::new(
                "update_offline",
                "Couldn't reach GitHub. Check your internet connection and try again.",
            ),
            E::Reqwest(_)
            | E::Network(_)
            | E::ReleaseNotFound
            | E::TargetNotFound(_)
            | E::TargetsNotFound(_)
            | E::Serialization(_)
            | E::Semver(_)
            | E::UrlParse(_) => IpcError::new(
                "update_unreachable",
                "Couldn't get the update information from GitHub. Try again later.",
            ),
            E::Minisign(_)
            | E::Base64(_)
            | E::SignatureUtf8(_)
            | E::SignedVersionMismatch { .. }
            | E::MissingSignedVersion => IpcError::new(
                "update_signature",
                "The download didn't match the Almend signature, so it wasn't installed. Your version is unchanged.",
            ),
            E::AuthenticationFailed => IpcError::new(
                "update_cancelled",
                "Installing needs your permission, and it was cancelled. Your version is unchanged.",
            ),
            _ => IpcError::new(
                "update_failed",
                "Couldn't install the update. Your version is unchanged. Try again, or download it from GitHub.",
            ),
        }
    }

    pub async fn check(app: &tauri::AppHandle, updates: &Updates) -> IpcResult<UpdateCheck> {
        let updater = app
            .updater_builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| failure(&e))?;
        let found = updater.check().await.map_err(|e| failure(&e))?;
        let current = env!("CARGO_PKG_VERSION").to_owned();
        let result = match &found {
            None => UpdateCheck::UpToDate { current },
            Some(u) => UpdateCheck::Available(UpdateInfo::new(
                &u.version,
                &current,
                u.body.as_deref(),
                u.raw_json.get("pub_date").and_then(|d| d.as_str()),
            )),
        };
        *updates.pending.lock().unwrap_or_else(|p| p.into_inner()) = found;
        Ok(result)
    }

    pub async fn install(
        app: &tauri::AppHandle,
        updates: &Updates,
        version: &str,
        on_progress: Channel<UpdateProgress>,
    ) -> IpcResult<()> {
        let update = {
            let mut pending = updates.pending.lock().unwrap_or_else(|p| p.into_inner());
            confirmed(pending.as_ref().map(|u| u.version.as_str()), version)?;
            pending.take()
        };
        let Some(update) = update else {
            return confirmed(None, version);
        };
        let mut throttle = Throttle::new();
        let _ = on_progress.send(UpdateProgress::Downloading {
            downloaded: 0,
            total: None,
        });
        let result = async {
            let bytes = update
                .download(
                    |len, total| {
                        if let Some(p) = throttle.chunk(len, total) {
                            let _ = on_progress.send(p);
                        }
                    },
                    || {},
                )
                .await?;
            let _ = on_progress.send(UpdateProgress::Installing);
            update.install(bytes)
        }
        .await;
        match result {
            // Windows: the installer has already quit the app. macOS and Linux: restart now.
            Ok(()) => app.restart(),
            Err(e) => {
                // Keep it on offer, so "Try again" doesn't need a new check.
                let err = failure(&e);
                *updates.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(update);
                Err(err)
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn failures_tell_signature_and_server_problems_apart() {
            assert_eq!(failure(&E::ReleaseNotFound).code, "update_unreachable");
            assert_eq!(
                failure(&E::Network("503".into())).code,
                "update_unreachable"
            );
            assert_eq!(
                failure(&E::SignatureUtf8("x".into())).code,
                "update_signature"
            );
            assert_eq!(failure(&E::MissingSignedVersion).code, "update_signature");
            assert_eq!(
                failure(&E::SignedVersionMismatch {
                    signed: "0.1.0".into(),
                    announced: "9.0.0".into()
                })
                .code,
                "update_signature"
            );
            assert_eq!(failure(&E::AuthenticationFailed).code, "update_cancelled");
            assert_eq!(failure(&E::DebInstallFailed).code, "update_failed");
        }

        #[test]
        fn the_plugin_reads_our_config() {
            let conf: serde_json::Value =
                serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
            let c: tauri_plugin_updater::Config =
                serde_json::from_value(conf["plugins"]["updater"].clone()).expect("updater config");
            assert_eq!(c.endpoints.len(), 1);
            assert_eq!(c.endpoints[0].as_str(), ENDPOINT);
            assert!(c.require_signed_version);
            assert!(!c.allow_downgrades);
        }

        #[test]
        fn a_build_with_the_feature_and_a_key_updates_itself() {
            assert_eq!(
                mode(true, Distribution::Direct, "dW50cnVzdGVkIGNvbW1lbnQ="),
                UpdateMode::SelfUpdate
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distribution::check_build;

    #[test]
    fn the_launch_check_is_opt_in_self_update_only_and_once() {
        assert!(launch_check_due(UpdateMode::SelfUpdate, true, true));
        assert!(
            !launch_check_due(UpdateMode::SelfUpdate, false, true),
            "off by default"
        );
        assert!(
            !launch_check_due(UpdateMode::SelfUpdate, true, false),
            "once per launch"
        );
        for m in [
            UpdateMode::MicrosoftStore,
            UpdateMode::Flathub,
            UpdateMode::Off,
            UpdateMode::NoKey,
        ] {
            assert!(!launch_check_due(m, true, true), "{m:?}");
        }
        let u = Updates::default();
        assert!(u.claim_launch());
        assert!(!u.claim_launch());
    }

    #[test]
    fn store_and_flathub_builds_never_update_themselves() {
        for (d, want) in [
            (Distribution::MicrosoftStore, UpdateMode::MicrosoftStore),
            (Distribution::Flathub, UpdateMode::Flathub),
        ] {
            for feature in [false, true] {
                assert_eq!(mode(feature, d, "a-key"), want);
            }
        }
        // The build itself fails if the updater is turned on for them.
        assert!(check_build(true, Some("msix")).is_err());
        assert!(check_build(true, Some("flatpak")).is_err());
        assert!(check_build(true, Some(" flatpak ")).is_err());
        assert_eq!(
            check_build(false, Some("msix")),
            Ok(Distribution::MicrosoftStore)
        );
        assert_eq!(
            check_build(false, Some("flatpak")),
            Ok(Distribution::Flathub)
        );
    }

    #[test]
    fn an_unknown_distribution_fails_the_build() {
        for v in ["store", "MSIX", "flathub", "appimage"] {
            assert!(check_build(false, Some(v)).is_err(), "{v}");
            assert!(check_build(true, Some(v)).is_err(), "{v}");
        }
    }

    #[test]
    fn direct_builds_update_only_with_the_feature_and_a_key() {
        assert_eq!(check_build(true, None), Ok(Distribution::Direct));
        assert_eq!(check_build(true, Some("direct")), Ok(Distribution::Direct));
        assert_eq!(check_build(true, Some("")), Ok(Distribution::Direct));
        assert_eq!(mode(false, Distribution::Direct, "a-key"), UpdateMode::Off);
        assert_eq!(mode(true, Distribution::Direct, ""), UpdateMode::NoKey);
        assert_eq!(mode(true, Distribution::Direct, "  "), UpdateMode::NoKey);
        assert_eq!(
            mode(true, Distribution::Direct, "a-key"),
            UpdateMode::SelfUpdate
        );
    }

    /// What `cargo test` builds: without the feature the commands refuse and say why. The
    /// distribution is fixed to a direct download, so `DEMIOS_DISTRIBUTION` at build time
    /// (e.g. `flatpak`) doesn't change the result.
    #[cfg(not(feature = "updater"))]
    #[test]
    fn a_build_without_the_feature_has_no_updater() {
        let config: tauri::Config =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        assert_eq!(
            mode(
                cfg!(feature = "updater"),
                Distribution::Direct,
                &pubkey(&config)
            ),
            UpdateMode::Off
        );
        assert_eq!(not_available(UpdateMode::Off).code, "update_not_available");
    }

    #[test]
    fn the_config_points_at_github_releases_and_binds_signatures_to_versions() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        let u = &conf["plugins"]["updater"];
        assert_eq!(u["endpoints"], serde_json::json!([ENDPOINT]));
        assert!(ENDPOINT.starts_with("https://github.com/"));
        assert_eq!(u["requireSignedVersion"], true);
        assert_ne!(u["allowDowngrades"], true);
        assert_ne!(u["dangerousInsecureTransportProtocol"], true);
        assert_ne!(u["dangerousAcceptInvalidCerts"], true);
        assert_ne!(u["dangerousAcceptInvalidHostnames"], true);
        // Base builds don't make update bundles; direct release builds add tauri.updater.conf.json.
        assert_ne!(conf["bundle"]["createUpdaterArtifacts"], true);
        let overlay: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.updater.conf.json")).expect("overlay");
        assert_eq!(overlay["bundle"]["createUpdaterArtifacts"], true);
    }

    #[test]
    fn only_the_confirmed_version_installs() {
        assert!(confirmed(Some("0.2.0"), "0.2.0").is_ok());
        assert_eq!(
            confirmed(Some("0.3.0"), "0.2.0").unwrap_err().code,
            "update_stale"
        );
        assert_eq!(confirmed(None, "0.2.0").unwrap_err().code, "update_stale");
    }

    #[test]
    fn notes_and_version_are_plain_text() {
        let info = UpdateInfo::new(
            "0.2.0\u{202E}",
            "0.1.0",
            Some("Fixed\u{200B} <b>things</b>\n- more"),
            Some(""),
        );
        assert_eq!(info.version, "0.2.0");
        assert_eq!(info.notes, "Fixed <b>things</b>\n- more");
        assert_eq!(info.date, None);
        let long = "x".repeat(NOTES_MAX + 10);
        assert_eq!(
            UpdateInfo::new("1", "0", Some(&long), None)
                .notes
                .chars()
                .count(),
            NOTES_MAX
        );
    }

    #[test]
    fn progress_is_sent_in_steps_and_at_the_end() {
        let mut t = Throttle::new();
        let total = Some(600 * 1024);
        assert!(t.chunk(1024, total).is_some(), "first chunk");
        let sent: Vec<_> = (0..599).filter_map(|_| t.chunk(1024, total)).collect();
        assert_eq!(sent.len(), 3, "two steps of 256 KiB, then the last chunk");
        assert_eq!(
            sent.last(),
            Some(&UpdateProgress::Downloading {
                downloaded: 600 * 1024,
                total
            })
        );
    }

    #[test]
    fn serialises_for_the_ui() {
        assert_eq!(
            serde_json::to_value(UpdateCheck::UpToDate {
                current: "0.1.0".into()
            })
            .unwrap(),
            serde_json::json!({"status": "up_to_date", "current": "0.1.0"})
        );
        assert_eq!(
            serde_json::to_value(UpdateCheck::Available(UpdateInfo::new(
                "0.2.0",
                "0.1.0",
                None,
                Some("2027-01-15T10:00:00Z")
            )))
            .unwrap(),
            serde_json::json!({"status": "available", "version": "0.2.0", "current": "0.1.0", "notes": "", "date": "2027-01-15T10:00:00Z"})
        );
        assert_eq!(
            serde_json::to_value(UpdateMode::SelfUpdate).unwrap(),
            "self_update"
        );
    }
}
