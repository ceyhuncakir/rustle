//! Builds the engine from the crates and runs it on its own thread.
//!
//! Works with or without a Tauri `AppHandle`: the headless mode on GNOME has
//! no windows at all, so anything that needs the app (the webview overlay,
//! the global-shortcut plugin) is optional here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use flow_core::backends;
use flow_core::cleanup::build_cleaner;
use flow_core::config::{data_dir, Config};
use flow_core::engine::{Deps, Engine, Event, Focus, Hotkey, HotkeyEvent, Injector, Overlay, State};
use flow_core::history::History;
use flow_core::learning::Learner;
use flow_desktop::{Backends, HotkeySource, OverlayChoice};
use log::{info, warn};
use tauri::AppHandle;

/// Everything the commands, the tray and the CLI share.
pub struct Shared {
    pub config: Mutex<Config>,
    pub engine: Mutex<Option<Running>>,
    pub state: Arc<Mutex<State>>,
    pub notes: Mutex<Vec<String>>,
    /// The loaded recogniser and the provider it was built for.
    pub transcriber: Mutex<Option<(String, Arc<flow_stt::Parakeet>)>>,
    /// Set by settings changes that only take effect after a restart.
    pub needs_restart: AtomicBool,
    /// Raised by `cancel_download`; the downloader polls it.
    pub download_cancel: Arc<AtomicBool>,
}

pub struct Running {
    pub tx: Sender<Event>,
    pub thread: JoinHandle<()>,
    pub hotkey: Option<Box<dyn Hotkey>>,
    pub shortcut: Option<String>,
    /// Unloads a local cleanup model if Flow dies before the engine can.
    pub watchdog: Option<crate::watchdog::Watchdog>,
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

    pub fn running(&self) -> bool {
        self.engine.lock().unwrap().is_some()
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
pub fn recorder(config: &Config, tx: Sender<Event>) -> flow_audio::CpalRecorder {
    let device = Some(config.audio.device.clone()).filter(|d| !d.is_empty());
    flow_audio::CpalRecorder::new(device, config.audio.sample_rate, tx)
}

/// The transcriber is shared with the wizard and the CLI, and loading it is
/// slow, so it is built once and reused across engine restarts unless the
/// model or compute setting changed.
pub fn transcriber(shared: &Shared, config: &Config) -> Arc<flow_stt::Parakeet> {
    let mut slot = shared.transcriber.lock().unwrap();
    if let Some((provider, existing)) = slot.as_ref() {
        if existing.id() == config.stt.model && *provider == config.stt.provider {
            return existing.clone();
        }
    }
    let fresh = Arc::new(flow_stt::Parakeet::new(&config.stt.model, &config.stt.provider));
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
    let (history, learner) = if config.learning.enabled {
        let history = Arc::new(History::open_default()?);
        let learner = (config.cleanup.enabled && config.cleanup.backend != "none").then(|| {
            let backend: Arc<dyn backends::Backend> = Arc::from(backends::build_backend(&config.cleanup));
            Arc::new(Learner::new(backend))
        });
        (Some(history), learner)
    } else {
        (None, None)
    };

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
    if shared.running() {
        return Ok(());
    }
    shared.reload_config();
    let config = shared.config();

    let backends: Backends = flow_desktop::build(&config.desktop)?;
    *shared.notes.lock().unwrap() = backends.notes.clone();
    for note in &backends.notes {
        info!("desktop: {note}");
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
    let mut hotkey = None;
    let mut shortcut = None;
    match backends.hotkey {
        HotkeySource::Builtin(mut builtin) => {
            builtin.start(tx.clone())?;
            hotkey = Some(builtin);
        }
        HotkeySource::AppShortcut(combo) => match app {
            Some(app) => {
                register_shortcut(app, &combo, tx.clone())?;
                shortcut = Some(combo);
            }
            None => warn!("headless: no global shortcut on this desktop; use `flow dictate`"),
        },
        HotkeySource::Unsupported(why) => {
            warn!("no hotkey: {why}");
            if let Some(app) = app {
                crate::windows::notify(app, "No dictation shortcut", &why);
            }
        }
    }

    // Before the engine warms the cleanup model up, so a crash from here on
    // does not leave it in video memory.
    let watchdog = crate::watchdog::Watchdog::spawn(&config.cleanup);

    let thread = std::thread::Builder::new().name("flow-engine".into()).spawn({
        let app = app.cloned();
        move || {
            if let Err(err) = engine.prepare() {
                log::error!("engine failed to prepare: {err:#}");
                if let Some(app) = &app {
                    crate::windows::notify(app, "Flow could not load its models", &format!("{err:#}"));
                }
                return;
            }
            engine.run();
        }
    })?;

    *shared.engine.lock().unwrap() = Some(Running { tx, thread, hotkey, shortcut, watchdog });
    shared.needs_restart.store(false, Ordering::SeqCst);
    Ok(())
}

pub fn stop(shared: &Shared, app: Option<&AppHandle>) {
    let Some(mut running) = shared.engine.lock().unwrap().take() else { return };
    if let Some(hotkey) = running.hotkey.as_mut() {
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
        crate::tray::reflect_state(app, State::Hidden);
    }
}

/// Dictation switched off: stop the engine, which unloads a local cleanup
/// model, and give back the GPU memory the recogniser holds, so being off
/// costs nothing. Switching on loads it again, in a second or two. A restart
/// for a settings change goes through [`restart`] instead and keeps it.
pub fn turn_off(shared: &Shared, app: Option<&AppHandle>) {
    stop(shared, app);
    let transcriber = shared.transcriber.lock().unwrap().as_ref().map(|(_, t)| t.clone());
    if let Some(transcriber) = transcriber {
        transcriber.unload();
    }
    flow_stt::free_gpu_context();
}

/// Give the GPU back before the process ends: the recogniser's sessions,
/// then ONNX Runtime with its GPU context. After [`stop`], which unloads a
/// local cleanup model from Ollama. Nothing can be recognised afterwards.
pub fn release_gpu(shared: &Shared) {
    let transcriber = shared.transcriber.lock().unwrap().take();
    if let Some((_, transcriber)) = transcriber {
        transcriber.unload();
    }
    flow_stt::release_runtime();
}

/// Run `shutdown` when Flow is told to stop: SIGTERM, SIGINT or SIGHUP
/// (systemd stopping the service, logging out, Ctrl-C), or Ctrl-C and
/// Ctrl-Break in a Windows console. Those then end Flow the way Quit does
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
    stop(shared, app);
    start(shared, app)
}

/// Register the dictation shortcut with the app's global-shortcut plugin and
/// forward press/release as raw hotkey events.
pub fn register_shortcut(app: &AppHandle, combo: &str, tx: Sender<Event>) -> anyhow::Result<()> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

    let shortcut: Shortcut =
        combo.parse().map_err(|e| anyhow::anyhow!("shortcut {combo:?} not understood: {e}"))?;
    app.global_shortcut()
        .on_shortcut(shortcut, move |app, _shortcut, event| {
            let down = matches!(event.state(), ShortcutState::Pressed);
            // The first-run wizard shows a live down/up indicator.
            let _ = tauri::Emitter::emit(app, "flow:hotkey", serde_json::json!({ "down": down }));
            let _ = tx.send(Event::Hotkey(if down { HotkeyEvent::Down } else { HotkeyEvent::Up }));
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
