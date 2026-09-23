//! The dictation state machine: hotkey in, pasted text out.
//!
//! A port of the Python daemon. It owns no pixels and no devices; it drives
//! them through the traits below, which the platform crates implement. All
//! handling happens on one thread fed by a channel, and slow work (recognition,
//! cleanup, learning) runs on worker threads that report back through the
//! same channel.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{info, warn};

use crate::cleanup::Cleaner;
use crate::config::Config;
use crate::dsp;
use crate::history::History;
use crate::hold_or_tap::{Action, HoldOrTap};
use crate::learning::Learner;

/// The island pushes ~60 levels/sec; there is no point sending more than the
/// waveform can show.
const LEVEL_INTERVAL: Duration = Duration::from_millis(20);
/// The error state has no auto-hide on the overlay side; the engine clears
/// it after a beat.
const ERROR_HIDE_AFTER: Duration = Duration::from_millis(2500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Hidden,
    Idle,
    Listening,
    Thinking,
    Inserting,
    Error,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Hidden => "hidden",
            State::Idle => "idle",
            State::Listening => "listening",
            State::Thinking => "thinking",
            State::Inserting => "inserting",
            State::Error => "error",
        }
    }
}

/// What the user is dictating into, captured at the moment the hotkey is
/// pressed - that is when their target app is focused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FocusContext {
    pub app: String,
    pub title: String,
    pub role: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Failed(String),
}

/// The floating island. Every call is fire-and-forget.
pub trait Overlay: Send + Sync {
    fn set_state(&self, state: State);
    fn set_text(&self, text: &str);
    fn push_level(&self, level: f32);
}

pub trait Focus: Send + Sync {
    fn context(&self) -> Result<FocusContext, DesktopError>;
}

pub trait Injector: Send + Sync {
    fn insert(&self, text: &str) -> Result<(), DesktopError>;
}

/// Raw `Down`/`Up` come from platforms that report key state and go through
/// [`HoldOrTap`]; `Pressed`/`Released` are already resolved (the GNOME
/// extension does that itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Down,
    Up,
    Pressed,
    Released,
    Cancel,
}

pub trait Hotkey: Send {
    fn start(&mut self, sink: Sender<Event>) -> Result<(), DesktopError>;
    fn stop(&mut self);
}

/// Records until stopped. Push-to-talk defines the boundaries, so there is
/// no VAD in the capture path. Levels are reported through the sender the
/// recorder was built with.
pub trait Recorder: Send {
    fn start(&mut self) -> anyhow::Result<()>;
    /// Stop and return the whole take as 16 kHz mono f32.
    fn stop(&mut self) -> Vec<f32>;
    fn recording(&self) -> bool;
}

pub trait Transcriber: Send + Sync {
    fn load(&self) -> anyhow::Result<()>;
    fn loaded(&self) -> bool;
    fn transcribe(&self, audio: &[f32], sample_rate: u32) -> anyhow::Result<String>;
}

#[derive(Debug)]
pub enum Event {
    Hotkey(HotkeyEvent),
    Level(f32),
    Finished { raw: String, text: String },
    Failed { message: String },
    HideError { seq: u64 },
    Shutdown,
}

pub struct Deps {
    pub overlay: Arc<dyn Overlay>,
    pub focus: Arc<dyn Focus>,
    pub injector: Arc<dyn Injector>,
    pub recorder: Box<dyn Recorder>,
    pub transcriber: Arc<dyn Transcriber>,
    pub cleaner: Arc<dyn Cleaner>,
    pub history: Option<Arc<History>>,
    pub learner: Option<Arc<Learner>>,
}

pub struct Engine {
    config: Config,
    overlay: Arc<dyn Overlay>,
    focus: Arc<dyn Focus>,
    injector: Arc<dyn Injector>,
    recorder: Box<dyn Recorder>,
    transcriber: Arc<dyn Transcriber>,
    cleaner: Arc<dyn Cleaner>,
    history: Option<Arc<History>>,
    learner: Option<Arc<Learner>>,

    tx: Sender<Event>,
    rx: Receiver<Event>,
    hold: HoldOrTap,

    busy: bool,
    context: FocusContext,
    started_at: Instant,
    processing_at: Instant,
    last_level: Instant,
    last_raw: String,
    error_seq: u64,
    since_refresh: Arc<AtomicU32>,
    learning_now: Arc<AtomicBool>,
}

impl Engine {
    pub fn new(config: Config, deps: Deps) -> Engine {
        let (tx, rx) = mpsc::channel();
        Self::with_channel(config, deps, tx, rx)
    }

    /// Build around a channel created earlier, so collaborators that need
    /// the engine's sender before the engine exists (the recorder's level
    /// meter, the hotkey) can be constructed first.
    pub fn with_channel(config: Config, deps: Deps, tx: Sender<Event>, rx: Receiver<Event>) -> Engine {
        let hold = HoldOrTap::new(config.desktop.push_to_talk);
        let now = Instant::now();
        Engine {
            config,
            overlay: deps.overlay,
            focus: deps.focus,
            injector: deps.injector,
            recorder: deps.recorder,
            transcriber: deps.transcriber,
            cleaner: deps.cleaner,
            history: deps.history,
            learner: deps.learner,
            tx,
            rx,
            hold,
            busy: false,
            context: FocusContext::default(),
            started_at: now,
            processing_at: now,
            last_level: now - LEVEL_INTERVAL,
            last_raw: String::new(),
            error_seq: 0,
            since_refresh: Arc::new(AtomicU32::new(0)),
            learning_now: Arc::new(AtomicBool::new(false)),
        }
    }

    // -- lifecycle ----------------------------------------------------------

    /// Load and warm both models. Done once at startup so the first dictation
    /// is as fast as the hundredth; a cold cleanup model costs seconds and
    /// nobody should meet that mid-sentence.
    pub fn prepare(&mut self) -> anyhow::Result<()> {
        let (ok, why) = self.cleaner.available();
        if !ok {
            warn!("cleanup unavailable ({why}) - transcripts will paste raw");
        }

        info!("loading {} ...", self.config.stt.model);
        self.transcriber.load()?;

        if ok {
            self.cleaner.warm_up();
        }

        if let Some(history) = &self.history {
            let (terms, style) = crate::learning::load_profile(history);
            info!(
                "learning on: {} dictations stored, {} terms learned",
                history.count().unwrap_or(0),
                terms.len()
            );
            self.cleaner.set_profile(terms, style);
        }
        Ok(())
    }

    /// Run until `Event::Shutdown`.
    pub fn run(&mut self) {
        info!("ready - press the dictation hotkey");
        while let Ok(event) = self.rx.recv() {
            if !self.handle(event) {
                break;
            }
        }
        if self.recorder.recording() {
            self.recorder.stop();
        }
        self.overlay.set_state(State::Hidden);
        // Hand the GPU back; a large cleanup model should not outlive us.
        self.cleaner.unload();
    }

    /// Handle one event. Returns false on shutdown.
    fn handle(&mut self, event: Event) -> bool {
        match event {
            Event::Hotkey(HotkeyEvent::Down) => {
                if let Some(action) = self.hold.on_down(Instant::now()) {
                    self.apply(action);
                }
            }
            Event::Hotkey(HotkeyEvent::Up) => {
                if let Some(action) = self.hold.on_up(Instant::now()) {
                    self.apply(action);
                }
            }
            Event::Hotkey(HotkeyEvent::Pressed) => self.on_pressed("toggle"),
            Event::Hotkey(HotkeyEvent::Released) => self.on_released(),
            Event::Hotkey(HotkeyEvent::Cancel) => self.on_cancel(),
            Event::Level(level) => self.on_level(level),
            Event::Finished { raw, text } => self.finish(raw, text),
            Event::Failed { message } => self.fail(&message),
            Event::HideError { seq } => {
                if seq == self.error_seq && !self.busy && !self.recorder.recording() {
                    self.overlay.set_state(State::Hidden);
                }
            }
            Event::Shutdown => return false,
        }
        true
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Start => self.on_pressed("toggle"),
            Action::Stop => self.on_released(),
        }
    }

    // -- level meter, from the recorder's thread via the channel -------------

    fn on_level(&mut self, level: f32) {
        let now = Instant::now();
        if now.duration_since(self.last_level) < LEVEL_INTERVAL {
            return;
        }
        self.last_level = now;
        self.overlay.push_level(level);
    }

    // -- hotkey -----------------------------------------------------------

    fn on_pressed(&mut self, mode: &str) {
        if self.busy || self.recorder.recording() {
            return;
        }

        // Capture context now: this is when the user's target app is focused.
        self.context = match self.focus.context() {
            Ok(ctx) => ctx,
            Err(err) => {
                warn!("could not read focus context: {err}");
                FocusContext::default()
            }
        };

        let app = if self.context.app.is_empty() { "?" } else { self.context.app.as_str() };
        info!("recording ({mode}) into {app}");
        self.started_at = Instant::now();
        self.overlay.set_text("");
        self.overlay.set_state(State::Listening);
        if let Err(err) = self.recorder.start() {
            self.hold.reset();
            self.fail(&format!("Microphone: {err}"));
        }
    }

    fn on_released(&mut self) {
        if !self.recorder.recording() {
            return;
        }

        let audio = self.recorder.stop();
        let seconds = audio.len() as f32 / self.config.audio.sample_rate as f32;
        info!("captured {seconds:.2}s");

        if seconds < self.config.audio.min_seconds {
            self.fail("Too short");
            return;
        }

        self.busy = true;
        self.processing_at = Instant::now();
        self.overlay.set_state(State::Thinking);
        self.spawn_process(audio);
    }

    fn on_cancel(&mut self) {
        if self.recorder.recording() {
            self.recorder.stop();
        }
        self.hold.reset();
        self.busy = false;
        self.overlay.set_state(State::Hidden);
    }

    // -- worker thread ---------------------------------------------------------

    fn spawn_process(&self, audio: Vec<f32>) {
        let tx = self.tx.clone();
        let transcriber = Arc::clone(&self.transcriber);
        let cleaner = Arc::clone(&self.cleaner);
        let context = self.context.clone();
        let audio_cfg = self.config.audio.clone();
        std::thread::Builder::new()
            .name("flow-dictation".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process(&*transcriber, &*cleaner, &context, &audio_cfg, audio)
                }));
                let event = match result {
                    Ok(Ok(Outcome::Text { raw, text })) => Event::Finished { raw, text },
                    Ok(Ok(Outcome::NoSpeech)) => Event::Failed { message: "No speech detected".into() },
                    Ok(Err(err)) => {
                        // A crash here must not kill the engine.
                        log::error!("dictation failed: {err:#}");
                        let mut message = err.to_string();
                        message.truncate(80);
                        Event::Failed { message }
                    }
                    Err(_) => Event::Failed { message: "dictation crashed".into() },
                };
                let _ = tx.send(event);
            })
            .expect("spawn dictation worker");
    }

    // -- back on the engine thread ---------------------------------------------

    fn finish(&mut self, raw: String, text: String) {
        self.busy = false;
        // Learning needs the raw transcript alongside the cleaned one.
        self.last_raw = raw;

        if text.is_empty() {
            self.fail("Nothing to insert");
            return;
        }

        // Report the wait the user actually feels - from letting go of the
        // key to text appearing. Timing from the keypress just measures how
        // long they spoke.
        let now = Instant::now();
        info!(
            "inserted in {:.2}s (spoke {:.1}s)",
            now.duration_since(self.processing_at).as_secs_f32(),
            self.processing_at.duration_since(self.started_at).as_secs_f32(),
        );
        self.overlay.set_text(&text);
        self.overlay.set_state(State::Inserting);
        if let Err(err) = self.injector.insert(&text) {
            warn!("insert failed: {err}");
            self.fail("Could not paste");
            return;
        }

        self.remember(text);
    }

    fn fail(&mut self, message: &str) {
        self.busy = false;
        warn!("failed: {message}");
        self.overlay.set_text(message);
        self.overlay.set_state(State::Error);
        self.error_seq += 1;
        let seq = self.error_seq;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(ERROR_HIDE_AFTER);
            let _ = tx.send(Event::HideError { seq });
        });
    }

    /// Store the dictation and re-mine the profile when enough have piled up.
    /// Runs on a worker so the paste is never waiting on it, and its failures
    /// are swallowed: learning must never break dictation.
    fn remember(&self, text: String) {
        let Some(history) = self.history.clone() else { return };
        let raw = self.last_raw.clone();
        let context = self.context.clone();
        let cfg = self.config.learning.clone();
        let since_refresh = Arc::clone(&self.since_refresh);
        let learning_now = Arc::clone(&self.learning_now);
        let learner = self.learner.clone();
        let cleaner = Arc::clone(&self.cleaner);

        std::thread::spawn(move || {
            if let Err(err) = history.record(&raw, &text, &context) {
                warn!("could not record dictation: {err}");
                return;
            }

            let count = since_refresh.fetch_add(1, Ordering::SeqCst) + 1;
            if learning_now.load(Ordering::SeqCst) || count < cfg.refresh_every {
                return;
            }
            if history.count().unwrap_or(0) < cfg.min_dictations as u64 {
                return;
            }
            let Some(learner) = learner else { return };

            since_refresh.store(0, Ordering::SeqCst);
            learning_now.store(true, Ordering::SeqCst);
            match learner.refresh(&history, cfg.max_terms) {
                Ok((terms, style)) => {
                    if !terms.is_empty() || !style.is_empty() {
                        cleaner.set_profile(terms, style);
                    }
                }
                Err(err) => warn!("profile refresh failed: {err}"),
            }
            learning_now.store(false, Ordering::SeqCst);
        });
    }

    /// Record for a fixed time and insert. Used by `flow dictate` to test the
    /// whole path without a working hotkey. Runs synchronously.
    pub fn dictate_once(&mut self, seconds: f32) -> anyhow::Result<String> {
        self.on_pressed("manual");
        std::thread::sleep(Duration::from_secs_f32(seconds));

        let audio = self.recorder.stop();
        self.overlay.set_state(State::Thinking);

        match process(&*self.transcriber, &*self.cleaner, &self.context, &self.config.audio, audio)? {
            Outcome::NoSpeech => {
                self.fail("No speech detected");
                Ok(String::new())
            }
            Outcome::Text { text, .. } => {
                self.overlay.set_text(&text);
                self.overlay.set_state(State::Inserting);
                self.injector.insert(&text)?;
                Ok(text)
            }
        }
    }
}

enum Outcome {
    NoSpeech,
    Text { raw: String, text: String },
}

fn process(
    transcriber: &dyn Transcriber,
    cleaner: &dyn Cleaner,
    context: &FocusContext,
    audio_cfg: &crate::config::AudioConfig,
    mut audio: Vec<f32>,
) -> anyhow::Result<Outcome> {
    if audio_cfg.trim_silence {
        audio = dsp::trim_silence(&audio, audio_cfg.sample_rate, audio_cfg.silence_rms);
    }

    let raw = transcriber.transcribe(&audio, audio_cfg.sample_rate)?;
    info!("raw: {raw:?}");
    if raw.is_empty() {
        return Ok(Outcome::NoSpeech);
    }

    let text = cleaner.clean(&raw, context);
    info!("clean: {text:?}");
    Ok(Outcome::Text { raw, text })
}

#[cfg(test)]
mod test_support {
    //! Fakes for every trait the engine drives.
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeOverlay {
        pub states: Mutex<Vec<State>>,
        pub levels: Mutex<Vec<f32>>,
    }
    impl Overlay for FakeOverlay {
        fn set_state(&self, state: State) {
            self.states.lock().unwrap().push(state);
        }
        fn set_text(&self, _text: &str) {}
        fn push_level(&self, level: f32) {
            self.levels.lock().unwrap().push(level);
        }
    }

    pub struct FakeFocus(pub Mutex<FocusContext>);
    impl Focus for FakeFocus {
        fn context(&self) -> Result<FocusContext, DesktopError> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    #[derive(Default)]
    pub struct FakeInjector(pub Mutex<Vec<String>>);
    impl Injector for FakeInjector {
        fn insert(&self, text: &str) -> Result<(), DesktopError> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(())
        }
    }

    pub struct FakeRecorder {
        pub take: Vec<f32>,
        pub recording: bool,
        pub starts: Arc<AtomicU32>,
        pub stops: Arc<AtomicU32>,
    }
    impl FakeRecorder {
        pub fn seconds(seconds: f32) -> Self {
            Self {
                take: vec![0.1; (16000.0 * seconds) as usize],
                recording: false,
                starts: Arc::new(AtomicU32::new(0)),
                stops: Arc::new(AtomicU32::new(0)),
            }
        }
    }
    impl Recorder for FakeRecorder {
        fn start(&mut self) -> anyhow::Result<()> {
            self.recording = true;
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn stop(&mut self) -> Vec<f32> {
            self.recording = false;
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.take.clone()
        }
        fn recording(&self) -> bool {
            self.recording
        }
    }

    pub struct FakeTranscriber {
        pub text: String,
        pub fail: bool,
        pub calls: Mutex<Vec<usize>>,
    }
    impl FakeTranscriber {
        pub fn saying(text: &str) -> Self {
            Self { text: text.into(), fail: false, calls: Mutex::new(vec![]) }
        }
    }
    impl Transcriber for FakeTranscriber {
        fn load(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn loaded(&self) -> bool {
            true
        }
        fn transcribe(&self, audio: &[f32], _sample_rate: u32) -> anyhow::Result<String> {
            self.calls.lock().unwrap().push(audio.len());
            if self.fail {
                anyhow::bail!("boom");
            }
            Ok(self.text.clone())
        }
    }

    /// Echoes the transcript back, recording the context it was given.
    #[derive(Default)]
    pub struct FakeCleaner {
        pub seen: Mutex<Vec<(String, FocusContext)>>,
    }
    impl Cleaner for FakeCleaner {
        fn clean(&self, raw: &str, context: &FocusContext) -> String {
            self.seen.lock().unwrap().push((raw.to_string(), context.clone()));
            raw.trim().to_string()
        }
        fn available(&self) -> (bool, String) {
            (true, "fake".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use std::sync::Mutex;

    struct Rig {
        engine: Engine,
        overlay: Arc<FakeOverlay>,
        injector: Arc<FakeInjector>,
        transcriber: Arc<FakeTranscriber>,
        cleaner: Arc<FakeCleaner>,
        starts: Arc<AtomicU32>,
        stops: Arc<AtomicU32>,
    }

    fn rig(take_seconds: f32, transcriber: FakeTranscriber) -> Rig {
        let overlay = Arc::new(FakeOverlay::default());
        let injector = Arc::new(FakeInjector::default());
        let transcriber = Arc::new(transcriber);
        let cleaner = Arc::new(FakeCleaner::default());
        let recorder = FakeRecorder::seconds(take_seconds);
        let starts = recorder.starts.clone();
        let stops = recorder.stops.clone();
        let focus = Arc::new(FakeFocus(Mutex::new(FocusContext {
            app: "org.gnome.TextEditor".into(),
            title: "notes".into(),
            role: String::new(),
        })));
        let engine = Engine::new(
            Config::default(),
            Deps {
                overlay: overlay.clone(),
                focus,
                injector: injector.clone(),
                recorder: Box::new(recorder),
                transcriber: transcriber.clone(),
                cleaner: cleaner.clone(),
                history: None,
                learner: None,
            },
        );
        Rig { engine, overlay, injector, transcriber, cleaner, starts, stops }
    }

    /// Pump events until the worker has reported back.
    fn settle(engine: &mut Engine) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.busy && Instant::now() < deadline {
            if let Ok(event) = engine.rx.recv_timeout(Duration::from_millis(50)) {
                engine.handle(event);
            }
        }
    }

    #[test]
    fn happy_path_states_and_insert() {
        let mut r = rig(2.0, FakeTranscriber::saying("hello world"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        settle(&mut r.engine);

        let states = r.overlay.states.lock().unwrap().clone();
        assert_eq!(states, vec![State::Listening, State::Thinking, State::Inserting]);
        assert_eq!(r.starts.load(Ordering::SeqCst), 1);
        assert_eq!(*r.injector.0.lock().unwrap(), vec!["hello world".to_string()]);
    }

    #[test]
    fn focus_context_is_captured_at_press_not_at_insert() {
        let mut r = rig(2.0, FakeTranscriber::saying("hi"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        // Focus moves elsewhere before the release.
        let focus =
            Arc::new(FakeFocus(Mutex::new(FocusContext { app: "other".into(), ..Default::default() })));
        r.engine.focus = focus;
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        settle(&mut r.engine);
        let seen = r.cleaner.seen.lock().unwrap();
        assert_eq!(seen[0].1.app, "org.gnome.TextEditor");
    }

    #[test]
    fn too_short_a_take_is_rejected_without_transcribing() {
        let mut r = rig(0.1, FakeTranscriber::saying("x"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        let states = r.overlay.states.lock().unwrap().clone();
        assert_eq!(states.last(), Some(&State::Error));
        assert!(r.transcriber.calls.lock().unwrap().is_empty());
        assert!(!r.engine.busy);
    }

    #[test]
    fn empty_transcript_shows_an_error() {
        let mut r = rig(2.0, FakeTranscriber::saying(""));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        settle(&mut r.engine);
        assert_eq!(r.overlay.states.lock().unwrap().last(), Some(&State::Error));
        assert!(r.injector.0.lock().unwrap().is_empty());
    }

    #[test]
    fn press_while_recording_is_ignored() {
        let mut r = rig(2.0, FakeTranscriber::saying("x"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        assert_eq!(r.starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn release_without_press_does_nothing() {
        let mut r = rig(2.0, FakeTranscriber::saying("x"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        assert!(r.overlay.states.lock().unwrap().is_empty());
    }

    #[test]
    fn cancel_stops_and_hides() {
        let mut r = rig(2.0, FakeTranscriber::saying("x"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Cancel));
        assert_eq!(r.overlay.states.lock().unwrap().last(), Some(&State::Hidden));
        assert_eq!(r.stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn transcriber_failure_does_not_kill_the_engine() {
        let mut t = FakeTranscriber::saying("x");
        t.fail = true;
        let mut r = rig(2.0, t);
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        settle(&mut r.engine);
        assert_eq!(r.overlay.states.lock().unwrap().last(), Some(&State::Error));
        assert!(r.injector.0.lock().unwrap().is_empty());
        assert!(!r.engine.busy);
    }

    #[test]
    fn silence_is_trimmed_before_transcription() {
        let mut r = rig(2.0, FakeTranscriber::saying("x"));
        // Half the take is silence.
        let take = 16000 * 2;
        r.engine.recorder = Box::new(FakeRecorder {
            take: (0..take).map(|i| if i < take / 2 { 0.0 } else { 0.1 }).collect(),
            recording: false,
            starts: Arc::new(AtomicU32::new(0)),
            stops: Arc::new(AtomicU32::new(0)),
        });
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        settle(&mut r.engine);
        let seen = r.transcriber.calls.lock().unwrap();
        assert!(seen[0] < take, "transcriber saw {} of {take} samples", seen[0]);
    }

    #[test]
    fn raw_down_up_goes_through_hold_or_tap() {
        let mut r = rig(2.0, FakeTranscriber::saying("x"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Down));
        assert_eq!(r.starts.load(Ordering::SeqCst), 1);
        // A quick tap latches: the up does not stop it.
        r.engine.handle(Event::Hotkey(HotkeyEvent::Up));
        assert!(r.engine.recorder.recording());
    }

    #[test]
    fn levels_are_throttled() {
        let mut r = rig(2.0, FakeTranscriber::saying("x"));
        for _ in 0..10 {
            r.engine.handle(Event::Level(0.5));
        }
        assert_eq!(r.overlay.levels.lock().unwrap().len(), 1);
    }
}
