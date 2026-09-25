//! Everything the settings window and the first-run wizard can ask for.
//! The contract (names, arguments, payloads) is mirrored in `ui/shared/api.ts`.
//!
//! Anything that touches a disk, a device, the network or the engine is
//! `async`: a plain command runs on the main thread, where it would freeze
//! every window and the tray, and where creating a window deadlocks on
//! Windows. Only instant, self-contained ones stay plain.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use flow_core::backends::{self, PROVIDERS};
use flow_core::config::{self, Config};
use flow_core::engine::{Recorder, Transcriber};
use flow_core::models;
use flow_core::secrets;
use log::warn;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tauri::{AppHandle, Emitter, State};

use crate::doctor::{self, Check};
use crate::host::{self, Shared};

type App<'a> = State<'a, Arc<Shared>>;

fn err<E: std::fmt::Display>(e: E) -> String {
    format!("{e:#}")
}

// -- config -------------------------------------------------------------------

#[tauri::command(async)]
pub fn get_config(shared: App<'_>) -> Config {
    shared.reload_config();
    shared.config()
}

#[derive(Debug, Deserialize)]
pub struct SetValue {
    pub section: String,
    pub key: String,
    pub value: Json,
}

fn to_value(value: Json) -> Result<config::Value, String> {
    Ok(match value {
        Json::Bool(b) => config::Value::Bool(b),
        Json::Number(n) if n.is_i64() => config::Value::Int(n.as_i64().unwrap()),
        Json::Number(n) => config::Value::Float(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => config::Value::Str(s),
        Json::Array(items) => config::Value::List(
            items
                .into_iter()
                .map(|i| match i {
                    Json::String(s) => Ok(s),
                    other => Err(format!("list item {other} is not a string")),
                })
                .collect::<Result<_, _>>()?,
        ),
        other => return Err(format!("unsupported value {other}")),
    })
}

/// Saves one value. Returns whether the engine must restart for the saved
/// settings to apply: it reads its config once, when it starts.
#[tauri::command(async)]
pub fn set_config_value(shared: App<'_>, section: String, key: String, value: Json) -> Result<bool, String> {
    config::set_value(&section, &key, to_value(value)?).map_err(err)?;
    shared.reload_config();
    // The shortcut is re-registered live by `set_hotkey`; the update check
    // reads its switch each time and is no business of the engine.
    let live = section == "desktop" && (key == "hotkey" || key == "check_updates");
    if !live && shared.running() {
        shared.needs_restart.store(true, Ordering::SeqCst);
    }
    Ok(shared.needs_restart.load(Ordering::SeqCst))
}

#[tauri::command]
pub fn get_config_path() -> String {
    config::config_path().display().to_string()
}

#[tauri::command(async)]
pub fn open_config_file(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let path = config::write_default_config().map_err(err)?;
    app.opener().open_path(path.display().to_string(), None::<&str>).map_err(err)
}

// -- status and lifecycle --------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Status {
    pub running: bool,
    pub state: String,
    pub hotkey: String,
    pub version: String,
    pub needs_restart: bool,
    pub session: String,
    /// Why dictation is not working (a model failed to load, the shortcut
    /// is taken), or `None`.
    pub error: Option<String>,
}

#[tauri::command(async)]
pub fn get_status(shared: App<'_>) -> Status {
    let config = shared.config();
    let session = flow_desktop::session::detect();
    Status {
        running: shared.running(),
        state: shared.state.lock().unwrap().as_str().to_string(),
        hotkey: current_hotkey(session, &config),
        version: crate::APP_VERSION.to_string(),
        needs_restart: shared.needs_restart.load(Ordering::SeqCst),
        session: session.to_string(),
        error: shared.last_error(),
    }
}

/// The dictation shortcut in the shortcut plugin's notation, whichever
/// side owns it: on GNOME it lives in the extension's settings.
pub fn current_hotkey(session: flow_desktop::Session, config: &Config) -> String {
    #[cfg(target_os = "linux")]
    {
        // Where the portal hands Flow its shortcut, the desktop chose the key.
        if let Some(key) = crate::host::shortcut::portal_key() {
            return key;
        }
        if matches!(session, flow_desktop::Session::GnomeWayland { .. }) {
            if let Some(binding) = crate::gnome_extension::binding() {
                return binding;
            }
        }
    }
    let _ = session;
    config.desktop.hotkey.clone()
}

#[tauri::command(async)]
pub fn set_running(app: AppHandle, shared: App<'_>, on: bool) -> Result<(), String> {
    if on {
        host::start(&shared, Some(&app)).map_err(err)?;
    } else {
        host::turn_off(&shared, Some(&app));
    }
    crate::tray::sync_toggle(&app);
    Ok(())
}

#[tauri::command(async)]
pub fn restart_engine(app: AppHandle, shared: App<'_>) -> Result<(), String> {
    host::restart(&shared, Some(&app)).map_err(err)?;
    crate::tray::sync_toggle(&app);
    Ok(())
}

// -- voice ----------------------------------------------------------------------

#[tauri::command(async)]
pub fn list_input_devices() -> Vec<flow_audio::DeviceInfo> {
    flow_audio::list_devices()
}

#[derive(Debug, Serialize)]
pub struct SttModelInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    /// Whether the files the configured provider will load are on disk.
    pub downloaded: bool,
    /// Their download size, in MB.
    pub download_mb: u64,
    /// `int8` for the CPU, `fp32` for the GPU.
    pub precision: String,
}

#[tauri::command(async)]
pub fn list_stt_models(shared: App<'_>) -> Vec<SttModelInfo> {
    let precision = flow_stt::gpu::precision_for(&shared.config().stt.provider);
    models::STT_MODELS
        .iter()
        .map(|m| SttModelInfo {
            id: m.id.into(),
            label: m.label.into(),
            description: m.description.into(),
            downloaded: models::is_downloaded(m.id, precision),
            download_mb: models::download_mb(precision),
            precision: match precision {
                models::Precision::Int8 => "int8".into(),
                models::Precision::Fp32 => "fp32".into(),
            },
        })
        .collect()
}

/// The graphics card and whether recognition can use it. The first call
/// loads the CUDA libraries, so it stays off the main thread.
#[tauri::command(async)]
pub fn detect_gpu() -> flow_stt::GpuReport {
    flow_stt::gpu::detect().clone()
}

#[derive(Debug, Serialize, Clone)]
pub struct DownloadEvent {
    pub id: String,
    pub file: String,
    pub received: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// Fetch a model in the background, reporting through `flow:download`. One
/// download at a time: two would share the cancel switch, and two of the
/// same model would write the same file.
#[tauri::command(async)]
pub fn download_model(app: AppHandle, shared: App<'_>, id: String) -> Result<(), String> {
    if models::stt_model(&id).is_none() {
        return Err(format!("unknown model {id:?}"));
    }
    let precision = flow_stt::gpu::precision_for(&shared.config().stt.provider);
    {
        let mut current = shared.download.lock().unwrap();
        if let Some(running) = current.as_ref() {
            return Err(format!("{} is already downloading", running.id));
        }
        *current = Some(DownloadEvent {
            id: id.clone(),
            file: String::new(),
            received: 0,
            total: 0,
            done: false,
            error: None,
        });
    }
    let cancel = shared.download_cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    let slot = shared.download.clone();
    let spawned = std::thread::Builder::new().name("flow-download".into()).spawn(move || {
        let report = |event: DownloadEvent| {
            *slot.lock().unwrap() = (!event.done).then(|| event.clone());
            let _ = app.emit("flow:download", event);
        };
        let result = flow_stt::download(&id, precision, &cancel, |p: flow_stt::Progress| {
            report(DownloadEvent {
                id: id.clone(),
                file: p.file,
                received: p.received,
                total: p.total,
                done: false,
                error: None,
            });
        });
        report(DownloadEvent {
            id: id.clone(),
            file: String::new(),
            received: 0,
            total: 0,
            done: true,
            error: result.err().map(|e| format!("{e:#}")),
        });
    });
    if let Err(e) = spawned {
        *shared.download.lock().unwrap() = None;
        return Err(err(e));
    }
    Ok(())
}

/// The download in progress as last reported, for a window that opens
/// halfway through one; `None` when nothing is downloading.
#[tauri::command]
pub fn get_download(shared: App<'_>) -> Option<DownloadEvent> {
    shared.download.lock().unwrap().clone()
}

/// Stop a running download; the partial file is kept for a later resume.
#[tauri::command]
pub fn cancel_download(shared: App<'_>) {
    shared.download_cancel.store(true, Ordering::SeqCst);
}

#[tauri::command(async)]
pub fn get_compute_report(shared: App<'_>) -> flow_stt::ComputeReport {
    let slot = shared.transcriber.lock().unwrap();
    slot.as_ref().and_then(|(_, t)| t.compute_report()).unwrap_or_else(|| flow_stt::ComputeReport {
        requested: shared.config().stt.provider,
        actual: "not loaded".into(),
        reason: "the recogniser has not been loaded yet".into(),
        fallback: None,
    })
}

// -- cleanup --------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    pub key: String,
    pub label: String,
    pub needs_api_key: bool,
    pub default_model: String,
    pub suggested_models: Vec<String>,
    pub note: String,
    pub has_base_url: bool,
}

#[tauri::command]
pub fn list_providers() -> Vec<ProviderInfo> {
    PROVIDERS
        .iter()
        .map(|p| ProviderInfo {
            key: p.key.into(),
            label: p.label.into(),
            needs_api_key: p.needs_api_key,
            default_model: p.default_model.into(),
            suggested_models: p.suggested_models.iter().map(|s| s.to_string()).collect(),
            note: p.note.into(),
            has_base_url: p.key == "custom",
        })
        .collect()
}

/// The live model list for a provider, using the current config for the
/// endpoint / base URL and the stored key.
#[tauri::command(async)]
pub fn list_provider_models(shared: App<'_>, provider: String) -> Vec<String> {
    let mut cfg = shared.config().cleanup;
    cfg.backend = provider.clone();
    if provider == "none" {
        return Vec::new();
    }
    let backend = backends::build_backend(&cfg);
    backends::usable_chat_models(&backend.installed_models(), &provider)
}

// The keyring can block on an unlock prompt, so these stay off the main
// thread too.
#[tauri::command(async)]
pub fn get_key_source(provider: String) -> String {
    secrets::key_source(&provider)
}

#[tauri::command(async)]
pub fn set_api_key(provider: String, key: String) -> Result<(), String> {
    if secrets::set_key(&provider, &key) {
        Ok(())
    } else {
        Err(KEYRING_FAILED.into())
    }
}

#[tauri::command(async)]
pub fn clear_api_key(provider: String) -> Result<(), String> {
    if secrets::clear_key(&provider) {
        Ok(())
    } else {
        Err(KEYRING_FAILED.into())
    }
}

const KEYRING_FAILED: &str = "The system keyring refused it. Is a keyring (GNOME Keyring, KWallet, \
     Keychain, Credential Manager) running and unlocked? You can also export the provider's \
     environment variable instead.";

// -- learning ---------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct LearningSummary {
    pub enabled: bool,
    pub count: u64,
    pub terms: Vec<String>,
    pub style: String,
}

#[tauri::command(async)]
pub fn get_learning_summary(shared: App<'_>) -> LearningSummary {
    let enabled = shared.config().learning.enabled;
    match flow_core::history::History::open_default() {
        Ok(history) => {
            let (terms, style) = flow_core::learning::load_profile(&history);
            LearningSummary { enabled, count: history.count().unwrap_or(0), terms, style }
        }
        Err(_) => LearningSummary { enabled, count: 0, terms: Vec::new(), style: String::new() },
    }
}

#[tauri::command(async)]
pub fn forget_history(shared: App<'_>) -> Result<u64, String> {
    let history = flow_core::history::History::open_default().map_err(err)?;
    let removed = history.clear().map_err(err)?;
    // The running engine holds the learned profile in memory as well.
    if let Some(engine) = shared.engine.lock().unwrap().as_ref() {
        let _ = engine.tx.send(flow_core::engine::Event::ReloadProfile);
    }
    Ok(removed)
}

// -- doctor, diagnostics, updates ----------------------------------------------------

#[tauri::command(async)]
pub fn run_doctor(shared: App<'_>) -> Vec<Check> {
    let notes = shared.notes.lock().unwrap().clone();
    doctor::run(&shared.config(), &notes, shared.running())
}

#[tauri::command(async)]
pub fn copy_diagnostics(shared: App<'_>) -> String {
    let text = diagnostics(&shared);
    flow_desktop::copy_text(text.clone());
    text
}

pub fn diagnostics(shared: &Shared) -> String {
    let config = shared.config();
    let mut out = format!("Flow {}\n", crate::APP_VERSION);
    out.push_str(&format!("session: {}\n", flow_desktop::session::detect()));
    out.push_str(&format!("config: {}\n", config::config_path().display()));
    if let Some(error) = shared.last_error() {
        out.push_str(&format!("error: {error}\n"));
    }
    out.push_str(&format!("stt: {} / {}\n", config.stt.model, config.stt.provider));
    out.push_str(&format!(
        "cleanup: {} / {} ({})\n",
        config.cleanup.backend,
        config.cleanup.model,
        if config.cleanup.enabled { "on" } else { "off" }
    ));
    for note in shared.notes.lock().unwrap().iter() {
        out.push_str(&format!("desktop: {note}\n"));
    }
    for check in doctor::run(&config, &[], shared.running()) {
        out.push_str(&format!(
            "[{}] {}: {}\n",
            if check.ok { "ok" } else { "FAIL" },
            check.name,
            check.detail
        ));
    }
    out
}

// Updates (check_for_updates, install_update) live in updates.rs.

// -- permissions -------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Permission {
    pub id: String,
    pub label: String,
    pub granted: bool,
    pub required: bool,
    pub help: String,
}

#[tauri::command(async)]
pub fn get_permissions() -> Vec<Permission> {
    // Windows asks for nothing up front.
    #[cfg_attr(windows, allow(unused_mut))]
    let mut list = Vec::new();
    #[cfg(target_os = "linux")]
    {
        use flow_desktop::Session;
        match flow_desktop::session::detect() {
            Session::GnomeWayland { extension } => {
                use flow_desktop::linux::gnome;
                let running = if extension { gnome::running_extension_version() } else { None };
                let outdated = extension && gnome::extension_outdated(running.as_deref());
                list.push(Permission {
                    id: "hotkey-gnome-extension".into(),
                    label: "Flow GNOME Shell extension".into(),
                    granted: extension && !outdated,
                    required: true,
                    help: if outdated {
                        format!(
                            "The Shell is running an older copy of Flow's extension ({}), which lacks parts this Flow relies on. Installing this version's copy takes effect after you log out and back in.",
                            running.map_or("no version".to_string(), |v| format!("version {v}"))
                        )
                    } else {
                        "Flow's Shell extension draws the island, hears the shortcut and pastes the text. Installing it takes effect after you log out and back in.".into()
                    },
                });
            }
            session @ (Session::KdeWayland | Session::LayerShellWayland | Session::OtherWayland) => {
                // On PATH is not enough: ydotool needs its daemon, dotool
                // needs /dev/uinput.
                let tool = flow_desktop::linux::wayland::probe_tool();
                list.push(Permission {
                    id: "paste-tool".into(),
                    label: "Paste helper (dotool or ydotool)".into(),
                    granted: tool.is_ok(),
                    required: true,
                    help: match tool {
                        Ok(name) => format!("{name} is ready to send the paste keystroke."),
                        Err(why) => format!(
                            "Wayland lets no ordinary app type into another, so Flow needs dotool or ydotool to send the paste keystroke. {why}"
                        ),
                    },
                });
                // Through the desktop portal, or the compositor's own
                // bindings running `flow hotkey`.
                let (granted, help) = crate::host::shortcut::permission(session);
                list.push(Permission {
                    id: "hotkey-wayland".into(),
                    label: "Global shortcut".into(),
                    granted,
                    required: true,
                    help,
                });
            }
            _ => {}
        }
    }
    #[cfg(target_os = "macos")]
    {
        list.push(Permission {
            id: "accessibility".into(),
            label: "Accessibility".into(),
            granted: macos::accessibility_granted(),
            required: true,
            help: "Needed to paste into other apps. System Settings → Privacy & Security → Accessibility."
                .into(),
        });
        list.push(Permission {
            id: "screen-recording".into(),
            label: "Screen Recording (window titles only)".into(),
            granted: macos::screen_recording_granted(),
            required: false,
            help:
                "Lets Flow read the focused window's title so the cleanup model can adapt its tone. Optional."
                    .into(),
        });
    }
    list
}

/// Do what granting a permission takes: install the GNOME extension, or
/// open the right pane of System Settings on macOS.
#[tauri::command(async)]
pub fn request_permission(app: AppHandle, id: String) -> Result<(), String> {
    match id.as_str() {
        #[cfg(target_os = "linux")]
        "hotkey-gnome-extension" => crate::gnome_extension::install(&app).map_err(err),
        #[cfg(target_os = "macos")]
        "accessibility" | "screen-recording" => {
            use tauri_plugin_opener::OpenerExt;
            if id == "accessibility" {
                // Asking puts Flow in the list, switched off, for the user to tick.
                macos::prompt_accessibility();
            }
            let pane = if id == "accessibility" { "Privacy_Accessibility" } else { "Privacy_ScreenCapture" };
            let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
            app.opener().open_url(url, None::<&str>).map_err(err)
        }
        // Shows the desktop's shortcut dialog, or explains the key bindings
        // where its portal has none.
        #[cfg(target_os = "linux")]
        "hotkey-wayland" => crate::host::shortcut::request(&app),
        "paste-tool" => Err("Install dotool or ydotool with your package manager (ydotool also needs \
             its ydotoold service running), then check again."
            .into()),
        _ => {
            let _ = &app;
            warn!("permission request for {id:?} has nothing to do on this platform");
            Ok(())
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    //! The two privacy checks macOS answers without asking the user.

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
        fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> u8;
        static kAXTrustedCheckOptionPrompt: *const std::ffi::c_void;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> u8;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDictionaryCreate(
            allocator: *const std::ffi::c_void,
            keys: *const *const std::ffi::c_void,
            values: *const *const std::ffi::c_void,
            count: isize,
            key_callbacks: *const std::ffi::c_void,
            value_callbacks: *const std::ffi::c_void,
        ) -> *const std::ffi::c_void;
        fn CFRelease(cf: *const std::ffi::c_void);
        static kCFBooleanTrue: *const std::ffi::c_void;
        static kCFTypeDictionaryKeyCallBacks: u8;
        static kCFTypeDictionaryValueCallBacks: u8;
    }

    pub fn accessibility_granted() -> bool {
        // SAFETY: takes no arguments and only reads the TCC database.
        unsafe { AXIsProcessTrusted() != 0 }
    }

    pub fn screen_recording_granted() -> bool {
        // SAFETY: as above; available since macOS 10.15, below our minimum.
        unsafe { CGPreflightScreenCaptureAccess() != 0 }
    }

    /// Shows the system's own "Flow would like to control this computer"
    /// prompt, which also adds Flow to the Accessibility list.
    pub fn prompt_accessibility() {
        // SAFETY: a one-entry CFDictionary built from constant CF objects,
        // with the standard CFType callbacks, released after use.
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast(),
                std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast(),
            );
            if !options.is_null() {
                AXIsProcessTrustedWithOptions(options);
                CFRelease(options);
            }
        }
    }
}

// -- hotkey and autostart ------------------------------------------------------------

/// Change the dictation shortcut. Nothing changes unless the new one can
/// be had: a combination another app owns must not leave Flow with none,
/// or stop it from starting next time.
#[tauri::command(async)]
pub fn set_hotkey(app: AppHandle, shared: App<'_>, combo: String) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

    let parsed = combo.parse::<Shortcut>().map_err(|e| format!("{combo:?} is not a valid shortcut: {e}"))?;

    // On GNOME the extension owns the shortcut; it rebinds as soon as its
    // setting changes.
    #[cfg(target_os = "linux")]
    if matches!(flow_desktop::session::detect(), flow_desktop::Session::GnomeWayland { extension: true }) {
        return crate::gnome_extension::set_binding(&combo).map_err(err);
    }
    // Where the desktop's portal hands Flow its shortcut, the desktop picks
    // the key; asking it for another would change nothing.
    #[cfg(target_os = "linux")]
    if flow_desktop::linux::portal::active() {
        return Err(crate::host::shortcut::OWNED_BY_THE_DESKTOP.into());
    }

    // The engine's sender and current shortcut, without holding the lock
    // while the plugin registers (which can wait for the main thread).
    let engine = shared.engine.lock().unwrap().as_ref().map(|r| (r.tx.clone(), r.shortcut.clone()));
    match engine {
        Some((_, Some(old))) if old == combo => {}
        Some((tx, old)) => {
            host::register_shortcut(&app, &combo, tx).map_err(err)?;
            if let Some(old) = old {
                host::unregister_shortcut(&app, &old);
            }
            if let Some(running) = shared.engine.lock().unwrap().as_mut() {
                running.shortcut = Some(combo.clone());
            }
        }
        None => {
            // Not running: check it can be had, then let it go again.
            app.global_shortcut()
                .register(parsed)
                .map_err(|e| format!("could not use {combo:?}: {e} - another app may own it"))?;
            let _ = app.global_shortcut().unregister(parsed);
        }
    }
    config::set_value("desktop", "hotkey", combo).map_err(err)?;
    shared.reload_config();
    Ok(())
}

#[tauri::command(async)]
pub fn get_autostart(app: AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command(async)]
pub fn set_autostart(app: AppHandle, on: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let launcher = app.autolaunch();
    if on { launcher.enable() } else { launcher.disable() }.map_err(err)
}

// -- first-run wizard ------------------------------------------------------------------

/// Record from the configured microphone for a fixed time, showing the
/// level live in the windows as `flow:level`. Returns the take and the
/// loudest level seen.
fn record_for(app: &AppHandle, config: &Config, seconds: f32) -> Result<(Vec<f32>, f32), String> {
    use flow_core::engine::Event;

    let (tx, rx) = std::sync::mpsc::channel();
    let mut recorder = host::recorder(config, tx);
    recorder.start().map_err(err)?;
    let until = std::time::Instant::now() + std::time::Duration::from_secs_f32(seconds);
    let mut peak = 0.0f32;
    let mut broken = None;
    while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Event::Level(level)) => {
                peak = peak.max(level);
                let _ = app.emit("flow:level", serde_json::json!({ "level": level }));
            }
            Ok(Event::MicError(message)) => {
                broken = Some(message);
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let take = recorder.stop();
    if let Some(message) = broken {
        return Err(format!("the microphone stopped: {message}"));
    }
    Ok((take, peak))
}

#[derive(Debug, Serialize)]
pub struct MicTest {
    pub ok: bool,
    pub peak: f32,
    pub detail: String,
}

/// A peak below this is a muted or wrong microphone, not a quiet voice.
const SILENT: f32 = 0.02;

/// Record two seconds and report how loud it was.
#[tauri::command(async)]
pub fn wizard_test_mic(app: AppHandle, shared: App<'_>) -> Result<MicTest, String> {
    let config = shared.config();
    let (take, peak) = record_for(&app, &config, 2.0)?;
    let seconds = take.len() as f32 / config.audio.sample_rate as f32;
    let captured = seconds > 1.0;
    let ok = captured && peak >= SILENT;
    let detail = if !captured {
        format!("only {seconds:.2}s captured - is the microphone in use elsewhere?")
    } else if peak < SILENT {
        "captured audio, but it was silent - check the input level".into()
    } else {
        format!("heard you: peak level {:.0}%", peak * 100.0)
    };
    Ok(MicTest { ok, peak, detail })
}

#[derive(Debug, Serialize)]
pub struct TranscribeTest {
    pub text: String,
    pub seconds: f32,
}

/// Record for three seconds and recognise it, loading the model if needed.
#[tauri::command(async)]
pub fn wizard_test_transcribe(app: AppHandle, shared: App<'_>) -> Result<TranscribeTest, String> {
    let config = shared.config();
    let transcriber = host::transcriber(&shared, &config);
    transcriber.load().map_err(err)?;
    let (take, _) = record_for(&app, &config, 3.0)?;
    let started = std::time::Instant::now();
    let text = transcriber.transcribe(&take, config.audio.sample_rate).map_err(err)?;
    Ok(TranscribeTest { text, seconds: started.elapsed().as_secs_f32() })
}

#[derive(Debug, Serialize)]
pub struct PasteTest {
    pub ok: bool,
    pub restored: bool,
    pub detail: String,
}

/// Paste a marker into whatever is focused (the wizard's own text field)
/// and check the clipboard came back.
#[tauri::command(async)]
pub fn wizard_test_paste(shared: App<'_>) -> Result<PasteTest, String> {
    let config = shared.config();
    let backends = flow_desktop::build(&config.desktop).map_err(err)?;
    let previous = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok());
    let marker = "Flow paste test";
    let focus = backends.focus.context().map(|c| c.app).unwrap_or_default();
    match backends.injector.insert(marker) {
        Ok(()) => {
            std::thread::sleep(std::time::Duration::from_millis(700));
            let now = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok());
            let restored = previous.is_none() || now == previous;
            Ok(PasteTest {
                ok: true,
                restored,
                detail: format!("pasted into {}", if focus.is_empty() { "the focused app" } else { &focus }),
            })
        }
        Err(e) => Ok(PasteTest { ok: false, restored: false, detail: e.to_string() }),
    }
}

#[derive(Debug, Serialize)]
pub struct CleanupTest {
    pub ok: bool,
    pub sample: String,
}

#[tauri::command(async)]
pub fn wizard_test_cleanup(shared: App<'_>) -> CleanupTest {
    let cleaner = flow_core::cleanup::build_cleaner(&shared.config().cleanup);
    let (ok, why) = cleaner.available();
    if !ok {
        return CleanupTest { ok: false, sample: why };
    }
    let sample = cleaner.clean(
        "so um can you look at the the login page it uh it hangs on submit",
        &flow_core::engine::FocusContext::default(),
    );
    CleanupTest { ok: true, sample }
}

/// Start dictating and close the wizard. The engine starts first, so a
/// failure (say, a shortcut another app owns) is shown in the wizard rather
/// than lost behind a closed window.
#[tauri::command(async)]
pub fn wizard_complete(app: AppHandle, shared: App<'_>) -> Result<(), String> {
    host::start(&shared, Some(&app)).map_err(err)?;
    crate::tray::sync_toggle(&app);
    host::mark_first_run_done().map_err(err)?;
    if let Some(window) = tauri::Manager::get_webview_window(&app, crate::windows::FIRST_RUN) {
        let _ = window.close();
    }
    Ok(())
}
