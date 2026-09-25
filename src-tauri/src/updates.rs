//! Updates from GitHub Releases, through tauri-plugin-updater.
//!
//! The release workflow publishes `latest.json` beside the installers, each
//! update artifact signed with Flow's updater key (docs/releasing.md); the
//! public half is `plugins.updater.pubkey` in tauri.conf.json, and nothing
//! that key did not sign is installed.
//!
//! Only a copy that can swap itself for the new version offers to: the
//! AppImage (`$APPIMAGE`), the Windows installer's copy and the macOS app.
//! A .deb or .rpm belongs to the package manager and a build from source to
//! its checkout, so those hear about a new version and are pointed at the
//! releases page instead.
//!
//! Apart from the button in settings, a copy installed from a release asks
//! once a day at most and only says so with a notification;
//! `desktop.check_updates = false` stops that.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flow_core::config::data_dir;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};

use crate::host::{self, Shared};

/// Where a copy that cannot update itself gets the new version.
pub const RELEASES_URL: &str = "https://github.com/ceyhuncakir/flow/releases/latest";

/// The least time between two checks Flow makes on its own.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The update the last check found, for `install_update`.
static PENDING: Mutex<Option<Update>> = Mutex::new(None);
static INSTALLING: AtomicBool = AtomicBool::new(false);
/// Set once Flow stopped itself for the Windows installer.
static HANDED_OVER: AtomicBool = AtomicBool::new(false);

fn describe(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

// -- how this copy was installed ---------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Install {
    /// A Linux AppImage started through its runtime, which sets `$APPIMAGE`.
    AppImage,
    /// An AppImage whose folder Flow may not write to, or one run unpacked.
    AppImageReadOnly,
    Deb,
    Rpm,
    /// The Windows installer's copy.
    Nsis,
    Msi,
    /// A macOS app bundle.
    App,
    /// Built from source: scripts/install-app.sh or `cargo run --release`.
    Source,
    /// A debug build.
    Dev,
}

impl Install {
    pub fn detect(app: &AppHandle) -> Install {
        if cfg!(debug_assertions) {
            return Install::Dev;
        }
        use tauri::utils::config::BundleType;
        // The bundler stamps the type into the binary; `--no-bundle` leaves
        // it unset, and on macOS every binary claims App, bundled or not.
        match tauri::utils::platform::bundle_type() {
            Some(BundleType::AppImage) => appimage(app),
            Some(BundleType::Deb) => Install::Deb,
            Some(BundleType::Rpm) => Install::Rpm,
            Some(BundleType::Nsis) => Install::Nsis,
            Some(BundleType::Msi) => Install::Msi,
            Some(BundleType::App | BundleType::Dmg) if in_app_bundle() => Install::App,
            _ => Install::Source,
        }
    }

    pub fn can_install(self) -> bool {
        matches!(self, Install::AppImage | Install::Nsis | Install::Msi | Install::App)
    }

    /// Whether Flow checks on its own. Not for builds from source: whoever
    /// builds Flow knows where newer code comes from.
    fn is_release(self) -> bool {
        !matches!(self, Install::Source | Install::Dev)
    }

    /// For a copy that cannot update itself: how to update instead.
    fn how(self) -> Option<&'static str> {
        Some(match self {
            Install::AppImageReadOnly => {
                "Flow may not replace this AppImage. Download the new one from the releases page."
            }
            Install::Deb => {
                "Flow came from a .deb package. Install the new .deb from the releases page, or update it the way you installed it."
            }
            Install::Rpm => {
                "Flow came from an .rpm package. Install the new .rpm from the releases page, or update it the way you installed it."
            }
            Install::Source => "This copy was built from source. Pull the new version and run scripts/install-app.sh again.",
            Install::Dev => "A development build does not update itself.",
            _ => return None,
        })
    }
}

#[cfg(target_os = "linux")]
fn appimage(app: &AppHandle) -> Install {
    use tauri::Manager;
    // The updater renames the AppImage aside and writes the new one in its
    // place, so it needs the folder, not just the file.
    let writable =
        app.env().appimage.as_deref().map(Path::new).and_then(Path::parent).is_some_and(dir_writable);
    if writable {
        Install::AppImage
    } else {
        Install::AppImageReadOnly
    }
}

#[cfg(not(target_os = "linux"))]
fn appimage(_app: &AppHandle) -> Install {
    Install::Source
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".flow-update-probe-{}", std::process::id()));
    let ok = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).is_ok();
    if ok {
        let _ = std::fs::remove_file(&probe);
    }
    ok
}

/// Inside `Something.app/Contents/MacOS/`.
fn in_app_bundle() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.ends_with("Contents/MacOS")))
        .unwrap_or(false)
}

// -- checking --------------------------------------------------------------------

/// What `check_for_updates` returns; mirrored in ui/shared/api.ts.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub available: bool,
    /// The running version.
    pub current: String,
    /// The newer version, when there is one.
    pub version: Option<String>,
    /// The release notes `latest.json` carries.
    pub notes: Option<String>,
    /// When it was published (RFC 3339), when the release says.
    pub date: Option<String>,
    pub install: Install,
    /// Whether "Install and restart" can replace this copy.
    pub can_install: bool,
    /// When it cannot: how to update instead.
    pub how: Option<String>,
    pub release_url: String,
}

fn updater(app: &AppHandle, shared: &Arc<Shared>) -> Result<Updater, String> {
    let handle = app.clone();
    let shared = shared.clone();
    app.updater_builder()
        .timeout(Duration::from_secs(30))
        // Windows only: the installer is about to start and this process to
        // end with `exit`, never passing RunEvent::Exit. Do what Quit does.
        .on_before_exit(move || {
            HANDED_OVER.store(true, Ordering::SeqCst);
            host::stop(&shared, Some(&handle));
            host::release_gpu(&shared);
            handle.cleanup_before_exit();
        })
        .build()
        .map_err(describe)
}

async fn fetch(app: &AppHandle, shared: &Arc<Shared>) -> Result<Option<Update>, String> {
    use tauri_plugin_updater::Error;
    let result = updater(app, shared)?.check().await;
    let update = result.map_err(|e| match e {
        Error::ReleaseNotFound => "No release with update information is published yet.".to_string(),
        Error::TargetsNotFound(_) | Error::TargetNotFound(_) => {
            "The latest release has no update for this platform yet.".to_string()
        }
        Error::Reqwest(e) => format!("Could not reach GitHub: {e}"),
        other => describe(other),
    })?;
    Stamp::load().checked(update.as_ref().map(|u| u.version.as_str())).save();
    Ok(update)
}

fn info(app: &AppHandle, install: Install, update: Option<&Update>) -> UpdateInfo {
    UpdateInfo {
        available: update.is_some(),
        current: app.package_info().version.to_string(),
        version: update.map(|u| u.version.clone()),
        notes: update.and_then(|u| u.body.clone()).filter(|n| !n.trim().is_empty()),
        date: update.and_then(|u| u.raw_json.get("pub_date")).and_then(|d| d.as_str()).map(str::to_string),
        install,
        can_install: install.can_install(),
        how: install.how().map(str::to_string),
        release_url: RELEASES_URL.to_string(),
    }
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle, shared: State<'_, Arc<Shared>>) -> Result<UpdateInfo, String> {
    let install = Install::detect(&app);
    let update = fetch(&app, &shared).await?;
    let info = info(&app, install, update.as_ref());
    *PENDING.lock().unwrap() = update;
    Ok(info)
}

// -- installing ------------------------------------------------------------------

/// `flow:update`, while `install_update` downloads. `total` is 0 while unknown.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateEvent {
    pub received: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

fn report(app: &AppHandle, event: UpdateEvent) {
    if let Err(err) = app.emit("flow:update", event) {
        warn!("could not report update progress: {err}");
    }
}

/// Download the update the last check found, verify its signature, put it
/// in place and restart into it. On Windows the installer takes over and
/// starts the new Flow itself.
#[tauri::command]
pub async fn install_update(app: AppHandle, shared: State<'_, Arc<Shared>>) -> Result<(), String> {
    let install = Install::detect(&app);
    if !install.can_install() {
        return Err(install.how().unwrap_or("This copy of Flow cannot update itself.").to_string());
    }
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("The update is already being installed.".into());
    }
    let result = download_and_install(&app, &shared).await;
    if let Err(error) = &result {
        INSTALLING.store(false, Ordering::SeqCst);
        warn!("update failed: {error}");
        report(&app, UpdateEvent { received: 0, total: 0, done: true, error: Some(error.clone()) });
    }
    result
}

async fn download_and_install(app: &AppHandle, shared: &Arc<Shared>) -> Result<(), String> {
    let pending = PENDING.lock().unwrap().take();
    let update = match pending {
        Some(update) => update,
        None => fetch(app, shared).await?.ok_or("Flow is already up to date.")?,
    };
    info!("downloading Flow {}", update.version);

    let (mut received, mut total) = (0u64, 0u64);
    let mut last = Instant::now();
    report(app, UpdateEvent { received, total, done: false, error: None });
    let bytes = update
        .download(
            |chunk, length| {
                received += chunk as u64;
                total = length.unwrap_or(0);
                if last.elapsed() >= Duration::from_millis(100) {
                    last = Instant::now();
                    report(app, UpdateEvent { received, total, done: false, error: None });
                }
            },
            || {},
        )
        .await
        .map_err(|e| format!("Could not download Flow {}: {}", update.version, describe(e)))?;
    report(app, UpdateEvent { received, total: total.max(received), done: false, error: None });

    info!("installing Flow {}", update.version);
    let version = update.version.clone();
    let installed =
        tauri::async_runtime::spawn_blocking(move || update.install(bytes)).await.map_err(describe)?;
    if let Err(error) = installed {
        let error = format!("Could not install Flow {version}: {}", describe(error));
        if HANDED_OVER.load(Ordering::SeqCst) {
            // Windows: Flow stopped for an installer that then did not
            // start. Recognition is gone with the GPU; start over.
            crate::windows::notify(app, "Flow could not update", &error);
            app.request_restart();
        }
        return Err(error);
    }

    info!("installed Flow {version}; restarting into it");
    report(app, UpdateEvent { received, total: total.max(received), done: true, error: None });
    // Through RunEvent::Exit, which stops the engine and hands the GPU back.
    app.request_restart();
    Ok(())
}

#[tauri::command(async)]
pub fn open_release_page(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(RELEASES_URL, None::<&str>).map_err(describe)
}

// -- checking on its own ---------------------------------------------------------

/// When Flow last asked, and which version it last told the user about.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Stamp {
    /// Unix seconds.
    #[serde(default)]
    checked: i64,
    #[serde(default)]
    told: Option<String>,
}

impl Stamp {
    fn path() -> std::path::PathBuf {
        data_dir().join("update-check.json")
    }

    fn load() -> Stamp {
        std::fs::read(Self::path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// A check just happened; `found` is the newer version, which whoever
    /// sees the result (settings, or the notification) now knows about.
    fn checked(mut self, found: Option<&str>) -> Stamp {
        self.checked = chrono::Utc::now().timestamp();
        if let Some(version) = found {
            self.told = Some(version.to_string());
        }
        self
    }

    fn due(&self) -> bool {
        let elapsed = chrono::Utc::now().timestamp().saturating_sub(self.checked);
        // A clock set back makes `elapsed` negative: ask again.
        !(0..CHECK_EVERY.as_secs() as i64).contains(&elapsed)
    }

    fn save(&self) {
        let path = Self::path();
        let written = std::fs::create_dir_all(data_dir())
            .and_then(|()| std::fs::write(&path, serde_json::to_vec(self).unwrap_or_default()));
        if let Err(err) = written {
            warn!("could not write {}: {err}", path.display());
        }
    }
}

/// Check once a day at most while Flow runs, and send a notification the
/// first time a new version turns up. Only for copies installed from a
/// release, and only while `desktop.check_updates` is on.
pub fn watch(app: AppHandle, shared: Arc<Shared>) {
    let install = Install::detect(&app);
    if !install.is_release() {
        info!("{install:?} build: Flow does not check for updates on its own");
        return;
    }
    let spawned = std::thread::Builder::new().name("update-check".into()).spawn(move || {
        // Not in the rush of starting up, while the model loads.
        std::thread::sleep(Duration::from_secs(90));
        loop {
            let wanted = shared.config().desktop.check_updates;
            if wanted && !INSTALLING.load(Ordering::SeqCst) && Stamp::load().due() {
                tauri::async_runtime::block_on(check_quietly(&app, &shared, install));
            }
            std::thread::sleep(Duration::from_secs(60 * 60));
        }
    });
    if let Err(err) = spawned {
        warn!("could not start the update check: {err}");
    }
}

async fn check_quietly(app: &AppHandle, shared: &Arc<Shared>, install: Install) {
    // Read before `fetch`, which records the version it finds.
    let told = Stamp::load().told;
    match fetch(app, shared).await {
        Ok(Some(update)) => {
            if told.as_deref() != Some(update.version.as_str()) {
                let body = if install.can_install() {
                    "Open Flow's settings to install it."
                } else {
                    "Flow's settings link to the release."
                };
                crate::windows::notify(app, &format!("Flow {} is available", update.version), body);
            }
            *PENDING.lock().unwrap() = Some(update);
        }
        Ok(None) => info!("update check: Flow is up to date"),
        Err(err) => {
            // Offline, or GitHub unreachable: try again tomorrow, not hourly.
            Stamp::load().checked(None).save();
            warn!("update check failed: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_stamp_is_due() {
        assert!(Stamp::default().due());
    }

    #[test]
    fn a_check_waits_a_day() {
        let stamp = Stamp::default().checked(None);
        assert!(!stamp.due());
        let yesterday = Stamp { checked: stamp.checked - CHECK_EVERY.as_secs() as i64, told: None };
        assert!(yesterday.due());
        let future = Stamp { checked: stamp.checked + 3600, told: None };
        assert!(future.due(), "a clock set back asks again");
    }

    #[test]
    fn a_check_remembers_the_version_it_found() {
        let stamp = Stamp::default().checked(Some("0.4.0"));
        assert_eq!(stamp.told.as_deref(), Some("0.4.0"));
        let stamp = stamp.checked(None);
        assert_eq!(stamp.told.as_deref(), Some("0.4.0"), "nothing newer keeps it");
        let stamp = stamp.checked(Some("0.5.0"));
        assert_eq!(stamp.told.as_deref(), Some("0.5.0"));
    }

    #[test]
    fn only_bundles_that_own_their_files_install() {
        for install in [Install::AppImage, Install::Nsis, Install::Msi, Install::App] {
            assert!(install.can_install(), "{install:?}");
            assert!(install.how().is_none(), "{install:?}");
        }
        for install in [Install::AppImageReadOnly, Install::Deb, Install::Rpm, Install::Source, Install::Dev]
        {
            assert!(!install.can_install(), "{install:?}");
            assert!(install.how().is_some(), "{install:?}");
        }
        assert!(!Install::Source.is_release());
        assert!(!Install::Dev.is_release());
        assert!(Install::Deb.is_release());
    }

    #[test]
    fn the_updater_config_is_what_the_plugin_reads() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let updater: tauri_plugin_updater::Config =
            serde_json::from_value(conf["plugins"]["updater"].clone()).expect("plugins.updater");
        assert!(updater.require_signed_version);
        assert_eq!(
            updater.endpoints.iter().map(|u| u.as_str()).collect::<Vec<_>>(),
            ["https://github.com/ceyhuncakir/flow/releases/latest/download/latest.json"]
        );
        assert!(!updater.pubkey.is_empty());
        // Local builds must not need the private key: only the release
        // overlay asks for updater artifacts.
        assert!(conf["bundle"].get("createUpdaterArtifacts").is_none());
        let release: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.release.conf.json")).unwrap();
        assert_eq!(release["bundle"]["createUpdaterArtifacts"], true);
    }

    #[test]
    fn a_writable_folder_is_seen_as_one() {
        let dir = tempfile::tempdir().unwrap();
        assert!(dir_writable(dir.path()));
        assert!(!dir_writable(&dir.path().join("missing")));
    }
}
