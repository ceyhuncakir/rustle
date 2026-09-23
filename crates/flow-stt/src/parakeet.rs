//! The recogniser: three onnxruntime sessions and the glue between them.
//!
//! `nemo128.onnx` (features) and `decoder_joint` always run on the CPU: the
//! first is cheap and the second is called once per 80 ms frame with tiny
//! tensors, where a GPU round trip costs more than the arithmetic. Only the
//! encoder goes to the chosen execution provider, and as in onnx-asr the CPU
//! runs the int8 export while CUDA runs fp32.
//!
//! Whether the GPU is used is decided by [`crate::gpu::detect`] before
//! anything loads (and for CUDA, it also puts CUDA 12 and cuDNN 9 where the
//! provider finds them). `auto` goes to the CPU when it says no, or when the
//! GPU then fails anyway; `gpu` tries regardless and fails with the reason.
//! Both GPU backends run the fp32 export, like CUDA always has.

use std::borrow::Cow;
use std::mem::ManuallyDrop;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::Instant;

use anyhow::{anyhow, bail, Context};
use flow_core::dsp;
use flow_core::engine::Transcriber;
use flow_core::models::{is_downloaded, model_dir, model_files, Precision, NEMO128_FILE, VOCAB_FILE};
use log::{debug, info, warn};
use ort::session::{Session, SessionOutputs};
use ort::value::TensorRef;
use rand::{Rng, SeedableRng};

use crate::decode::{greedy_tdt, Decoded, DecoderState, JointOutput};
use crate::files::{verify_nemo128, NEMO128_SHA256};
use crate::gpu;
use crate::vocab::Vocab;

/// Samples per second the model expects.
const SAMPLE_RATE: u32 = 16_000;

/// The most audio the encoder is given at once: 60 s, the length the video
/// memory check in [`crate::gpu`] was measured with. Its attention spans the
/// whole input, so memory grows with the square of the length (about 1.8 GB
/// per attention tensor at 10 minutes), and a long dictation in toggle mode
/// would otherwise run the GPU out of memory and lose the take. Longer audio
/// is recognised in pieces; anything up to this runs exactly as before.
const MAX_CHUNK: usize = 60 * SAMPLE_RATE as usize;

/// How far before a piece's end to look for a quiet place to cut: the last
/// 10 s of it.
const CUT_SEARCH: usize = 10 * SAMPLE_RATE as usize;

/// The stretch whose energy decides the cut (100 ms), and the step the
/// search moves it by (10 ms).
const CUT_WINDOW: usize = SAMPLE_RATE as usize / 10;
const CUT_STEP: usize = SAMPLE_RATE as usize / 100;

/// Input samples per encoder frame: a 10 ms hop, subsampled 8x.
const SAMPLES_PER_FRAME: usize = 8 * SAMPLE_RATE as usize / 100;

/// What the recogniser ended up running on, and why.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ComputeReport {
    /// The configured provider: `auto`, `cuda` or `cpu`.
    pub requested: String,
    /// `cuda` or `cpu`.
    pub actual: String,
    /// How `actual` came about, including which precision was loaded.
    pub reason: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Transcription {
    /// Trimmed text, `""` when nothing was recognised.
    pub text: String,
    pub tokens: Vec<u32>,
    /// Encoder frame each token was emitted at: 8x subsampled, 80 ms each.
    pub frames: Vec<usize>,
}

/// Log-mel features as the encoder sees them: `[1, mel, frames]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Features {
    pub data: Vec<f32>,
    pub mel: usize,
    pub frames: usize,
    /// `features_lens[0]`: frames that carry signal.
    pub valid: usize,
}

/// Encoder output as `[frames, channels]`; only the first `frames` rows
/// (`encoded_lengths`) carry signal.
struct Encoded {
    data: Vec<f32>,
    channels: usize,
    frames: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Device {
    Cpu,
    Cuda,
    WebGpu,
}

/// The device this build's GPU backend drives, if it has one.
fn gpu_device() -> Option<Device> {
    match gpu::backend() {
        Some("cuda") => Some(Device::Cuda),
        Some("webgpu") => Some(Device::WebGpu),
        _ => None,
    }
}

impl Device {
    fn name(self) -> &'static str {
        match self {
            Device::Cpu => "cpu",
            Device::Cuda => "cuda",
            Device::WebGpu => "webgpu",
        }
    }
}

pub struct Parakeet {
    id: String,
    provider: String,
    /// The sessions, until [`Parakeet::unload`] hands them back.
    loaded: RwLock<Option<Loaded>>,
    /// Held while building, so two callers of `load` do not both build.
    building: Mutex<()>,
    /// Set by [`unload`](Parakeet::unload), cleared by [`load`](Transcriber::load).
    /// While set, recognising does not load the sessions again by itself: a
    /// dictation still finishing when Flow was switched off or told to quit
    /// must not bring the model back.
    parked: AtomicBool,
}

/// Held while a GPU session is built or run, and while the GPU is given
/// back. ONNX Runtime's WebGPU provider shares one device between every
/// session in the process, and that device cannot be used from two threads
/// at once (two sessions built together crashed in Dawn's command encoder);
/// and CUDA's context must not be reset under a session that is using it.
static GPU: Mutex<()> = Mutex::new(());

fn lock_gpu() -> MutexGuard<'static, ()> {
    GPU.lock().unwrap_or_else(|e| e.into_inner())
}

fn gpu_lock(device: Device) -> Option<MutexGuard<'static, ()>> {
    (device != Device::Cpu).then(lock_gpu)
}

struct Loaded {
    preprocessor: Mutex<Session>,
    /// Dropped by hand, under the GPU lock; see `Drop for Loaded`.
    encoder: ManuallyDrop<Mutex<Session>>,
    /// Where the encoder runs; the other two always run on the CPU.
    device: Device,
    decoder: Mutex<Session>,
    vocab: Vocab,
    /// `[layers, 1, hidden]` of each LSTM state.
    state_shape: [usize; 3],
    report: ComputeReport,
    /// Last, so it is dropped after the sessions: [`release_runtime`] and
    /// [`free_gpu_context`] wait for every one of these to be gone.
    _live: Live,
}

impl Drop for Loaded {
    /// A GPU session is torn down under the GPU lock, like it is built and
    /// run: releasing it uses the device, which another recogniser (say, a
    /// new one warming up after a settings change while a dictation on the
    /// old one finishes) may be using at that moment.
    fn drop(&mut self) {
        let _gpu = gpu_lock(self.device);
        // SAFETY: dropped once, here, and never touched again.
        unsafe { ManuallyDrop::drop(&mut self.encoder) };
    }
}

/// Counts the recognisers loaded or being loaded.
static LIVE: AtomicUsize = AtomicUsize::new(0);

struct Live;

impl Live {
    fn new() -> Live {
        LIVE.fetch_add(1, Ordering::SeqCst);
        Live
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Parakeet {
    /// `provider` is `auto`, `cuda` or `cpu`; it is checked at [`load`](Self::load).
    pub fn new(id: &str, provider: &str) -> Parakeet {
        Parakeet {
            id: id.to_string(),
            provider: provider.to_string(),
            loaded: RwLock::new(None),
            building: Mutex::new(()),
            parked: AtomicBool::new(false),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// `None` while nothing is loaded.
    pub fn compute_report(&self) -> Option<ComputeReport> {
        self.read().as_ref().map(|l| l.report.clone())
    }

    /// Hand the sessions back, and with them the memory the encoder holds on
    /// the GPU. Waits for a recognition in progress. Until the next
    /// [`load`](Transcriber::load), recognising fails rather than loading the
    /// model again. Returns whether anything was loaded.
    pub fn unload(&self) -> bool {
        // Parked first: a build finishing from here on sees it and throws
        // its sessions away instead of storing them.
        self.parked.store(true, Ordering::SeqCst);
        let loaded = self.loaded.write().unwrap_or_else(|e| e.into_inner()).take();
        let Some(loaded) = loaded else { return false };
        let device = loaded.device.name();
        drop(loaded);
        info!("unloaded {} from {device}", self.id);
        true
    }

    /// Recognise, keeping the token ids and frame indices.
    pub fn transcribe_detailed(&self, audio: &[f32], sample_rate: u32) -> anyhow::Result<Transcription> {
        if sample_rate == 0 {
            bail!("audio with a sample rate of 0");
        }
        if audio.is_empty() {
            self.ensure_loaded()?;
            return Ok(Transcription::default());
        }
        let started = Instant::now();
        let audio16 = if sample_rate == SAMPLE_RATE {
            Cow::Borrowed(audio)
        } else {
            Cow::Owned(dsp::resample(audio, sample_rate, SAMPLE_RATE))
        };
        let result = self.with_loaded(|loaded| loaded.recognize_long(&audio16))?;
        let elapsed = started.elapsed().as_secs_f64();
        let seconds = audio.len() as f64 / sample_rate as f64;
        info!(
            "transcribed {seconds:.1}s of audio in {elapsed:.2}s ({:.0}x realtime)",
            if elapsed > 0.0 { seconds / elapsed } else { 0.0 }
        );
        Ok(result)
    }

    /// The preprocessor's output for 16 kHz audio, for the parity tests.
    pub fn features(&self, audio: &[f32]) -> anyhow::Result<Features> {
        self.with_loaded(|loaded| loaded.features(audio))
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Option<Loaded>> {
        self.loaded.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Run `f` on the loaded sessions, loading them first if need be. The
    /// read lock keeps [`unload`](Self::unload) waiting until `f` is done.
    fn with_loaded<T>(&self, f: impl FnOnce(&Loaded) -> anyhow::Result<T>) -> anyhow::Result<T> {
        self.ensure_loaded()?;
        let guard = self.read();
        // An unload that slipped in between wins: loading again would undo
        // switching dictation off.
        let loaded = guard.as_ref().ok_or_else(unloaded)?;
        f(loaded)
    }

    /// Build the sessions unless they are loaded, or parked by an unload.
    fn ensure_loaded(&self) -> anyhow::Result<()> {
        if self.read().is_some() {
            return Ok(());
        }
        let _building = self.building.lock().unwrap_or_else(|e| e.into_inner());
        // Whoever held the lock before us may have finished the job.
        if self.read().is_some() {
            return Ok(());
        }
        if self.parked.load(Ordering::SeqCst) {
            return Err(unloaded());
        }
        let started = Instant::now();
        let loaded = self.build()?;
        let mut slot = self.loaded.write().unwrap_or_else(|e| e.into_inner());
        // Checked under the write lock, which `unload` takes after parking:
        // either it sees these sessions and takes them, or they go here.
        if self.parked.load(Ordering::SeqCst) {
            drop(slot);
            drop(loaded);
            return Err(unloaded());
        }
        info!(
            "loaded {} on {} in {:.1}s ({})",
            self.id,
            loaded.report.actual,
            started.elapsed().as_secs_f64(),
            loaded.report.reason
        );
        *slot = Some(loaded);
        Ok(())
    }

    fn build(&self) -> anyhow::Result<Loaded> {
        let provider = self.provider.as_str();
        match provider {
            "cpu" => self.build_on(Device::Cpu, "provider = cpu"),
            p if gpu::insists_on_gpu(p) => {
                let Some(device) = gpu_device() else {
                    bail!(
                        "provider = \"{provider}\" but this build of Flow has no GPU support; \
                         set provider = \"auto\" or \"cpu\""
                    );
                };
                // Asked for explicitly, so tried even when the check says no;
                // the check's reason is the better error if it then fails.
                let gpu = gpu::detect();
                let reason = match &gpu.gpu {
                    Some(card) => format!("provider = {provider}, {card}"),
                    None => format!("provider = {provider}"),
                };
                self.build_on(device, &reason).map_err(|e| {
                    let e = anyhow!("{}", tidy_ort_error(&format!("{e:#}")));
                    match &gpu.problem {
                        Some(problem) => e.context(format!("the GPU cannot be used: {problem}")),
                        None => e,
                    }
                })
            }
            "auto" => {
                let gpu = gpu::detect();
                let reason = match gpu_device().filter(|_| gpu.usable) {
                    Some(device) => match self.build_on(device, &gpu.describe()) {
                        Ok(loaded) => return Ok(loaded),
                        Err(e) => {
                            let why = tidy_ort_error(&format!("{e:#}"));
                            warn!("the GPU could not load the model, using the CPU: {why}");
                            format!("the GPU could not load the model ({why})")
                        }
                    },
                    None => gpu.describe(),
                };
                self.build_on(Device::Cpu, &reason)
            }
            other => bail!("unknown stt provider {other:?}; expected auto, gpu or cpu"),
        }
    }

    fn build_on(&self, device: Device, reason: &str) -> anyhow::Result<Loaded> {
        // Counted from the start, so the GPU is not given back under a build
        // in progress; dropped with the error if the build fails.
        let live = Live::new();
        let dir = model_dir(&self.id);
        let (precision, note) = pick_precision(&self.id, &dir, device)?;
        let files = model_files(precision);
        let file = |prefix: &str| -> anyhow::Result<PathBuf> {
            files
                .iter()
                .find(|f| f.starts_with(prefix) && f.ends_with(".onnx"))
                .map(|f| dir.join(f))
                .with_context(|| format!("no {prefix}*.onnx among the {} files", precision_name(precision)))
        };
        let nemo = dir.join(NEMO128_FILE);
        verify_nemo128(&nemo, NEMO128_SHA256)?;
        let vocab = Vocab::load(&dir.join(VOCAB_FILE))?;

        let started = Instant::now();
        debug!("{}", ort::info());
        let preprocessor = build_session(&nemo, Device::Cpu)?;
        let decoder = build_session(&file("decoder_joint-model")?, Device::Cpu)?;
        let state_shape = state_shape(&decoder)?;
        // Last, so from here on only `Loaded` owns it, and drops it under the
        // GPU lock.
        let encoder = {
            let _gpu = gpu_lock(device);
            build_session(&file("encoder-model")?, device)?
        };
        debug!("sessions built in {:.1}s", started.elapsed().as_secs_f64());

        let loaded = Loaded {
            preprocessor: Mutex::new(preprocessor),
            encoder: ManuallyDrop::new(Mutex::new(encoder)),
            device,
            decoder: Mutex::new(decoder),
            vocab,
            state_shape,
            report: ComputeReport {
                requested: self.provider.clone(),
                actual: device.name().to_string(),
                reason: format!("{reason}; {note}"),
            },
            _live: live,
        };
        loaded.warm_up()?;
        Ok(loaded)
    }
}

impl Transcriber for Parakeet {
    /// Load the sessions, and undo an earlier [`unload`](Parakeet::unload)
    /// (switching dictation on). An unload while this builds wins, and this
    /// then fails.
    fn load(&self) -> anyhow::Result<()> {
        self.parked.store(false, Ordering::SeqCst);
        self.ensure_loaded()
    }

    fn loaded(&self) -> bool {
        self.read().is_some()
    }

    fn transcribe(&self, audio: &[f32], sample_rate: u32) -> anyhow::Result<String> {
        Ok(self.transcribe_detailed(audio, sample_rate)?.text)
    }
}

impl Loaded {
    /// One throwaway recognition: the first run compiles kernels and sizes
    /// arenas, which is better paid at startup than at the first keypress.
    fn warm_up(&self) -> anyhow::Result<()> {
        let started = Instant::now();
        self.recognize(&warm_up_noise()).context("warm-up recognition")?;
        info!("warm-up took {:.2} s", started.elapsed().as_secs_f64());
        Ok(())
    }

    /// [`recognize`](Self::recognize), in pieces of at most [`MAX_CHUNK`]
    /// cut where it is quietest, one after the other, with the tokens joined
    /// and their frames counted from the start of the whole take.
    fn recognize_long(&self, audio: &[f32]) -> anyhow::Result<Transcription> {
        let pieces = chunks(audio);
        if pieces.len() == 1 {
            return self.recognize(audio);
        }
        info!(
            "recognising {:.0} s of audio in {} pieces",
            audio.len() as f64 / SAMPLE_RATE as f64,
            pieces.len()
        );
        let mut whole = Transcription::default();
        for piece in pieces {
            // Rounded, so a token's frame is within one frame of where a
            // single pass would have put it.
            let offset = (piece.start + SAMPLES_PER_FRAME / 2) / SAMPLES_PER_FRAME;
            let part = self.recognize(&audio[piece])?;
            whole.tokens.extend(part.tokens);
            whole.frames.extend(part.frames.into_iter().map(|f| f + offset));
        }
        whole.text = self.vocab.text(&whole.tokens)?.trim().to_string();
        Ok(whole)
    }

    fn recognize(&self, audio: &[f32]) -> anyhow::Result<Transcription> {
        let started = Instant::now();
        let features = self.features(audio)?;
        let t_features = started.elapsed().as_secs_f64();
        if features.valid == 0 || features.frames == 0 {
            return Ok(Transcription::default());
        }
        let encoded = self.encode(&features)?;
        let t_encoder = started.elapsed().as_secs_f64() - t_features;
        let decoded = self.decode(&encoded)?;
        let t_decoder = started.elapsed().as_secs_f64() - t_features - t_encoder;
        debug!(
            "features {t_features:.3}s, encoder {t_encoder:.3}s ({} frames), decoder {t_decoder:.3}s ({} tokens)",
            encoded.frames,
            decoded.tokens.len()
        );
        let text = self.vocab.text(&decoded.tokens)?.trim().to_string();
        Ok(Transcription { text, tokens: decoded.tokens, frames: decoded.frames })
    }

    fn features(&self, audio: &[f32]) -> anyhow::Result<Features> {
        let lens = [audio.len() as i64];
        let mut session = lock(&self.preprocessor, "preprocessor")?;
        let outputs = session
            .run(ort::inputs![
                "waveforms" => TensorRef::from_array_view(([1usize, audio.len()], audio))?,
                "waveforms_lens" => TensorRef::from_array_view(([1usize], &lens[..]))?,
            ])
            .context("running nemo128.onnx")?;
        let (mel, frames, data) = matrix_output(&outputs, "features")?;
        Ok(Features { data: data.to_vec(), mel, frames, valid: length_output(&outputs, "features_lens")? })
    }

    fn encode(&self, features: &Features) -> anyhow::Result<Encoded> {
        let valid_frames = [features.valid as i64];
        let _gpu = gpu_lock(self.device);
        let mut session = lock(&self.encoder, "encoder")?;
        let outputs = session
            .run(ort::inputs![
                "audio_signal" => TensorRef::from_array_view(([1usize, features.mel, features.frames], &features.data[..]))?,
                "length" => TensorRef::from_array_view(([1usize], &valid_frames[..]))?,
            ])
            .context("running the encoder")?;
        let (channels, frames, data) = matrix_output(&outputs, "outputs")?;
        let valid = length_output(&outputs, "encoded_lengths")?;
        // Transposed to `[frames, channels]` so each frame is one contiguous slice.
        let transposed = (0..frames).flat_map(|t| (0..channels).map(move |c| data[c * frames + t])).collect();
        Ok(Encoded { data: transposed, channels, frames: valid.min(frames) })
    }

    fn decode(&self, encoded: &Encoded) -> anyhow::Result<Decoded> {
        let channels = encoded.channels;
        let mut session = lock(&self.decoder, "decoder")?;
        greedy_tdt(
            encoded.frames,
            self.vocab.len(),
            self.vocab.blank(),
            DecoderState::zeros(self.state_shape.iter().product()),
            |t, prev, state| {
                self.joint(&mut session, &encoded.data[t * channels..(t + 1) * channels], prev, state)
            },
        )
    }

    fn joint(
        &self,
        session: &mut Session,
        frame: &[f32],
        prev: usize,
        state: &DecoderState,
    ) -> anyhow::Result<JointOutput> {
        let targets = [prev as i32];
        let target_length = [1i32];
        let outputs = session
            .run(ort::inputs![
                "encoder_outputs" => TensorRef::from_array_view(([1usize, frame.len(), 1], frame))?,
                "targets" => TensorRef::from_array_view(([1usize, 1], &targets[..]))?,
                "target_length" => TensorRef::from_array_view(([1usize], &target_length[..]))?,
                "input_states_1" => TensorRef::from_array_view((self.state_shape, &state.h[..]))?,
                "input_states_2" => TensorRef::from_array_view((self.state_shape, &state.c[..]))?,
            ])
            .context("running decoder_joint")?;
        Ok(JointOutput {
            logits: floats(&outputs, "outputs")?,
            state: DecoderState {
                h: floats(&outputs, "output_states_1")?,
                c: floats(&outputs, "output_states_2")?,
            },
        })
    }
}

fn unloaded() -> anyhow::Error {
    anyhow!("the speech model was unloaded (Flow was switched off or is quitting)")
}

/// Where to split `audio` so no piece is longer than [`MAX_CHUNK`]: each cut
/// is the middle of the quietest 100 ms in the last [`CUT_SEARCH`] before a
/// piece would get too long, so it falls between words where there is a
/// pause. One range, the whole of it, for audio that fits.
fn chunks(audio: &[f32]) -> Vec<Range<usize>> {
    let mut pieces = Vec::new();
    let mut start = 0;
    while audio.len() - start > MAX_CHUNK {
        let end = start + MAX_CHUNK;
        let cut = quietest(&audio[end - CUT_SEARCH..end]) + end - CUT_SEARCH;
        pieces.push(start..cut);
        start = cut;
    }
    pieces.push(start..audio.len());
    pieces
}

/// The middle of the [`CUT_WINDOW`] with the least energy in `audio`, the
/// latest one on a tie (so digital silence still leaves long pieces).
fn quietest(audio: &[f32]) -> usize {
    let mut best = (f64::INFINITY, audio.len() / 2);
    let mut at = 0;
    while at + CUT_WINDOW <= audio.len() {
        let energy: f64 = audio[at..at + CUT_WINDOW].iter().map(|&x| f64::from(x) * f64::from(x)).sum();
        if energy <= best.0 {
            best = (energy, at + CUT_WINDOW / 2);
        }
        at += CUT_STEP;
    }
    best.1
}

fn lock<'a>(session: &'a Mutex<Session>, name: &str) -> anyhow::Result<MutexGuard<'a, Session>> {
    session.lock().map_err(|_| anyhow!("{name} session poisoned"))
}

fn output<'a>(outputs: &'a SessionOutputs<'_>, name: &str) -> anyhow::Result<&'a ort::value::DynValue> {
    outputs.get(name).with_context(|| format!("model has no output named {name}"))
}

/// An f32 output, copied out of the session.
fn floats(outputs: &SessionOutputs<'_>, name: &str) -> anyhow::Result<Vec<f32>> {
    Ok(output(outputs, name)?.try_extract_tensor::<f32>()?.1.to_vec())
}

/// A `[1, rows, cols]` f32 output as `(rows, cols, data)`.
fn matrix_output<'a>(
    outputs: &'a SessionOutputs<'_>,
    name: &str,
) -> anyhow::Result<(usize, usize, &'a [f32])> {
    let (shape, data) = output(outputs, name)?.try_extract_tensor::<f32>()?;
    match shape[..] {
        [1, rows, cols] => Ok((rows as usize, cols as usize, data)),
        _ => bail!("unexpected {name} shape {shape}"),
    }
}

/// The first entry of an i64 length output, clamped at zero.
fn length_output(outputs: &SessionOutputs<'_>, name: &str) -> anyhow::Result<usize> {
    let (_, lens) = output(outputs, name)?.try_extract_tensor::<i64>()?;
    Ok(lens.first().copied().unwrap_or(0).max(0) as usize)
}

/// int8 on the CPU, fp32 on CUDA; the other precision if only that is on
/// disk. The string says which was chosen and why, for the compute report.
fn pick_precision(id: &str, dir: &Path, device: Device) -> anyhow::Result<(Precision, String)> {
    let (preferred, other) = match device {
        Device::Cpu => (Precision::Int8, Precision::Fp32),
        Device::Cuda | Device::WebGpu => (Precision::Fp32, Precision::Int8),
    };
    if is_downloaded(id, preferred) {
        return Ok((preferred, format!("{} files", precision_name(preferred))));
    }
    if is_downloaded(id, other) {
        let (p, o) = (precision_name(preferred), precision_name(other));
        return Ok((other, format!("{o} files (the {p} files are missing, using the {o} ones on disk)")));
    }
    let missing: Vec<_> = model_files(preferred).into_iter().filter(|f| !dir.join(f).is_file()).collect();
    bail!("model {id} is not downloaded: missing {} in {}", missing.join(", "), dir.display())
}

/// ONNX Runtime's errors lead with the source location of its own build
/// (`/home/runner/work/.../provider_bridge_ort.cc:1952 Provider& ...Get()
/// [ONNXRuntimeError] : 1 : FAIL : Failed to load library ...`); keep the
/// part after the last ` : `, which says what happened.
fn tidy_ort_error(message: &str) -> String {
    match message.rfind("[ONNXRuntimeError]") {
        Some(at) => message[at..].rsplit(" : ").next().unwrap_or(message).trim().to_string(),
        None => message.to_string(),
    }
}

fn precision_name(precision: Precision) -> &'static str {
    match precision {
        Precision::Int8 => "int8",
        Precision::Fp32 => "fp32",
    }
}

fn build_session(path: &Path, device: Device) -> anyhow::Result<Session> {
    if RUNTIME_RELEASED.load(Ordering::SeqCst) {
        bail!("ONNX Runtime has been shut down; Flow is exiting");
    }
    keep_environment();
    let builder = Session::builder().context("creating session options")?;
    let mut builder = match device {
        Device::Cpu => builder,
        #[cfg(feature = "cuda")]
        Device::Cuda => builder
            .with_execution_providers([ort::ep::CUDA::default().build().error_on_failure()])
            .map_err(|e| anyhow!("registering the CUDA execution provider: {e}"))?,
        #[cfg(not(feature = "cuda"))]
        Device::Cuda => bail!("flow-stt was built without the `cuda` feature"),
        #[cfg(feature = "webgpu")]
        Device::WebGpu => builder
            .with_execution_providers([ort::ep::WebGPU::default().build().error_on_failure()])
            .map_err(|e| anyhow!("registering the WebGPU execution provider: {e}"))?,
        #[cfg(not(feature = "webgpu"))]
        Device::WebGpu => bail!("flow-stt was built without the `webgpu` feature"),
    };
    let started = Instant::now();
    let session = builder.commit_from_file(path).with_context(|| format!("loading {}", path.display()))?;
    debug!(
        "loaded {} in {:.1}s: inputs {:?}, outputs {:?}",
        path.display(),
        started.elapsed().as_secs_f64(),
        session.inputs().iter().map(|o| o.name()).collect::<Vec<_>>(),
        session.outputs().iter().map(|o| o.name()).collect::<Vec<_>>()
    );
    Ok(session)
}

/// Set once [`release_runtime`] has torn ONNX Runtime down.
static RUNTIME_RELEASED: AtomicBool = AtomicBool::new(false);

/// ONNX Runtime's environment, held from the first session on and never
/// dropped. ort would otherwise release it from `.fini_array` at exit, which
/// on Linux runs after the C++ destructors of Dawn and the Vulkan driver;
/// releasing an environment that holds WebGPU then segfaults
/// (`OrtEnv::~OrtEnv` -> `WebGpuContextFactory::Cleanup` ->
/// `dawn::native::InstanceBase::~InstanceBase`). [`release_runtime`] does it
/// at a moment of our choosing instead.
static KEPT: OnceLock<Option<Arc<ort::environment::Environment>>> = OnceLock::new();

fn keep_environment() -> Option<&'static Arc<ort::environment::Environment>> {
    KEPT.get_or_init(|| ort::environment::current().ok()).as_ref()
}

/// Give back the video memory that outlives the recognisers, for when
/// recognition is switched off: CUDA keeps its context (about 400 MB) after
/// every session is gone. WebGPU and the CPU keep nothing. Only while no
/// recogniser is loaded or loading; the next load sets CUDA up again.
/// Returns whether anything was freed.
pub fn free_gpu_context() -> bool {
    let _gpu = lock_gpu();
    if LIVE.load(Ordering::SeqCst) > 0 || RUNTIME_RELEASED.load(Ordering::SeqCst) {
        return false;
    }
    let freed = gpu::reset_cuda();
    if freed {
        info!("freed the CUDA context");
    }
    freed
}

/// Tear ONNX Runtime down now, while every library it drives is still
/// loaded, and with it the GPU context recognition used: CUDA's, or
/// WebGPU's Dawn device and instance. For when the app exits, after every
/// [`Parakeet`] was [unloaded](Parakeet::unload); nothing can be recognised
/// afterwards. Does nothing, and says so, while a recogniser is still
/// loaded, since its sessions depend on the runtime: the OS reclaims the
/// GPU at exit either way. Returns whether it released.
pub fn release_runtime() -> bool {
    // Only a runtime that was set up; asking for one here would create it.
    let Some(Some(env)) = KEPT.get() else { return false };
    // Flag first, count second; a build counts itself first and checks the
    // flag second (`build_on`, `build_session`). So a build racing this one
    // either sees the flag and stops, or is counted here and the flag is
    // taken back.
    if RUNTIME_RELEASED.swap(true, Ordering::SeqCst) {
        return false;
    }
    let live = LIVE.load(Ordering::SeqCst);
    if live > 0 {
        RUNTIME_RELEASED.store(false, Ordering::SeqCst);
        warn!("{live} recogniser(s) still loaded; leaving ONNX Runtime to the OS");
        return false;
    }
    let _gpu = lock_gpu();
    // SAFETY: no session is left (counted above), none can be made now
    // (`RUNTIME_RELEASED`), and ort never releases this environment itself
    // because `keep_environment` holds a reference to it forever.
    unsafe { (ort::api().ReleaseEnv)(ort::AsPointer::ptr(env.as_ref()).cast_mut()) };
    // CUDA keeps its context, and ~400 MB of video memory with it, until the
    // process ends; WebGPU's went with the environment.
    let cuda = gpu::reset_cuda();
    info!("released ONNX Runtime{}", if cuda { " and the CUDA context" } else { "" });
    true
}

/// `[layers, 1, hidden]` from decoder_joint's `input_states_1` (batch is the
/// dynamic axis).
fn state_shape(decoder: &Session) -> anyhow::Result<[usize; 3]> {
    let input = decoder
        .inputs()
        .iter()
        .find(|o| o.name() == "input_states_1")
        .context("decoder_joint has no input_states_1")?;
    let shape = input.dtype().tensor_shape().context("input_states_1 is not a tensor")?;
    // Parakeet's state is a few thousand floats; the cap keeps a broken file
    // from asking for more memory than exists when the zero state is made.
    match shape[..] {
        [layers, _, hidden] if layers > 0 && hidden > 0 && layers.saturating_mul(hidden) <= 1 << 24 => {
            Ok([layers as usize, 1, hidden as usize])
        }
        _ => bail!("unexpected input_states_1 shape {shape}"),
    }
}

/// One second of near-silent noise as `stt.py` used: seed 0, standard
/// normal scaled by 1e-4.
fn warm_up_noise() -> Vec<f32> {
    let mut rng = rand::rngs::StdRng::seed_from_u64(0);
    let mut noise = Vec::with_capacity(SAMPLE_RATE as usize);
    while noise.len() < SAMPLE_RATE as usize {
        // Box-Muller: two uniforms in (0, 1] to two standard normals.
        let u1: f64 = 1.0 - rng.random::<f64>();
        let u2: f64 = rng.random::<f64>();
        let r = (-2.0 * u1.ln()).sqrt();
        let theta = std::f64::consts::TAU * u2;
        noise.push((r * theta.cos() * 1e-4) as f32);
        noise.push((r * theta.sin() * 1e-4) as f32);
    }
    noise.truncate(SAMPLE_RATE as usize);
    noise
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_up_noise_is_quiet_and_deterministic() {
        let a = warm_up_noise();
        let b = warm_up_noise();
        assert_eq!(a.len(), 16_000);
        assert_eq!(a, b);
        let rms = (a.iter().map(|x| x * x).sum::<f32>() / a.len() as f32).sqrt();
        assert!((rms - 1e-4).abs() < 2e-5, "rms {rms}");
    }

    #[test]
    fn ort_errors_lose_their_source_location() {
        let raw = "registering the CUDA execution provider: /home/runner/work/ort-artifacts/onnxruntime/core/session/\
                   provider_bridge_ort.cc:1952 Provider& onnxruntime::ProviderLibrary::Get() [ONNXRuntimeError] : 1 : \
                   FAIL : Failed to load library libonnxruntime_providers_cuda.so with error: libcudnn.so.9: cannot \
                   open shared object file";
        assert_eq!(
            tidy_ort_error(raw),
            "Failed to load library libonnxruntime_providers_cuda.so with error: libcudnn.so.9: cannot open shared object file"
        );
        assert_eq!(tidy_ort_error("plain error"), "plain error");
    }

    #[test]
    fn unknown_provider_is_an_error() {
        let p = Parakeet::new("nemo-parakeet-tdt-0.6b-v3", "tpu");
        let err = p.load().unwrap_err();
        assert!(err.to_string().contains("unknown stt provider"), "{err}");
        assert!(!p.loaded());
    }

    #[test]
    fn an_unloaded_recogniser_stays_unloaded_until_load() {
        // "tpu" fails the moment a build starts, which shows whether one did.
        let p = Parakeet::new("nemo-parakeet-tdt-0.6b-v3", "tpu");
        assert!(!p.unload());
        for _ in 0..2 {
            let err = p.transcribe(&[0.1; 1600], 16_000).unwrap_err();
            assert!(err.to_string().contains("unloaded"), "{err}");
        }
        let err = p.load().unwrap_err();
        assert!(err.to_string().contains("unknown stt provider"), "{err}");
        let err = p.transcribe(&[0.1; 1600], 16_000).unwrap_err();
        assert!(err.to_string().contains("unknown stt provider"), "after load: {err}");
    }

    #[test]
    fn a_sample_rate_of_zero_is_an_error() {
        let p = Parakeet::new("nemo-parakeet-tdt-0.6b-v3", "cpu");
        assert!(p.transcribe(&[0.1; 1600], 0).is_err());
    }

    /// 220 Hz at `amp`, `seconds` long.
    fn tone(seconds: f64, amp: f32) -> Vec<f32> {
        let n = (seconds * SAMPLE_RATE as f64) as usize;
        (0..n).map(|i| amp * (i as f32 * 220.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin()).collect()
    }

    #[test]
    fn audio_that_fits_is_one_piece() {
        assert_eq!(chunks(&[]), vec![0..0]);
        assert_eq!(chunks(&vec![0.5; MAX_CHUNK]), vec![0..MAX_CHUNK]);
    }

    #[test]
    fn long_audio_is_cut_in_its_pauses() {
        // "Speech" with a 300 ms pause at 55 s and at 108 s, 150 s in all.
        let mut audio = tone(55.0, 0.3);
        let first_pause = audio.len()..audio.len() + 4800;
        audio.extend(vec![0.0; 4800]);
        audio.extend(tone(52.7, 0.3));
        let second_pause = audio.len()..audio.len() + 4800;
        audio.extend(vec![0.0; 4800]);
        audio.extend(tone(41.7, 0.3));

        let pieces = chunks(&audio);
        assert_eq!(pieces.len(), 3, "{pieces:?}");
        assert_eq!(pieces[0].start, 0);
        assert_eq!(pieces[2].end, audio.len());
        assert!(pieces.windows(2).all(|w| w[0].end == w[1].start), "{pieces:?}");
        assert!(pieces.iter().all(|p| p.len() <= MAX_CHUNK), "{pieces:?}");
        assert!(first_pause.contains(&pieces[0].end), "{pieces:?}");
        assert!(second_pause.contains(&pieces[1].end), "{pieces:?}");
    }

    #[test]
    fn long_silence_is_cut_late() {
        let pieces = chunks(&vec![0.0; 3 * MAX_CHUNK]);
        assert_eq!(pieces.len(), 4);
        assert!(pieces[0].len() > MAX_CHUNK - CUT_WINDOW && pieces[0].len() <= MAX_CHUNK, "{pieces:?}");
    }

    #[test]
    fn gpu_without_a_backend_is_an_error() {
        if gpu::backend().is_some() {
            return;
        }
        for provider in ["gpu", "cuda"] {
            let p = Parakeet::new("nemo-parakeet-tdt-0.6b-v3", provider);
            let err = p.load().unwrap_err();
            assert!(err.to_string().contains("no GPU support"), "{err}");
        }
    }
}
