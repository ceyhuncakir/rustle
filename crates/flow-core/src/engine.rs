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

use log::{debug, info, warn};

use crate::cleanup::Cleaner;
use crate::config::Config;
use crate::dsp;
use crate::history::History;
use crate::hold_or_tap::{Action, HoldOrTap};
use crate::learning::{Learned, Learner};

/// The island pushes ~60 levels/sec; there is no point sending more than the
/// waveform can show.
const LEVEL_INTERVAL: Duration = Duration::from_millis(20);
/// The error state has no auto-hide on the overlay side; the engine clears
/// it after a beat.
const ERROR_HIDE_AFTER: Duration = Duration::from_millis(2500);
/// How long the success state lingers (`TIMING.autoHide` in both islands).
/// The pill fades itself out; the engine follows, so the overlay window and
/// the tray do not go on believing a paste is still under way.
const SUCCESS_HIDE_AFTER: Duration = Duration::from_millis(1400);

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
/// extension does that itself). `Toggle` starts a take when idle and stops
/// it when recording, for bindings that only fire on press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Down,
    Up,
    Pressed,
    Released,
    Toggle,
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
    /// A worker's result. `take` says which take it belongs to, so a result
    /// that arrives after that take was cancelled or superseded is dropped.
    Finished {
        take: u64,
        raw: String,
        text: String,
    },
    Failed {
        take: u64,
        message: String,
    },
    /// The microphone stream broke mid-take (unplugged, taken away).
    MicError(String),
    Hide {
        seq: u64,
    },
    /// A background refresh learned a new profile.
    Learned(Learned),
    /// The stored profile changed under the engine (the user cleared the
    /// history): take it up again from the store.
    ReloadProfile,
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
    /// Numbers the takes. Bumped when one starts and when one is cancelled,
    /// so only the take on screen can paste.
    take: u64,
    context: FocusContext,
    started_at: Instant,
    processing_at: Instant,
    last_level: Instant,
    last_raw: String,
    /// Bumped by every scheduled hide, so only the latest one acts.
    hide_seq: u64,
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
            take: 0,
            context: FocusContext::default(),
            started_at: now,
            processing_at: now,
            last_level: now - LEVEL_INTERVAL,
            last_raw: String::new(),
            hide_seq: 0,
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
            info!("learning on: {} dictations stored", history.count().unwrap_or(0));
            self.reload_profile();
        }
        Ok(())
    }

    /// Hand the cleaner whatever profile the store holds now.
    fn reload_profile(&self) {
        let Some(history) = &self.history else { return };
        let (terms, style) = crate::learning::load_profile(history);
        info!("{} learned terms in use", terms.len());
        self.cleaner.set_profile(terms, style);
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
            Event::Hotkey(HotkeyEvent::Toggle) => self.on_toggle(),
            Event::Hotkey(HotkeyEvent::Cancel) => self.on_cancel(),
            Event::Level(level) => self.on_level(level),
            Event::Finished { take, raw, text } => {
                if self.is_current(take) {
                    self.finish(raw, text);
                }
            }
            Event::Failed { take, message } => {
                if self.is_current(take) {
                    self.fail(&message);
                }
            }
            Event::MicError(message) => self.on_mic_error(&message),
            Event::Hide { seq } => {
                if seq == self.hide_seq && !self.busy && !self.recorder.recording() {
                    self.overlay.set_state(State::Hidden);
                }
            }
            Event::Learned(learned) => self.on_learned(learned),
            Event::ReloadProfile => self.reload_profile(),
            Event::Shutdown => return false,
        }
        true
    }

    /// Whether a worker's result still belongs to the take in progress. After
    /// a cancel, or once a newer take has started, pasting it would put old
    /// words into whatever the user has focused now.
    fn is_current(&self, take: u64) -> bool {
        let current = self.busy && take == self.take;
        if !current {
            debug!("dropping the result of take {take}, which was cancelled or superseded");
        }
        current
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
        if self.recorder.recording() {
            return;
        }
        if self.busy {
            // The press is dropped, so HoldOrTap must not go on believing it
            // started something: a tap would stay latched, and the next tap
            // would be spent stopping a recording that never began.
            self.hold.reset();
            // The GNOME extension latched the same tap on its side. Saying
            // the state again tells it that nothing started.
            self.overlay.set_state(State::Thinking);
            return;
        }
        self.take += 1;

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
        self.spawn_process(audio, self.take);
    }

    /// A toggle can arrive between raw presses of the real shortcut (a
    /// script next to a portal key), so HoldOrTap is kept in step with it:
    /// after a toggle starts a take, the next press stops it, as after a tap.
    fn on_toggle(&mut self) {
        if self.recorder.recording() {
            self.hold.reset();
            self.on_released();
            return;
        }
        self.on_pressed("toggle");
        if self.recorder.recording() {
            self.hold.latch();
        }
    }

    fn on_cancel(&mut self) {
        if self.recorder.recording() {
            self.recorder.stop();
        }
        self.hold.reset();
        // Whatever the worker is still doing for this take is now unwanted.
        self.take += 1;
        self.busy = false;
        self.overlay.set_state(State::Hidden);
    }

    /// Only a take still recording is lost. Once released, the stream is
    /// closed and the audio already in hand, so a late report from it is no
    /// reason to throw that take away.
    fn on_mic_error(&mut self, message: &str) {
        if !self.recorder.recording() {
            debug!("microphone error outside a take: {message}");
            return;
        }
        self.recorder.stop();
        self.hold.reset();
        let message: String = message.chars().take(80).collect();
        self.fail(&format!("Microphone: {message}"));
    }

    // -- worker thread ---------------------------------------------------------

    fn spawn_process(&self, audio: Vec<f32>, take: u64) {
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
                    Ok(Ok(Outcome::Text { raw, text })) => Event::Finished { take, raw, text },
                    Ok(Ok(Outcome::NoSpeech)) => Event::Failed { take, message: "No speech detected".into() },
                    Ok(Err(err)) => {
                        // A crash here must not kill the engine.
                        log::error!("dictation failed: {err:#}");
                        // Cut by characters: a byte cut can land inside a
                        // multi-byte one and panic, out here where nothing
                        // catches it and the engine would wait forever.
                        let message = err.to_string().chars().take(80).collect();
                        Event::Failed { take, message }
                    }
                    Err(_) => Event::Failed { take, message: "dictation crashed".into() },
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

        self.hide_after(SUCCESS_HIDE_AFTER);
        self.remember(text);
    }

    fn fail(&mut self, message: &str) {
        self.busy = false;
        warn!("failed: {message}");
        self.overlay.set_text(message);
        self.overlay.set_state(State::Error);
        self.hide_after(ERROR_HIDE_AFTER);
    }

    /// Hide the island after `delay`, unless something newer is on it by then.
    fn hide_after(&mut self, delay: Duration) {
        self.hide_seq += 1;
        let seq = self.hide_seq;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = tx.send(Event::Hide { seq });
        });
    }

    /// A refresh that read the history before the user cleared it must not
    /// bring the forgotten profile back.
    fn on_learned(&self, learned: Learned) {
        let Some(history) = &self.history else { return };
        match history.generation() {
            Ok(current) if current == learned.generation => {
                self.cleaner.set_profile(learned.terms, learned.style)
            }
            Ok(_) => debug!("dropping a profile learned from history that has since been cleared"),
            Err(err) => warn!("could not check the history before using the new profile: {err}"),
        }
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
        let tx = self.tx.clone();

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
            // The engine thread hands the result to the cleaner, after
            // checking the history was not cleared since.
            match learner.refresh_unless_cleared(&history, cfg.max_terms) {
                Ok(Some(learned)) => {
                    if !learned.terms.is_empty() || !learned.style.is_empty() {
                        let _ = tx.send(Event::Learned(learned));
                    }
                }
                Ok(None) => {}
                Err(err) => warn!("profile refresh failed: {err}"),
            }
            learning_now.store(false, Ordering::SeqCst);
        });
    }

    /// Record for a fixed time and insert. Used by `flow dictate` to test the
    /// whole path without a working hotkey. Runs synchronously.
    pub fn dictate_once(&mut self, seconds: f32) -> anyhow::Result<String> {
        // Checked first: from_secs_f32 panics on a negative or non-finite value.
        let length = Duration::try_from_secs_f32(seconds)
            .map_err(|_| anyhow::anyhow!("cannot record for {seconds} seconds"))?;
        self.on_pressed("manual");
        std::thread::sleep(length);

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

    // What was said goes to the debug log only: the default level ends up in
    // the system journal, and dictation is often private.
    let raw = transcriber.transcribe(&audio, audio_cfg.sample_rate)?;
    debug!("raw: {raw:?}");
    if raw.is_empty() {
        return Ok(Outcome::NoSpeech);
    }

    let text = cleaner.clean(&raw, context);
    debug!("clean: {text:?}");
    info!("transcribed {} characters, {} after cleanup", raw.chars().count(), text.chars().count());
    Ok(Outcome::Text { raw, text })
}

#[cfg(test)]
mod test_support {
    //! Fakes for every trait the engine drives.
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Condvar, Mutex};

    #[derive(Default)]
    pub struct FakeOverlay {
        pub states: Mutex<Vec<State>>,
        pub texts: Mutex<Vec<String>>,
        pub levels: Mutex<Vec<f32>>,
    }
    impl Overlay for FakeOverlay {
        fn set_state(&self, state: State) {
            self.states.lock().unwrap().push(state);
        }
        fn set_text(&self, text: &str) {
            self.texts.lock().unwrap().push(text.to_string());
        }
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

    /// Holds workers inside `transcribe` until the test lets them through,
    /// so a test can act while a take is still "thinking".
    #[derive(Default)]
    pub struct Gate {
        permits: Mutex<u32>,
        opened: Condvar,
    }
    impl Gate {
        pub fn open(&self, permits: u32) {
            *self.permits.lock().unwrap() += permits;
            self.opened.notify_all();
        }
        fn pass(&self) {
            // Bounded, so a broken test fails instead of hanging.
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut permits = self.permits.lock().unwrap();
            while *permits == 0 && Instant::now() < deadline {
                permits = self.opened.wait_timeout(permits, Duration::from_millis(20)).unwrap().0;
            }
            *permits = permits.saturating_sub(1);
        }
    }

    pub struct FakeTranscriber {
        pub text: String,
        /// Said by successive calls, ahead of `text`.
        pub script: Mutex<VecDeque<String>>,
        /// Every call fails with this.
        pub error: Option<String>,
        pub gate: Option<Arc<Gate>>,
        pub calls: Mutex<Vec<usize>>,
    }
    impl FakeTranscriber {
        pub fn saying(text: &str) -> Self {
            Self {
                text: text.into(),
                script: Mutex::default(),
                error: None,
                gate: None,
                calls: Mutex::new(vec![]),
            }
        }
        pub fn gated(text: &str, gate: &Arc<Gate>) -> Self {
            Self { gate: Some(Arc::clone(gate)), ..Self::saying(text) }
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
            let text = self.script.lock().unwrap().pop_front().unwrap_or_else(|| self.text.clone());
            if let Some(gate) = &self.gate {
                gate.pass();
            }
            if let Some(error) = &self.error {
                anyhow::bail!("{error}");
            }
            Ok(text)
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

    /// Pump events until a worker's result has been handled, whether or not
    /// the engine acted on it.
    fn next_result(engine: &mut Engine) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(event) = engine.rx.recv_timeout(Duration::from_millis(50)) {
                let result = matches!(event, Event::Finished { .. } | Event::Failed { .. });
                engine.handle(event);
                if result {
                    return;
                }
            }
        }
        panic!("the worker never reported back");
    }

    fn wait_for(done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Longer than HoldOrTap's debounce, so the next raw press counts.
    fn past_debounce() {
        std::thread::sleep(Duration::from_millis(200));
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
        t.error = Some("boom".into());
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

    #[test]
    fn a_long_non_ascii_error_is_reported_not_fatal() {
        // Byte 80 of this falls inside a two-byte character. Cutting there
        // used to panic the worker and leave the engine busy for good.
        let message = format!("x{}", "ü".repeat(60));
        let mut t = FakeTranscriber::saying("x");
        t.error = Some(message.clone());
        let mut r = rig(2.0, t);
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        next_result(&mut r.engine);
        assert!(!r.engine.busy);
        assert_eq!(r.overlay.states.lock().unwrap().last(), Some(&State::Error));
        let shown = r.overlay.texts.lock().unwrap().last().cloned().unwrap();
        assert_eq!(shown, message.chars().take(80).collect::<String>());
    }

    #[test]
    fn cancel_while_thinking_does_not_paste() {
        let gate = Arc::new(Gate::default());
        let mut r = rig(2.0, FakeTranscriber::gated("hello", &gate));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Cancel));
        gate.open(1);
        next_result(&mut r.engine);
        assert!(r.injector.0.lock().unwrap().is_empty());
        assert_eq!(r.overlay.states.lock().unwrap().last(), Some(&State::Hidden));
    }

    #[test]
    fn a_cancelled_take_cannot_paste_into_the_next_one() {
        let gate = Arc::new(Gate::default());
        let t = FakeTranscriber::gated("new words", &gate);
        t.script.lock().unwrap().push_back("old words".into());
        let mut r = rig(2.0, t);
        let transcriber = r.transcriber.clone();

        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        // The first worker has its words before the second take begins.
        wait_for(|| transcriber.calls.lock().unwrap().len() == 1);
        r.engine.handle(Event::Hotkey(HotkeyEvent::Cancel));

        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        wait_for(|| transcriber.calls.lock().unwrap().len() == 2);
        // Both finish, in whichever order; only the live take may paste.
        gate.open(2);
        next_result(&mut r.engine);
        next_result(&mut r.engine);

        assert_eq!(*r.injector.0.lock().unwrap(), vec!["new words".to_string()]);
        assert!(!r.engine.busy);
    }

    #[test]
    fn a_tap_while_busy_does_not_leave_the_shortcut_latched() {
        let gate = Arc::new(Gate::default());
        let mut r = rig(2.0, FakeTranscriber::gated("hi", &gate));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        // A tap while the take is still being transcribed is dropped...
        r.engine.handle(Event::Hotkey(HotkeyEvent::Down));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Up));
        assert_eq!(r.starts.load(Ordering::SeqCst), 1);
        gate.open(1);
        next_result(&mut r.engine);
        // ...so the next tap starts a take, rather than stopping one that
        // never began.
        past_debounce();
        r.engine.handle(Event::Hotkey(HotkeyEvent::Down));
        assert_eq!(r.starts.load(Ordering::SeqCst), 2);
        assert!(r.engine.recorder.recording());
    }

    #[test]
    fn a_press_dropped_while_busy_says_the_state_again() {
        // The GNOME extension latches a tap before the engine sees it; the
        // repeated state is what tells it the press started nothing.
        let gate = Arc::new(Gate::default());
        let mut r = rig(2.0, FakeTranscriber::gated("hi", &gate));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        assert_eq!(r.starts.load(Ordering::SeqCst), 1);
        let states = r.overlay.states.lock().unwrap().clone();
        assert_eq!(states, vec![State::Listening, State::Thinking, State::Thinking]);
        gate.open(1);
        next_result(&mut r.engine);
    }

    #[test]
    fn a_successful_take_hides_the_island_after_the_success_state() {
        let mut r = rig(2.0, FakeTranscriber::saying("hello"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        settle(&mut r.engine);
        let started = Instant::now();
        while r.overlay.states.lock().unwrap().last() != Some(&State::Hidden) {
            assert!(started.elapsed() < Duration::from_secs(5), "the island was never hidden");
            if let Ok(event) = r.engine.rx.recv_timeout(Duration::from_millis(50)) {
                r.engine.handle(event);
            }
        }
        assert!(started.elapsed() >= SUCCESS_HIDE_AFTER - Duration::from_millis(100));
        let states = r.overlay.states.lock().unwrap().clone();
        assert_eq!(states, vec![State::Listening, State::Thinking, State::Inserting, State::Hidden]);
    }

    #[test]
    fn a_new_take_is_not_hidden_by_the_last_ones_timer() {
        let mut r = rig(2.0, FakeTranscriber::saying("hello"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        settle(&mut r.engine);
        // The next take is recording when the first one's hide comes due.
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        let deadline = Instant::now() + SUCCESS_HIDE_AFTER + Duration::from_millis(300);
        while Instant::now() < deadline {
            if let Ok(event) = r.engine.rx.recv_timeout(Duration::from_millis(50)) {
                r.engine.handle(event);
            }
        }
        assert_eq!(r.overlay.states.lock().unwrap().last(), Some(&State::Listening));
    }

    #[test]
    fn a_microphone_error_mid_take_stops_and_says_so() {
        let mut r = rig(2.0, FakeTranscriber::saying("x"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Down));
        r.engine.handle(Event::MicError("device unplugged".into()));
        assert!(!r.engine.recorder.recording());
        assert_eq!(r.stops.load(Ordering::SeqCst), 1);
        assert!(!r.engine.busy);
        assert_eq!(r.overlay.states.lock().unwrap().last(), Some(&State::Error));
        let shown = r.overlay.texts.lock().unwrap().last().cloned();
        assert_eq!(shown.as_deref(), Some("Microphone: device unplugged"));
        assert!(r.transcriber.calls.lock().unwrap().is_empty());
        // The shortcut is not left latched: the next press starts afresh.
        past_debounce();
        r.engine.handle(Event::Hotkey(HotkeyEvent::Down));
        assert_eq!(r.starts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_microphone_error_after_release_leaves_the_take_alone() {
        let gate = Arc::new(Gate::default());
        let mut r = rig(2.0, FakeTranscriber::gated("hello", &gate));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Pressed));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Released));
        // The stream is already closed and the take is in hand.
        r.engine.handle(Event::MicError("late".into()));
        assert!(r.engine.busy);
        gate.open(1);
        next_result(&mut r.engine);
        assert_eq!(*r.injector.0.lock().unwrap(), vec!["hello".to_string()]);
    }

    #[test]
    fn a_microphone_error_while_idle_is_ignored() {
        let mut r = rig(2.0, FakeTranscriber::saying("x"));
        r.engine.handle(Event::MicError("stray".into()));
        assert!(r.overlay.states.lock().unwrap().is_empty());
    }

    #[test]
    fn toggle_starts_when_idle_and_stops_when_recording() {
        let mut r = rig(2.0, FakeTranscriber::saying("toggled"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Toggle));
        assert!(r.engine.recorder.recording());
        r.engine.handle(Event::Hotkey(HotkeyEvent::Toggle));
        assert!(!r.engine.recorder.recording());
        settle(&mut r.engine);
        assert_eq!(*r.injector.0.lock().unwrap(), vec!["toggled".to_string()]);
    }

    #[test]
    fn a_press_after_a_toggle_stops_the_take() {
        let mut r = rig(2.0, FakeTranscriber::saying("x"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Toggle));
        // The raw shortcut must not need two presses to stop it.
        r.engine.handle(Event::Hotkey(HotkeyEvent::Down));
        assert!(!r.engine.recorder.recording());
        assert_eq!(r.stops.load(Ordering::SeqCst), 1);
        // Its release does nothing, and the next press starts afresh.
        r.engine.handle(Event::Hotkey(HotkeyEvent::Up));
        settle(&mut r.engine);
        past_debounce();
        r.engine.handle(Event::Hotkey(HotkeyEvent::Down));
        assert_eq!(r.starts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_toggle_after_a_tap_stops_the_take_and_the_next_press_starts() {
        let mut r = rig(2.0, FakeTranscriber::saying("x"));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Down));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Up));
        assert!(r.engine.recorder.recording(), "a tap latches");
        r.engine.handle(Event::Hotkey(HotkeyEvent::Toggle));
        assert!(!r.engine.recorder.recording());
        settle(&mut r.engine);
        past_debounce();
        r.engine.handle(Event::Hotkey(HotkeyEvent::Down));
        assert_eq!(r.starts.load(Ordering::SeqCst), 2);
        assert!(r.engine.recorder.recording());
    }

    #[test]
    fn a_toggle_while_busy_is_dropped_without_latching() {
        let gate = Arc::new(Gate::default());
        let mut r = rig(2.0, FakeTranscriber::gated("hi", &gate));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Toggle));
        r.engine.handle(Event::Hotkey(HotkeyEvent::Toggle));
        // Still transcribing: this one is dropped.
        r.engine.handle(Event::Hotkey(HotkeyEvent::Toggle));
        assert_eq!(r.starts.load(Ordering::SeqCst), 1);
        gate.open(1);
        next_result(&mut r.engine);
        r.engine.handle(Event::Hotkey(HotkeyEvent::Down));
        assert_eq!(r.starts.load(Ordering::SeqCst), 2, "the next press starts a take");
    }

    #[test]
    fn dictate_once_refuses_a_nonsense_length() {
        let mut r = rig(2.0, FakeTranscriber::saying("x"));
        for seconds in [-1.0, f32::NAN, f32::INFINITY] {
            assert!(r.engine.dictate_once(seconds).is_err(), "{seconds}");
        }
        assert_eq!(r.starts.load(Ordering::SeqCst), 0);
    }
}
