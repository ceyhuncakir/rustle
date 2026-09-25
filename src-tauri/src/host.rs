//! Builds the engine from the crates and runs it on its own thread.
//!
//! Works with or without a Tauri `AppHandle`: the headless mode on GNOME has
//! no windows at all, so anything that needs the app (the webview overlay,
//! the global-shortcut plugin) is optional here.
//!
//! The tray, the settings window, the wizard and a signal can all ask to
//! start or stop the engine at once, so those are serialised by
//! [`Shared::lifecycle`]. Whoever holds it may wait for the main thread
//! (creating the overlay window, registering a shortcut), so the main thread
//! itself must never block on it: the tray hands the work to a thread, and
//! the exit path only tries the lock.

use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use log::{info, warn};
use rustle_core::backends;
use rustle_core::cleanup::build_cleaner;
use rustle_core::config::{data_dir, Config};
use rustle_core::engine::{Deps, Engine, Event, Focus, Hotkey, HotkeyEvent, Injector, Overlay, State};
use rustle_core::history::History;
use rustle_core::learning::Learner;
use rustle_desktop::{Backends, HotkeySource, OverlayChoice};
use tauri::{AppHandle, Emitter};

#[cfg(target_os = "linux")]
pub mod shortcut;

/// Everything the commands, the tray and the CLI share.
pub struct Shared {
    pub config: Mutex<Config>,
    pub engine: Mutex<Option<Running>>,
    pub state: Arc<Mutex<State>>,
    pub notes: Mutex<Vec<String>>,
    /// The loaded recogniser and the provider it was built for.
    pub transcriber: Mutex<Option<(String, Arc<rustle_stt::Parakeet>)>>,
    /// Set by settings changes that only take effect after a restart.
    pub needs_restart: AtomicBool,
    /// Raised by `cancel_download`; the downloader polls it.
    pub download_cancel: Arc<AtomicBool>,
    /// The download in progress as last reported, so a window opened
    /// mid-download can pick it up. `None` when nothing is downloading.
    pub download: Arc<Mutex<Option<crate::commands::DownloadEvent>>>,
    /// Why dictation is not working, for the status row. Cleared on start.
    pub last_error: Arc<Mutex<Option<String>>>,
    /// Held while the engine starts or stops; see the module docs.
    pub lifecycle: Mutex<()>,
    /// Whether a thread already waits for the GNOME extension to appear.
    pub watching_extension: AtomicBool,
}

pub struct Running {
    pub tx: Sender<Event>,
    pub thread: JoinHandle<()>,
    /// The desktop's hotkey, and on Linux the control socket beside it.
    pub hotkeys: Vec<Box<dyn Hotkey>>,
    pub shortcut: Option<String>,
    /// Unloads a local cleanup model if Rustle dies before the engine can.
    pub watchdog: Option<crate::watchdog::Watchdog>,
    /// Held for as long as this engine runs; see [`claim_engine`].
    pub _claim: Option<File>,
}

impl Shared {
    pub fn load() -> Shared {
        let config = Config::load().unwrap_or_else(|err| {
            warn!("could not read config, using defaults: {err:#}");
            Config::default()
        });
        Shared {
            config: Mutex::new(config),
            engine: Mutex::new(None),
            state: Arc::new(Mutex::new(State::Hidden)),
            notes: Mutex::new(Vec::new()),
            transcriber: Mutex::new(None),
            needs_restart: AtomicBool::new(false),
            download_cancel: Arc::default(),
            download: Arc::default(),
            last_error: Arc::default(),
            lifecycle: Mutex::new(()),
            watching_extension: AtomicBool::new(false),
        }
    }

    pub fn reload_config(&self) {
        match Config::load() {
            Ok(config) => *self.config.lock().unwrap() = config,
            Err(err) => warn!("could not reload config: {err:#}"),
        }
    }

    pub fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }

    /// Whether an engine is up. One whose thread ended - its models failed
    /// to load - is not, even before anyone cleans it up.
    pub fn running(&self) -> bool {
        self.engine.lock().unwrap().as_ref().is_some_and(|r| !r.thread.is_finished())
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().clone()
    }

    fn set_error(&self, error: Option<String>) {
        *self.last_error.lock().unwrap() = error;
    }

    fn lifecycle(&self) -> MutexGuard<'_, ()> {
        self.lifecycle.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Records every state the engine sets so the tray and the status row can
/// show it, whatever draws the island.
struct Tracking {
    inner: Arc<dyn Overlay>,
    state: Arc<Mutex<State>>,
    app: Option<AppHandle>,
}

impl Overlay for Tracking {
    fn set_state(&self, state: State) {
        *self.state.lock().unwrap() = state;
        if let Some(app) = &self.app {
            // Posted to the main thread, not run and waited for: this is the
            // engine thread, and stopping the engine means the main thread
            // waits for it to finish. Waiting on each other froze Quit.
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || crate::tray::reflect_state(&handle, state));
        }
        self.inner.set_state(state);
    }
    fn set_text(&self, text: &str) {
        self.inner.set_text(text);
    }
    fn push_level(&self, level: f32) {
        if let Some(app) = &self.app {
            // The wizard and the settings window draw the level too.
            for window in [crate::windows::FIRST_RUN, crate::windows::SETTINGS] {
                let _ = app.emit_to(window, "rustle:level", serde_json::json!({ "level": level }));
            }
        }
        self.inner.push_level(level);
    }
}

pub struct NullOverlay;
impl Overlay for NullOverlay {
    fn set_state(&self, _: State) {}
    fn set_text(&self, _: &str) {}
    fn push_level(&self, _: f32) {}
}

pub fn first_run_pending() -> bool {
    !data_dir().join("first-run-done").exists()
}

pub fn mark_first_run_done() -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir())?;
    std::fs::write(data_dir().join("first-run-done"), chrono::Utc::now().to_rfc3339())?;
    Ok(())
}

/// The microphone as configured; an empty device name means the default.
pub fn recorder(config: &Config, tx: Sender<Event>) -> rustle_audio::CpalRecorder {
    let device = Some(config.audio.device.clone()).filter(|d| !d.is_empty());
    rustle_audio::CpalRecorder::new(device, config.audio.sample_rate, tx)
}

/// The transcriber is shared with the wizard and the CLI, and loading it is
/// slow, so it is built once and reused across engine restarts unless the
/// model or compute setting changed.
pub fn transcriber(shared: &Shared, config: &Config) -> Arc<rustle_stt::Parakeet> {
    let mut slot = shared.transcriber.lock().unwrap();
    if let Some((provider, existing)) = slot.as_ref() {
        if existing.id() == config.stt.model && *provider == config.stt.provider {
            return existing.clone();
        }
    }
    let fresh = Arc::new(rustle_stt::Parakeet::new(&config.stt.model, &config.stt.provider));
    *slot = Some((config.stt.provider.clone(), fresh.clone()));
    fresh
}

/// An engine wired to the given desktop pieces, not yet prepared or running.
pub fn assemble(
    shared: &Shared,
    config: &Config,
    overlay: Arc<dyn Overlay>,
    focus: Arc<dyn Focus>,
    injector: Arc<dyn Injector>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
) -> anyhow::Result<Engine> {
    // Learning must never stand in the way of dictation: a damaged or
    // locked history database only turns it off for this run.
    let history = config.learning.enabled.then(History::open_default).and_then(|opened| match opened {
        Ok(history) => Some(Arc::new(history)),
        Err(err) => {
            warn!("history unavailable, learning is off until restart: {err:#}");
            None
        }
    });
    let learner =
        (history.is_some() && config.cleanup.enabled && config.cleanup.backend != "none").then(|| {
            let backend: Arc<dyn backends::Backend> = Arc::from(backends::build_backend(&config.cleanup));
            Arc::new(Learner::new(backend))
        });

    Ok(Engine::with_channel(
        config.clone(),
        Deps {
            overlay,
            focus,
            injector,
            recorder: Box::new(recorder(config, tx.clone())),
            transcriber: transcriber(shared, config),
            cleaner: Arc::from(build_cleaner(&config.cleanup)),
            history,
            learner,
        },
        tx,
        rx,
    ))
}

/// Build everything and start the engine thread.
pub fn start(shared: &Shared, app: Option<&AppHandle>) -> anyhow::Result<()> {
    let _lifecycle = shared.lifecycle();
    start_locked(shared, app)
}

fn start_locked(shared: &Shared, app: Option<&AppHandle>) -> anyhow::Result<()> {
    if shared.running() {
        return Ok(());
    }
    // An engine whose thread already ended still holds its shortcut.
    stop_locked(shared, app);
    shared.set_error(None);
    let result = launch(shared, app);
    if let Err(err) = &result {
        shared.set_error(Some(format!("{err:#}")));
    }
    result
}

fn launch(shared: &Shared, app: Option<&AppHandle>) -> anyhow::Result<()> {
    let claim = claim_engine()?;
    shared.reload_config();
    let config = shared.config();

    let backends: Backends = rustle_desktop::build(&config.desktop)?;
    *shared.notes.lock().unwrap() = backends.notes.clone();
    for note in &backends.notes {
        info!("desktop: {note}");
    }
    #[cfg(target_os = "linux")]
    if let (rustle_desktop::Session::GnomeWayland { extension: false }, Some(app)) = (backends.session, app) {
        watch_for_extension(app);
    }

    let (tx, rx) = mpsc::channel::<Event>();

    let overlay: Arc<dyn Overlay> = match (backends.overlay, app) {
        (OverlayChoice::Native(overlay), _) => overlay,
        (OverlayChoice::Webview(host), Some(app)) => {
            Arc::new(crate::overlay::WebviewOverlay::create(app, host)?)
        }
        (OverlayChoice::Webview(_), None) => {
            warn!("headless: no overlay window");
            Arc::new(NullOverlay)
        }
        (OverlayChoice::Off, _) => Arc::new(NullOverlay),
    };
    let overlay = Arc::new(Tracking { inner: overlay, state: shared.state.clone(), app: app.cloned() });

    let mut engine = assemble(shared, &config, overlay, backends.focus, backends.injector, tx.clone(), rx)?;

    // The hotkey is wired before the engine thread starts so nothing is
    // missed; events simply queue until `run` drains them.
    let relay = relay_hotkeys(app, tx.clone());
    let mut hotkeys: Vec<Box<dyn Hotkey>> = Vec::new();
    let mut shortcut = None;
    match backends.hotkey {
        HotkeySource::Builtin(mut builtin) => {
            builtin.start(relay.clone())?;
            hotkeys.push(builtin);
        }
        HotkeySource::AppShortcut(combo) => match app {
            Some(app) => {
                register_shortcut(app, &combo, tx.clone())?;
                shortcut = Some(combo);
            }
            None if cfg!(target_os = "linux") => {
                warn!("headless: no global shortcut here; bind a key to `rustle hotkey toggle` instead")
            }
            None => warn!("headless: no global shortcut on this desktop; use `rustle dictate`"),
        },
        // Nothing to start: the compositor's bindings reach the socket.
        HotkeySource::External(how) => info!("no shortcut of Rustle's own on this desktop: {how}"),
        HotkeySource::Unsupported(why) => {
            warn!("no hotkey: {why}");
            if let Some(app) = app {
                crate::windows::notify(app, "No dictation shortcut", &why);
            }
        }
    }

    #[cfg(target_os = "linux")]
    hotkeys.extend(control_socket(relay));
    #[cfg(not(target_os = "linux"))]
    drop(relay);

    // Before the engine warms the cleanup model up, so a crash from here on
    // does not leave it in video memory.
    let watchdog = crate::watchdog::Watchdog::spawn(&config.cleanup);

    let spawned = std::thread::Builder::new().name("rustle-engine".into()).spawn({
        let app = app.cloned();
        let last_error = shared.last_error.clone();
        move || {
            if let Err(err) = engine.prepare() {
                log::error!("engine failed to prepare: {err:#}");
                *last_error.lock().unwrap() = Some(format!("{err:#}"));
                if let Some(app) = &app {
                    crate::windows::notify(app, "Rustle could not load its models", &format!("{err:#}"));
                    // The thread ends here, so the tray should stop saying "ready".
                    let handle = app.clone();
                    let _ = app.run_on_main_thread(move || crate::tray::sync_toggle(&handle));
                }
                return;
            }
            engine.run();
        }
    });
    let thread = match spawned {
        Ok(thread) => thread,
        Err(err) => {
            if let (Some(app), Some(combo)) = (app, shortcut.as_deref()) {
                unregister_shortcut(app, combo);
            }
            return Err(err.into());
        }
    };

    *shared.engine.lock().unwrap() =
        Some(Running { tx, thread, hotkeys, shortcut, watchdog, _claim: Some(claim) });
    shared.needs_restart.store(false, Ordering::SeqCst);
    Ok(())
}

/// Only one engine may run per user. The background service and the app,
/// or two copies of either, would otherwise both paste every dictation and
/// both hold a model in video memory. The OS drops the lock however the
/// process ends.
fn claim_engine() -> anyhow::Result<File> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("engine.lock");
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => {
            let how = if cfg!(target_os = "linux") {
                " Stop the other one first; for the background service that is `systemctl --user stop rustle`."
            } else {
                " Quit the other one first."
            };
            anyhow::bail!("Rustle is already dictating in another process.{how}")
        }
        // A filesystem without locks should not stop dictation.
        Err(std::fs::TryLockError::Error(err)) => {
            warn!("could not lock {}: {err}; carrying on", path.display());
            Ok(file)
        }
    }
}

/// The control socket, for `rustle hotkey` and for the compositors that can
/// only reach Rustle that way. Failing to listen costs only that.
#[cfg(target_os = "linux")]
fn control_socket(sink: Sender<Event>) -> Option<Box<dyn Hotkey>> {
    let Some(mut socket) = rustle_desktop::linux::control::ControlSocket::new() else {
        warn!("no $XDG_RUNTIME_DIR: no control socket, so `rustle hotkey` cannot reach this Rustle");
        return None;
    };
    match socket.start(sink) {
        Ok(()) => Some(Box::new(socket)),
        Err(err) => {
            warn!("{err}; `rustle hotkey` cannot reach this Rustle");
            None
        }
    }
}

/// Hand hotkey events from a desktop backend on to the engine, telling the
/// windows too: the wizard lights a dot while the shortcut is held.
fn relay_hotkeys(app: Option<&AppHandle>, engine: Sender<Event>) -> Sender<Event> {
    let Some(app) = app.cloned() else { return engine };
    let (tx, rx) = mpsc::channel::<Event>();
    let relay = engine.clone();
    let spawned = std::thread::Builder::new().name("rustle-hotkeys".into()).spawn(move || {
        for event in rx {
            if let Event::Hotkey(hotkey) = &event {
                emit_hotkey(&app, *hotkey);
            }
            if relay.send(event).is_err() {
                break;
            }
        }
    });
    match spawned {
        Ok(_) => tx,
        Err(err) => {
            warn!("could not relay hotkeys to the windows: {err}");
            engine
        }
    }
}

pub fn emit_hotkey(app: &AppHandle, event: HotkeyEvent) {
    let down = match event {
        HotkeyEvent::Down | HotkeyEvent::Pressed => true,
        HotkeyEvent::Up | HotkeyEvent::Released => false,
        HotkeyEvent::Toggle | HotkeyEvent::Cancel => return,
    };
    let _ = app.emit("rustle:hotkey", serde_json::json!({ "down": down }));
}

/// GNOME without Rustle's extension has no hotkey. The extension can turn up
/// later - enabled from the wizard, or simply slower than Rustle at login -
/// so wait for it and then restart onto it.
#[cfg(target_os = "linux")]
fn watch_for_extension(app: &AppHandle) {
    let shared = tauri::Manager::state::<Arc<Shared>>(app).inner().clone();
    if shared.watching_extension.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    let watcher = shared.clone();
    let spawned = std::thread::Builder::new().name("rustle-extension-watch".into()).spawn(move || {
        let shared = watcher;
        loop {
            if rustle_desktop::linux::gnome::wait_for_extension(std::time::Duration::from_secs(60)) {
                info!("Rustle's GNOME Shell extension appeared: restarting onto it");
                if shared.running() {
                    if let Err(err) = restart(&shared, Some(&app)) {
                        warn!("could not restart onto the extension: {err:#}");
                    }
                    crate::tray::sync_toggle(&app);
                }
                break;
            }
            if !shared.running() {
                break;
            }
        }
        shared.watching_extension.store(false, Ordering::SeqCst);
    });
    if spawned.is_err() {
        shared.watching_extension.store(false, Ordering::SeqCst);
    }
}

pub fn stop(shared: &Shared, app: Option<&AppHandle>) {
    let _lifecycle = shared.lifecycle();
    stop_locked(shared, app);
}

/// For the exit path, which runs on the main thread: stop unless a start
/// or stop is under way, since that one may be waiting for this very
/// thread. The process ends either way; the watchdog unloads Ollama then.
pub fn stop_for_exit(shared: &Shared, app: Option<&AppHandle>) -> bool {
    match shared.lifecycle.try_lock() {
        Ok(_lifecycle) => {
            stop_locked(shared, app);
            true
        }
        Err(std::sync::TryLockError::Poisoned(poisoned)) => {
            let _lifecycle = poisoned.into_inner();
            stop_locked(shared, app);
            true
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            warn!("exiting while the engine starts or stops; leaving it to the OS");
            false
        }
    }
}

fn stop_locked(shared: &Shared, app: Option<&AppHandle>) {
    let Some(mut running) = shared.engine.lock().unwrap().take() else { return };
    for hotkey in &mut running.hotkeys {
        hotkey.stop();
    }
    if let (Some(app), Some(combo)) = (app, running.shortcut.as_deref()) {
        unregister_shortcut(app, combo);
    }
    let _ = running.tx.send(Event::Shutdown);
    let _ = running.thread.join();
    if let Some(watchdog) = running.watchdog.take() {
        watchdog.stand_down();
    }
    *shared.state.lock().unwrap() = State::Hidden;
    if let Some(app) = app {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || crate::tray::reflect_state(&handle, State::Hidden));
    }
}

/// Dictation switched off: stop the engine, which unloads a local cleanup
/// model, and give back the GPU memory the recogniser holds, so being off
/// costs nothing. Switching on loads it again, in a second or two. A restart
/// for a settings change goes through [`restart`] instead and keeps it.
pub fn turn_off(shared: &Shared, app: Option<&AppHandle>) {
    let _lifecycle = shared.lifecycle();
    stop_locked(shared, app);
    let transcriber = shared.transcriber.lock().unwrap().as_ref().map(|(_, t)| t.clone());
    if let Some(transcriber) = transcriber {
        transcriber.unload();
    }
    rustle_stt::free_gpu_context();
}

/// Give the GPU back before the process ends: the recogniser's sessions,
/// then ONNX Runtime with its GPU context. After [`stop`], which unloads a
/// local cleanup model from Ollama. Nothing can be recognised afterwards.
pub fn release_gpu(shared: &Shared) {
    let transcriber = shared.transcriber.lock().unwrap().take();
    if let Some((_, transcriber)) = transcriber {
        transcriber.unload();
    }
    rustle_stt::release_runtime();
}

/// Run `shutdown` when Rustle is told to stop: SIGTERM, SIGINT or SIGHUP
/// (systemd stopping the service, logging out, Ctrl-C), or Ctrl-C and
/// Ctrl-Break in a Windows console. Those then end Rustle the way Quit does
/// and the GPU is handed back. A second one exits at once. Logging off or
/// shutting down Windows reaches Quit's path on its own, through
/// `RunEvent::Exit`; closing a console window ends the process before
/// anything can run, and the Ollama watchdog covers that.
pub fn on_termination(shutdown: impl FnOnce() + Send + 'static) {
    let shutdown = Mutex::new(Some(shutdown));
    let installed = ctrlc::set_handler(move || match shutdown.lock().unwrap().take() {
        Some(shutdown) => {
            info!("told to stop: shutting down");
            shutdown();
        }
        None => {
            warn!("told to stop again: exiting now");
            std::process::exit(130);
        }
    });
    if let Err(err) = installed {
        warn!("could not watch for termination signals: {err}");
    }
}

pub fn restart(shared: &Shared, app: Option<&AppHandle>) -> anyhow::Result<()> {
    let _lifecycle = shared.lifecycle();
    stop_locked(shared, app);
    start_locked(shared, app)
}

/// Register the dictation shortcut with the app's global-shortcut plugin and
/// forward press/release as raw hotkey events.
pub fn register_shortcut(app: &AppHandle, combo: &str, tx: Sender<Event>) -> anyhow::Result<()> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

    let shortcut: Shortcut =
        combo.parse().map_err(|e| anyhow::anyhow!("shortcut {combo:?} not understood: {e}"))?;
    app.global_shortcut()
        .on_shortcut(shortcut, move |app, _shortcut, event| {
            let hotkey = match event.state() {
                ShortcutState::Pressed => HotkeyEvent::Down,
                ShortcutState::Released => HotkeyEvent::Up,
            };
            // The first-run wizard shows a live down/up indicator.
            emit_hotkey(app, hotkey);
            let _ = tx.send(Event::Hotkey(hotkey));
        })
        .map_err(|e| anyhow::anyhow!("could not register {combo:?}: {e} - another app may own it"))?;
    info!("shortcut registered: {combo}");
    Ok(())
}

pub fn unregister_shortcut(app: &AppHandle, combo: &str) {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
    if let Ok(shortcut) = combo.parse::<Shortcut>() {
        let _ = app.global_shortcut().unregister(shortcut);
    }
}
