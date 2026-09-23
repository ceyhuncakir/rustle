//! The cpal-backed [`Recorder`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Context;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, InputCallbackInfo, SampleFormat, SizedSample, StreamConfig};
use flow_core::engine::{Event, Recorder};
use log::{debug, info, warn};

use crate::devices::{device_name, find_input_device};
use crate::pipeline::{Pipeline, ToF32, BLOCK_FRAMES};

/// Records from a cpal input device until stopped.
///
/// The stream is opened on [`Recorder::start`] and closed on
/// [`Recorder::stop`], exactly like the PortAudio stream was, so nothing
/// holds the microphone between takes.
pub struct CpalRecorder {
    device: Option<String>,
    target_rate: u32,
    levels: Sender<Event>,
    stream: Option<cpal::Stream>,
    pipeline: Option<Arc<Mutex<Pipeline>>>,
    errored: Arc<AtomicBool>,
}

impl CpalRecorder {
    /// `device` is a case-insensitive substring of an input device's name
    /// (`None` for the system default); `target_rate` is what [`Recorder::stop`]
    /// returns audio at; every captured block reports an [`Event::Level`]
    /// through `levels`.
    pub fn new(device: Option<String>, target_rate: u32, levels: Sender<Event>) -> CpalRecorder {
        CpalRecorder {
            device,
            target_rate,
            levels,
            stream: None,
            pipeline: None,
            errored: Arc::new(AtomicBool::new(false)),
        }
    }

    fn open(&self) -> anyhow::Result<(cpal::Stream, Arc<Mutex<Pipeline>>)> {
        let host = cpal::default_host();
        let device = find_input_device(&host, self.device.as_deref())?;
        let name = device_name(&device);

        // Capture at the device's own default rate and channel count rather
        // than asking for 16 kHz mono. PipeWire's "default" ALSA device would
        // convert for us, but a device named explicitly - a monitor source,
        // an interface locked to 48 kHz - refuses, which is the fallback the
        // Python daemon needed. Taking what the device offers and resampling
        // in-process makes every device take the same path.
        let default = device
            .default_input_config()
            .with_context(|| format!("{name}: no usable input configuration"))?;
        let format = pick_format(&device, &default);
        let config = StreamConfig {
            channels: default.channels(),
            sample_rate: default.sample_rate(),
            // A request, not a promise: ALSA rounds the period to what the
            // hardware or the PipeWire shim allows, and some hosts ignore it
            // entirely. The pipeline re-frames whatever arrives, so this only
            // steers latency.
            buffer_size: BufferSize::Fixed(BLOCK_FRAMES as u32),
        };
        info!(
            "capturing {name}: {} Hz, {} ch, {format:?} -> {} Hz mono",
            config.sample_rate, config.channels, self.target_rate
        );

        let pipeline =
            Arc::new(Mutex::new(Pipeline::new(config.channels, config.sample_rate, self.target_rate)));

        let stream = match self.build(&device, config, format, &pipeline) {
            Ok(stream) => stream,
            Err(err) => {
                debug!("fixed {BLOCK_FRAMES}-frame buffer refused ({err}); using the device default");
                let config = StreamConfig { buffer_size: BufferSize::Default, ..config };
                self.build(&device, config, format, &pipeline)
                    .with_context(|| format!("{name}: could not open the input stream"))?
            }
        };
        stream.play().with_context(|| format!("{name}: could not start the input stream"))?;
        Ok((stream, pipeline))
    }

    fn build(
        &self,
        device: &cpal::Device,
        config: StreamConfig,
        format: SampleFormat,
        pipeline: &Arc<Mutex<Pipeline>>,
    ) -> Result<cpal::Stream, cpal::Error> {
        match format {
            SampleFormat::F32 => self.build_typed::<f32>(device, config, pipeline),
            SampleFormat::I16 => self.build_typed::<i16>(device, config, pipeline),
            SampleFormat::I32 => self.build_typed::<i32>(device, config, pipeline),
            other => Err(cpal::Error::with_message(
                ErrorKind::UnsupportedConfig,
                format!("sample format {other:?} is not supported (want F32, I16 or I32)"),
            )),
        }
    }

    fn build_typed<T: SizedSample + ToF32>(
        &self,
        device: &cpal::Device,
        config: StreamConfig,
        pipeline: &Arc<Mutex<Pipeline>>,
    ) -> Result<cpal::Stream, cpal::Error> {
        let pipeline = Arc::clone(pipeline);
        let levels = self.levels.clone();
        let errored = Arc::clone(&self.errored);
        device.build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _: &InputCallbackInfo| {
                // Only `stop` ever contends for this lock, and it does so
                // after the stream (and with it this callback) is gone.
                let mut pipeline = lock(&pipeline);
                pipeline.push(data);
                for level in pipeline.drain_levels() {
                    // The engine may already be shutting down; nothing to do.
                    let _ = levels.send(Event::Level(level));
                }
            },
            move |err| {
                // Overruns and device disappearances are logged, not fatal:
                // whatever was captured before is still the take.
                warn!("input stream error: {err}");
                errored.store(true, Ordering::Relaxed);
            },
            None,
        )
    }
}

impl Recorder for CpalRecorder {
    fn start(&mut self) -> anyhow::Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        self.errored.store(false, Ordering::Relaxed);
        let (stream, pipeline) = self.open()?;
        self.stream = Some(stream);
        self.pipeline = Some(pipeline);
        Ok(())
    }

    fn stop(&mut self) -> Vec<f32> {
        let Some(stream) = self.stream.take() else {
            return Vec::new();
        };
        // Dropping the stream stops it and joins its thread, so the callback
        // is finished before the pipeline is touched.
        drop(stream);
        let Some(pipeline) = self.pipeline.take() else {
            return Vec::new();
        };
        let take = lock(&pipeline).finish();
        if self.errored.swap(false, Ordering::Relaxed) {
            warn!(
                "the input stream reported an error during the take; \
                 returning the {} samples captured",
                take.len()
            );
        }
        take
    }

    fn recording(&self) -> bool {
        self.stream.is_some()
    }
}

/// F32 if the device offers it at the default rate and channel count, then
/// I16, then I32; failing all three, the default format (which `build` will
/// then refuse with a clear message).
fn pick_format(device: &cpal::Device, default: &cpal::SupportedStreamConfig) -> SampleFormat {
    let (rate, channels) = (default.sample_rate(), default.channels());
    let available: Vec<SampleFormat> = device
        .supported_input_configs()
        .into_iter()
        .flatten()
        .filter(|r| r.channels() == channels && (r.min_sample_rate()..=r.max_sample_rate()).contains(&rate))
        .map(|r| r.sample_format())
        .chain([default.sample_format()])
        .collect();
    [SampleFormat::F32, SampleFormat::I16, SampleFormat::I32]
        .into_iter()
        .find(|f| available.contains(f))
        .unwrap_or_else(|| default.sample_format())
}

/// A poisoned pipeline (a panic in the callback) still holds the audio.
fn lock(pipeline: &Mutex<Pipeline>) -> MutexGuard<'_, Pipeline> {
    pipeline.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn stop_without_start_is_empty() {
        let (tx, _rx) = mpsc::channel();
        let mut rec = CpalRecorder::new(None, 16000, tx);
        assert!(!rec.recording());
        assert!(rec.stop().is_empty());
        assert!(!rec.recording());
    }

    #[test]
    #[ignore = "needs a microphone: run with --ignored"]
    fn records_from_the_default_device() {
        let _ = env_logger::try_init();
        let (tx, rx) = mpsc::channel();
        let mut rec = CpalRecorder::new(None, 16000, tx);
        rec.start().expect("start");
        assert!(rec.recording());
        rec.start().expect("second start is a no-op");
        std::thread::sleep(Duration::from_secs(1));
        let take = rec.stop();
        assert!(!rec.recording());
        let n = take.len();
        assert!((12_800..=19_200).contains(&n), "{n} samples for 1 s at 16 kHz");
        let levels: Vec<f32> = rx
            .try_iter()
            .filter_map(|e| match e {
                Event::Level(l) => Some(l),
                _ => None,
            })
            .collect();
        assert!(!levels.is_empty(), "no levels reported");
        eprintln!(
            "captured {n} samples, {} levels, peak level {:.3}, rms {:.4}",
            levels.len(),
            levels.iter().cloned().fold(0.0, f32::max),
            flow_core::dsp::rms(&take)
        );
        assert!(rec.stop().is_empty());
    }
}
