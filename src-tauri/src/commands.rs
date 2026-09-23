//! Everything the settings window and the first-run wizard can ask for.
//! The contract (names, arguments, payloads) is mirrored in `ui/shared/api.ts`.

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

#[tauri::command]
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

#[tauri::command]
pub fn set_config_value(shared: App<'_>, section: String, key: String, value: Json) -> Result<bool, String> {
    config::set_value(&section, &key, to_value(value)?).map_err(err)?;
    shared.reload_config();
    // Recognition changes only take effect when the model is reloaded.
    let needs_restart = section == "stt" || (section == "audio" && key == "device");
    if needs_restart && shared.running() {
        shared.needs_restart.store(true, Ordering::SeqCst);
    }
    Ok(shared.needs_restart.load(Ordering::SeqCst))
}

#[tauri::command]
pub fn get_config_path() -> String {
    config::config_path().display().to_string()
}

#[tauri::command]
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
}

#[tauri::command]
pub fn get_status(shared: App<'_>) -> Status {
    let config = shared.config();
    Status {
        running: shared.running(),
        state: shared.state.lock().unwrap().as_str().to_string(),
        hotkey: current_hotkey(&config),
        version: crate::APP_VERSION.to_string(),
        needs_restart: shared.needs_restart.load(Ordering::SeqCst),
        session: flow_desktop::session::detect().to_string(),
    }
}

/// On GNOME the binding lives in the extension's settings.
pub fn current_hotkey(config: &Config) -> String {
    #[cfg(target_os = "linux")]
    {
        if matches!(flow_desktop::session::detect(), flow_desktop::Session::GnomeWayland { .. }) {
            if let Some(binding) = gnome_binding() {
                return binding;
            }
        }
    }
    config.desktop.hotkey.clone()
}

#[cfg(target_os = "linux")]
fn gnome_binding() -> Option<String> {
    let schemadir = dirs::data_dir()?.join("gnome-shell/extensions/flow@ceyhun.dev/schemas");
    let out = std::process::Command::new("gsettings")
        .args([
            "--schemadir",
            &schemadir.display().to_string(),
            "get",
            "org.gnome.shell.extensions.flow",
            "toggle-dictation",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let cleaned: String = raw.chars().filter(|c| !"[]'\"\n".contains(*c)).collect();
    Some(cleaned.trim().to_string()).filter(|s| !s.is_empty())
}

#[tauri::command]
pub fn set_running(app: AppHandle, shared: App<'_>, on: bool) -> Result<(), String> {
    if on {
        host::start(&shared, Some(&app)).map_err(err)?;
    } else {
        host::turn_off(&shared, Some(&app));
    }
    crate::tray::sync_toggle(&app);
    Ok(())
}

#[tauri::command]
pub fn restart_engine(app: AppHandle, shared: App<'_>) -> Result<(), String> {
    host::restart(&shared, Some(&app)).map_err(err)?;
    crate::tray::sync_toggle(&app);
    Ok(())
}

// -- voice ----------------------------------------------------------------------

#[tauri::command]
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

/// Fetch a model in the background, reporting through `flow:download`.
#[tauri::command]
pub fn download_model(app: AppHandle, shared: App<'_>, id: String) -> Result<(), String> {
    if models::stt_model(&id).is_none() {
        return Err(format!("unknown model {id:?}"));
    }
    let precision = flow_stt::gpu::precision_for(&shared.config().stt.provider);
    let cancel = shared.download_cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let progress = |file: String, received: u64, total: u64, done: bool, error: Option<String>| {
            let _ = app
                .emit("flow:download", DownloadEvent { id: id.clone(), file, received, total, done, error });
        };
        let result = flow_stt::download(&id, precision, &cancel, |p: flow_stt::Progress| {
            progress(p.file, p.received, p.total, false, None);
        });
        progress(String::new(), 0, 0, true, result.err().map(|e| format!("{e:#}")));
    });
    Ok(())
}

/// Stop a running download; the partial file is kept for a later resume.
#[tauri::command]
pub fn cancel_download(shared: App<'_>) {
    shared.download_cancel.store(true, Ordering::SeqCst);
}

#[tauri::command]
pub fn get_compute_report(shared: App<'_>) -> flow_stt::ComputeReport {
    let slot = shared.transcriber.lock().unwrap();
    slot.as_ref().and_then(|(_, t)| t.compute_report()).unwrap_or_else(|| flow_stt::ComputeReport {
        requested: shared.config().stt.provider,
        actual: "not loaded".into(),
        reason: "the recogniser has not been loaded yet".into(),
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
#[tauri::command]
pub fn list_provider_models(shared: App<'_>, provider: String) -> Vec<String> {
    let mut cfg = shared.config().cleanup;
    cfg.backend = provider.clone();
    if provider == "none" {
        return Vec::new();
    }
    let backend = backends::build_backend(&cfg);
    backends::usable_chat_models(&backend.installed_models(), &provider)
}

#[tauri::command]
pub fn get_key_source(provider: String) -> String {
    secrets::key_source(&provider)
}

#[tauri::command]
pub fn set_api_key(provider: String, key: String) -> bool {
    secrets::set_key(&provider, &key)
}

#[tauri::command]
pub fn clear_api_key(provider: String) -> bool {
    secrets::clear_key(&provider)
}

// -- learning ---------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct LearningSummary {
    pub enabled: bool,
    pub count: u64,
    pub terms: Vec<String>,
    pub style: String,
}

#[tauri::command]
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

#[tauri::command]
pub fn forget_history() -> Result<u64, String> {
    let history = flow_core::history::History::open_default().map_err(err)?;
    history.clear().map_err(err)
}

// -- doctor, diagnostics, updates ----------------------------------------------------

#[tauri::command]
pub fn run_doctor(shared: App<'_>) -> Vec<Check> {
    let notes = shared.notes.lock().unwrap().clone();
    doctor::run(&shared.config(), &notes, shared.running())
}

#[tauri::command]
pub fn copy_diagnostics(shared: App<'_>) -> String {
    let text = diagnostics(&shared);
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text.clone());
    }
    text
}

pub fn diagnostics(shared: &Shared) -> String {
    let config = shared.config();
    let mut out = format!("Flow {}\n", crate::APP_VERSION);
    out.push_str(&format!("session: {}\n", flow_desktop::session::detect()));
    out.push_str(&format!("config: {}\n", config::config_path().display()));
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

#[derive(Debug, Serialize)]
pub struct UpdateInfo {
    pub available: bool,
    pub version: Option<String>,
}

/// The updater plugin arrives with the release milestone; until then this
/// honestly says nothing is available.
#[tauri::command]
pub fn check_for_updates() -> UpdateInfo {
    UpdateInfo { available: false, version: None }
}

// -- permissions -------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Permission {
    pub id: String,
    pub label: String,
    pub granted: bool,
    pub required: bool,
    pub help: String,
}

#[tauri::command]
pub fn get_permissions() -> Vec<Permission> {
    let mut list = Vec::new();
    #[cfg(target_os = "linux")]
    {
        use flow_desktop::Session;
        match flow_desktop::session::detect() {
            Session::GnomeWayland { extension } => list.push(Permission {
                id: "hotkey-gnome-extension".into(),
                label: "Flow GNOME Shell extension".into(),
                granted: extension,
                required: true,
                help: "Flow's Shell extension draws the island, hears the shortcut and pastes the text. Enable it with `gnome-extensions enable flow@ceyhun.dev`, then log out and back in.".into(),
            }),
            Session::KdeWayland | Session::LayerShellWayland | Session::OtherWayland => {
                let tool = flow_desktop::linux::wayland::detect_tool();
                list.push(Permission {
                    id: "paste-tool".into(),
                    label: "Paste helper (dotool or ydotool)".into(),
                    granted: tool.is_some(),
                    required: true,
                    help: "Wayland lets no ordinary app type into another. Install dotool or ydotool so Flow can send the paste keystroke.".into(),
                });
                list.push(Permission {
                    id: "hotkey-wayland".into(),
                    label: "Global shortcut".into(),
                    granted: false,
                    required: true,
                    help: "Global shortcuts on this desktop are not wired up yet in this build.".into(),
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
            granted: false,
            required: true,
            help: "Needed to paste into other apps. System Settings → Privacy & Security → Accessibility."
                .into(),
        });
        list.push(Permission {
            id: "screen-recording".into(),
            label: "Screen Recording (window titles only)".into(),
            granted: false,
            required: false,
            help:
                "Lets Flow read the focused window's title so the cleanup model can adapt its tone. Optional."
                    .into(),
        });
    }
    list
}

#[tauri::command]
pub fn request_permission(id: String) -> Result<(), String> {
    warn!("permission request for {id:?} is not implemented on this platform yet");
    Ok(())
}

// -- hotkey and autostart ------------------------------------------------------------

#[tauri::command]
pub fn set_hotkey(app: AppHandle, shared: App<'_>, combo: String) -> Result<(), String> {
    // Validate before saving: a bad combination must not brick startup.
    combo
        .parse::<tauri_plugin_global_shortcut::Shortcut>()
        .map_err(|e| format!("{combo:?} is not a valid shortcut: {e}"))?;
    config::set_value("desktop", "hotkey", combo.clone()).map_err(err)?;
    shared.reload_config();

    let mut guard = shared.engine.lock().unwrap();
    if let Some(running) = guard.as_mut() {
        if let Some(old) = running.shortcut.take() {
            host::unregister_shortcut(&app, &old);
        }
        host::register_shortcut(&app, &combo, running.tx.clone()).map_err(err)?;
        running.shortcut = Some(combo);
    }
    Ok(())
}

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, on: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let launcher = app.autolaunch();
    if on { launcher.enable() } else { launcher.disable() }.map_err(err)
}

// -- first-run wizard ------------------------------------------------------------------

/// Record from the configured microphone for a fixed time. Returns the take
/// and the loudest level seen.
fn record_for(config: &Config, seconds: u64) -> Result<(Vec<f32>, f32), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut recorder = host::recorder(config, tx);
    recorder.start().map_err(err)?;
    std::thread::sleep(std::time::Duration::from_secs(seconds));
    let take = recorder.stop();
    let peak = rx
        .try_iter()
        .filter_map(|ev| match ev {
            flow_core::engine::Event::Level(l) => Some(l),
            _ => None,
        })
        .fold(0.0f32, f32::max);
    Ok((take, peak))
}

#[derive(Debug, Serialize)]
pub struct MicTest {
    pub ok: bool,
    pub peak: f32,
    pub detail: String,
}

/// Record one second and report how loud it was.
#[tauri::command]
pub fn wizard_test_mic(shared: App<'_>) -> Result<MicTest, String> {
    let config = shared.config();
    let (take, peak) = record_for(&config, 1)?;
    let seconds = take.len() as f32 / config.audio.sample_rate as f32;
    let ok = seconds > 0.5;
    let detail = if !ok {
        format!("only {seconds:.2}s captured - is the microphone in use elsewhere?")
    } else if peak < 0.02 {
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
#[tauri::command]
pub fn wizard_test_transcribe(shared: App<'_>) -> Result<TranscribeTest, String> {
    let config = shared.config();
    let transcriber = host::transcriber(&shared, &config);
    transcriber.load().map_err(err)?;
    let (take, _) = record_for(&config, 3)?;
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
#[tauri::command]
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

#[tauri::command]
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

#[tauri::command]
pub fn wizard_complete(app: AppHandle, shared: App<'_>) -> Result<(), String> {
    host::mark_first_run_done().map_err(err)?;
    if let Some(window) = tauri::Manager::get_webview_window(&app, crate::windows::FIRST_RUN) {
        let _ = window.close();
    }
    host::start(&shared, Some(&app)).map_err(err)?;
    crate::tray::sync_toggle(&app);
    Ok(())
}
