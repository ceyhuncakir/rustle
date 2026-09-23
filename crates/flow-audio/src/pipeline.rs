//! The device-free half of capture: interleaved samples in, mono at the
//! recogniser's rate out, one level per fixed block.
//!
//! Everything here runs inside cpal's audio callback, so after
//! [`Pipeline::new`] the hot path does no allocation in the common case:
//! scratch buffers are sized up front and the take reserves a minute of
//! audio. (A callback larger than the scratch, or a take longer than the
//! reservation, grows a `Vec` once - rare, and harmless for dictation.)

use flow_core::dsp;
use log::warn;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, Fft, FixedAsync, FixedSync, Indexing, Resampler, SincInterpolationParameters, WindowFunction,
};

/// The block the level meter sees. 512 samples at 16 kHz is 32 ms, which is
/// what PortAudio handed the Python daemon per callback; the engine throttles
/// levels to 50 Hz anyway, so ~31 blocks/s loses nothing on screen.
pub const BLOCK_FRAMES: usize = 512;

/// The resampler is fed fixed chunks of this many *input* frames no matter
/// how cpal sizes its callbacks. 512 frames at 48 kHz is ~11 ms of latency,
/// which is invisible behind a hotkey release.
const RESAMPLE_CHUNK: usize = 512;

/// rubato's FFT resampler works in blocks of `rate / gcd(rate, target)`
/// frames. For 48 kHz -> 16 kHz that is 3; for 44.1 kHz it is 441. Past this
/// the blocks are so long that the latency and the FFT cost stop making
/// sense, and the sinc resampler (any ratio, ~same quality) takes over.
const MAX_FFT_BLOCK: u32 = 4096;

/// Sinc length for the fallback resampler. 128 is plenty for speech, and
/// half the CPU of rubato's 256 default.
const SINC_LEN: usize = 128;

/// Take capacity reserved at start, so `Vec::push` in the callback does not
/// reallocate during a normal dictation.
const RESERVE_SECONDS: usize = 60;

/// The largest cpal callback the mono scratch is pre-sized for.
const SCRATCH_FRAMES: usize = 8192;

/// Sample decoding for every PCM format cpal delivers. Written out rather
/// than borrowed from `dasp` so the scaling is explicit and testable: full
/// scale integer maps to `-1.0..1.0`, and unsigned formats are offset binary,
/// silent at the middle of their range.
pub trait ToF32: Copy {
    fn to_f32(self) -> f32;
}

impl ToF32 for f32 {
    #[inline]
    fn to_f32(self) -> f32 {
        self
    }
}

impl ToF32 for f64 {
    #[inline]
    fn to_f32(self) -> f32 {
        self as f32
    }
}

macro_rules! full_scale_to_f32 {
    ($($int:ty),*) => {$(
        impl ToF32 for $int {
            #[inline]
            fn to_f32(self) -> f32 {
                (self as f64 / -(<$int>::MIN as f64)) as f32
            }
        }
    )*};
}

full_scale_to_f32!(i8, i16, i32, i64);

macro_rules! offset_binary_to_f32 {
    ($($uint:ty),*) => {$(
        impl ToF32 for $uint {
            #[inline]
            fn to_f32(self) -> f32 {
                let mid = (<$uint>::MAX as f64 + 1.0) / 2.0;
                ((self as f64 - mid) / mid) as f32
            }
        }
    )*};
}

offset_binary_to_f32!(u8, u16, u32, u64);

/// 24-bit samples, which USB interfaces opened as `hw:` devices often
/// deliver, arrive in 32-bit containers holding `-2^23..2^23`.
const I24_FULL_SCALE: f64 = 8_388_608.0;

impl ToF32 for cpal::I24 {
    #[inline]
    fn to_f32(self) -> f32 {
        (self.inner() as f64 / I24_FULL_SCALE) as f32
    }
}

impl ToF32 for cpal::U24 {
    #[inline]
    fn to_f32(self) -> f32 {
        ((self.inner() as f64 - I24_FULL_SCALE) / I24_FULL_SCALE) as f32
    }
}

/// Re-frames an arbitrary stream of samples into fixed blocks.
///
/// cpal's `BufferSize::Fixed(512)` is a request, not a promise: ALSA rounds
/// period sizes to what the hardware (or the PipeWire shim) supports, and a
/// callback can also hand over less after an xrun. The level meter wants
/// evenly sized blocks regardless, so it gets them from here.
pub struct Framer {
    size: usize,
    block: Vec<f32>,
}

impl Framer {
    pub fn new(size: usize) -> Framer {
        Framer { size: size.max(1), block: Vec::with_capacity(size.max(1)) }
    }

    /// Append `samples`, calling `on_block` with every block that completes.
    /// Whatever is left over waits for the next push.
    pub fn push(&mut self, samples: &[f32], mut on_block: impl FnMut(&[f32])) {
        let mut rest = samples;
        while !rest.is_empty() {
            let room = self.size - self.block.len();
            let n = room.min(rest.len());
            self.block.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if self.block.len() == self.size {
                on_block(&self.block);
                self.block.clear();
            }
        }
    }

    /// The samples that have not filled a block yet.
    #[cfg(test)]
    pub fn tail(&self) -> &[f32] {
        &self.block
    }
}

enum Stage {
    /// Same rate in and out, or rubato refused the ratio (then [`Pipeline::finish`]
    /// runs the whole take through `flow_core::dsp::resample`).
    Passthrough,
    Rubato(Box<dyn Resampler<f32>>),
}

/// Interleaved device samples in, mono `target_rate` samples out.
pub struct Pipeline {
    channels: usize,
    source_rate: u32,
    target_rate: u32,
    stage: Stage,
    /// One callback's worth of downmixed frames.
    mono: Vec<f32>,
    /// Resampler input: exactly `input_frames_next()` long, `filled` valid.
    chunk: Vec<f32>,
    filled: usize,
    /// Resampler output scratch: `output_frames_max()` long.
    out: Vec<f32>,
    /// Resampler start-up delay still to be dropped from the head of the output.
    skip: usize,
    /// Real (not padding) frames fed to the resampler so far.
    frames_in: usize,
    framer: Framer,
    /// Levels produced since the last [`Pipeline::drain_levels`].
    levels: Vec<f32>,
    take: Vec<f32>,
}

impl Pipeline {
    pub fn new(channels: u16, source_rate: u32, target_rate: u32) -> Pipeline {
        let stage = if source_rate == target_rate {
            Stage::Passthrough
        } else {
            match make_resampler(source_rate, target_rate) {
                Some(resampler) => Stage::Rubato(resampler),
                None => {
                    warn!(
                        "rubato refused {source_rate} -> {target_rate} Hz; \
                         the take will be resampled with flow_core::dsp on stop"
                    );
                    Stage::Passthrough
                }
            }
        };

        // Passthrough with a rate mismatch accumulates at the source rate.
        let (chunk, out, skip, take_rate) = match &stage {
            Stage::Rubato(r) => (
                vec![0.0; r.input_frames_next()],
                vec![0.0; r.output_frames_max()],
                r.output_delay(),
                target_rate,
            ),
            Stage::Passthrough => (Vec::new(), Vec::new(), 0, source_rate),
        };

        Pipeline {
            channels: channels.max(1) as usize,
            source_rate,
            target_rate,
            stage,
            mono: Vec::with_capacity(SCRATCH_FRAMES),
            chunk,
            filled: 0,
            out,
            skip,
            frames_in: 0,
            framer: Framer::new(BLOCK_FRAMES),
            levels: Vec::with_capacity(SCRATCH_FRAMES / BLOCK_FRAMES + 2),
            take: Vec::with_capacity(take_rate as usize * RESERVE_SECONDS),
        }
    }

    /// Feed one cpal callback: `channels`-interleaved samples of any length.
    pub fn push<T: ToF32>(&mut self, interleaved: &[T]) {
        let scale = 1.0 / self.channels as f32;
        let mut mono = std::mem::take(&mut self.mono);
        mono.clear();
        mono.extend(
            interleaved
                .chunks_exact(self.channels)
                .map(|frame| frame.iter().map(|s| s.to_f32()).sum::<f32>() * scale),
        );
        self.feed(&mono);
        self.mono = mono;
    }

    /// Levels produced since the last call, one per completed block, in order.
    pub fn drain_levels(&mut self) -> std::vec::Drain<'_, f32> {
        self.levels.drain(..)
    }

    /// Flush the resampler's tail and hand over the whole take at
    /// `target_rate`. The pipeline is empty afterwards.
    pub fn finish(&mut self) -> Vec<f32> {
        match self.stage {
            Stage::Passthrough => {
                let take = std::mem::take(&mut self.take);
                if self.source_rate == self.target_rate {
                    take
                } else {
                    dsp::resample(&take, self.source_rate, self.target_rate)
                }
            }
            Stage::Rubato(_) => {
                // The resampler always emits whole output chunks, so pad the
                // last partial chunk (maybe empty) and then silence until the
                // delayed samples have come out, then cut to the exact length
                // the input duration implies.
                let expected = (self.frames_in as f64 * self.target_rate as f64 / self.source_rate as f64)
                    .round() as usize;
                self.resample_chunk(Some(self.filled));
                let mut flushes = 0;
                while self.take.len() < expected && flushes < 8 {
                    self.resample_chunk(Some(0));
                    flushes += 1;
                }
                self.take.truncate(expected);
                self.frames_in = 0;
                self.skip = 0;
                std::mem::take(&mut self.take)
            }
        }
    }

    fn feed(&mut self, mono: &[f32]) {
        if matches!(self.stage, Stage::Passthrough) {
            self.emit(mono);
            return;
        }
        let mut rest = mono;
        while !rest.is_empty() {
            let room = self.chunk.len() - self.filled;
            let n = room.min(rest.len());
            self.chunk[self.filled..self.filled + n].copy_from_slice(&rest[..n]);
            self.filled += n;
            self.frames_in += n;
            rest = &rest[n..];
            if self.filled == self.chunk.len() {
                self.resample_chunk(None);
            }
        }
    }

    /// Run the resampler over `chunk`. `partial` limits how many frames of it
    /// are real (the rest is silence); `None` means the whole chunk.
    fn resample_chunk(&mut self, partial: Option<usize>) {
        let Stage::Rubato(resampler) = &mut self.stage else {
            return;
        };
        let indexing = partial.map(|n| Indexing::new().partial_len(n));
        let written = {
            let frames = self.chunk.len();
            let capacity = self.out.len();
            let (Ok(input), Ok(mut output)) = (
                InterleavedSlice::new(self.chunk.as_slice(), 1, frames),
                InterleavedSlice::new_mut(self.out.as_mut_slice(), 1, capacity),
            ) else {
                return;
            };
            match resampler.process_into_buffer(&input, &mut output, indexing.as_ref()) {
                Ok((_, written)) => written,
                Err(err) => {
                    warn!("resampler error: {err}");
                    0
                }
            }
        };
        self.filled = 0;
        let skip = self.skip.min(written);
        self.skip -= skip;
        let out = std::mem::take(&mut self.out);
        self.emit(&out[skip..written]);
        self.out = out;
    }

    fn emit(&mut self, samples: &[f32]) {
        self.take.extend_from_slice(samples);
        let levels = &mut self.levels;
        self.framer.push(samples, |block| {
            levels.push(dsp::rms_to_level(dsp::rms(block)));
        });
    }
}

/// FFT for ratios it handles in small blocks (48k, 44.1k, 32k, 96k -> 16k),
/// sinc for anything else, `None` if rubato refuses both.
fn make_resampler(source_rate: u32, target_rate: u32) -> Option<Box<dyn Resampler<f32>>> {
    if source_rate == 0 || target_rate == 0 {
        return None;
    }
    if source_rate / gcd(source_rate, target_rate) <= MAX_FFT_BLOCK {
        match Fft::<f32>::new(source_rate as usize, target_rate as usize, RESAMPLE_CHUNK, 1, FixedSync::Input)
        {
            Ok(resampler) => return Some(Box::new(resampler)),
            Err(err) => warn!("fft resampler refused {source_rate} -> {target_rate}: {err}"),
        }
    }
    let params = SincInterpolationParameters::new(SINC_LEN, WindowFunction::BlackmanHarris2);
    match Async::<f32>::new_sinc(
        target_rate as f64 / source_rate as f64,
        1.0,
        &params,
        RESAMPLE_CHUNK,
        1,
        FixedAsync::Input,
    ) {
        Ok(resampler) => Some(Box::new(resampler)),
        Err(err) => {
            warn!("sinc resampler refused {source_rate} -> {target_rate}: {err}");
            None
        }
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn tone(hz: f32, seconds: f32, rate: u32, amp: f32) -> Vec<f32> {
        (0..(seconds * rate as f32) as usize)
            .map(|i| amp * (2.0 * PI * hz * i as f32 / rate as f32).sin())
            .collect()
    }

    /// Feed `audio` the way a misbehaving device would: in callbacks of
    /// uneven, non-power-of-two frame counts. (Whole frames, though: cpal
    /// never splits a frame across callbacks.)
    fn push_awkwardly(p: &mut Pipeline, audio: &[f32]) {
        let channels = p.channels;
        let sizes = [441usize, 1000, 512, 37, 2048];
        let mut i = 0;
        let mut pos = 0;
        while pos < audio.len() {
            let n = (sizes[i % sizes.len()] * channels).min(audio.len() - pos);
            p.push(&audio[pos..pos + n]);
            pos += n;
            i += 1;
        }
    }

    /// One second of a 0.5-amplitude 220 Hz tone at `rate`, pushed awkwardly
    /// through a mono pipeline to 16 kHz: the output must be exactly one
    /// second whose RMS is the tone's, within 10 %.
    fn assert_tone_survives(p: &mut Pipeline, rate: u32) -> Vec<f32> {
        push_awkwardly(p, &tone(220.0, 1.0, rate, 0.5));
        let out = p.finish();
        assert_eq!(out.len(), 16000);
        let want = 0.5 / 2f32.sqrt();
        let got = dsp::rms(&out);
        assert!((got - want).abs() / want < 0.10, "rms {got}, wanted {want} +-10%");
        out
    }

    #[test]
    fn mono_downmix_averages_channels() {
        let mut p = Pipeline::new(2, 16000, 16000);
        p.push(&[0.5f32, -0.5, 1.0, 0.0, -1.0, -1.0]);
        assert_eq!(p.finish(), vec![0.0, 0.5, -1.0]);
    }

    #[test]
    fn four_channels_downmix_too() {
        let mut p = Pipeline::new(4, 16000, 16000);
        p.push(&[1.0f32, 1.0, 1.0, 1.0, 0.2, 0.4, 0.6, 0.8]);
        let out = p.finish();
        assert_eq!(out.len(), 2);
        assert!((out[0] - 1.0).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn i16_scales_to_unit_range() {
        assert_eq!(0i16.to_f32(), 0.0);
        assert_eq!(i16::MIN.to_f32(), -1.0);
        assert_eq!(16_384i16.to_f32(), 0.5);
        assert!((i16::MAX.to_f32() - 1.0).abs() < 1e-4);

        let mut p = Pipeline::new(1, 16000, 16000);
        p.push(&[i16::MIN, 0, 16_384, i16::MAX]);
        let out = p.finish();
        assert_eq!(out[0], -1.0);
        assert_eq!(out[1], 0.0);
        assert_eq!(out[2], 0.5);
        assert!(out[3] > 0.999 && out[3] <= 1.0);
    }

    #[test]
    fn i32_scales_to_unit_range() {
        assert_eq!(i32::MIN.to_f32(), -1.0);
        assert_eq!((1i32 << 30).to_f32(), 0.5);
        assert!((i32::MAX.to_f32() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn every_other_pcm_format_scales_to_unit_range() {
        let i24 = |v| cpal::I24::new(v).unwrap();
        let u24 = |v| cpal::U24::new(v).unwrap();
        let cases: [(f32, f32, f32); 9] = [
            (i8::MIN.to_f32(), 0i8.to_f32(), 64i8.to_f32()),
            (i64::MIN.to_f32(), 0i64.to_f32(), (1i64 << 62).to_f32()),
            (i24(-8_388_608).to_f32(), i24(0).to_f32(), i24(4_194_304).to_f32()),
            (0u8.to_f32(), 128u8.to_f32(), 192u8.to_f32()),
            (0u16.to_f32(), 32_768u16.to_f32(), 49_152u16.to_f32()),
            (u24(0).to_f32(), u24(8_388_608).to_f32(), u24(12_582_912).to_f32()),
            (0u32.to_f32(), (1u32 << 31).to_f32(), (3u32 << 30).to_f32()),
            (0u64.to_f32(), (1u64 << 63).to_f32(), (3u64 << 62).to_f32()),
            ((-1.0f64).to_f32(), 0.0f64.to_f32(), 0.5f64.to_f32()),
        ];
        for (i, (min, zero, half)) in cases.into_iter().enumerate() {
            assert_eq!((min, zero, half), (-1.0, 0.0, 0.5), "case {i}");
        }
        assert!((i24(8_388_607).to_f32() - 1.0).abs() < 1e-6);
        assert!((u8::MAX.to_f32() - 1.0).abs() < 1e-2);
    }

    #[test]
    fn same_rate_is_passed_through_untouched() {
        let a = tone(220.0, 0.1, 16000, 0.3);
        let mut p = Pipeline::new(1, 16000, 16000);
        push_awkwardly(&mut p, &a);
        assert_eq!(p.finish(), a);
    }

    #[test]
    fn resampling_48k_to_16k_preserves_a_tone() {
        assert_tone_survives(&mut Pipeline::new(1, 48000, 16000), 48000);
    }

    #[test]
    fn resampling_48k_to_16k_attenuates_above_nyquist() {
        let a = tone(15000.0, 1.0, 48000, 0.5);
        let mut p = Pipeline::new(1, 48000, 16000);
        push_awkwardly(&mut p, &a);
        let out = p.finish();
        assert!(dsp::rms(&out) < 0.10, "{}", dsp::rms(&out));
    }

    #[test]
    fn resampling_44_1k_to_16k_preserves_a_tone() {
        assert_tone_survives(&mut Pipeline::new(1, 44100, 16000), 44100);
    }

    #[test]
    fn an_awkward_ratio_takes_the_sinc_path_and_still_works() {
        // gcd(44101, 16000) == 1, so the FFT block would be 44101 frames.
        let mut p = Pipeline::new(1, 44101, 16000);
        assert!(matches!(p.stage, Stage::Rubato(_)));
        assert_tone_survives(&mut p, 44101);
    }

    #[test]
    fn finish_flushes_the_tail_to_the_exact_length() {
        // 49_000 frames is not a multiple of the resampler chunk, so the
        // last 369 frames only come out through the flush.
        let a = tone(220.0, 1.0, 48000, 0.5);
        let mut p = Pipeline::new(1, 48000, 16000);
        p.push(&a);
        p.push(&a[..1000]);
        let out = p.finish();
        assert_eq!(out.len(), (49_000.0f64 / 3.0).round() as usize);
        // The tail is real audio, not the padding.
        assert!(dsp::rms(&out[out.len() - 300..]) > 0.2);
    }

    #[test]
    fn finish_with_nothing_captured_is_empty() {
        let mut p = Pipeline::new(2, 48000, 16000);
        assert!(p.finish().is_empty());
        let mut p = Pipeline::new(1, 16000, 16000);
        assert!(p.finish().is_empty());
    }

    #[test]
    fn stereo_48k_end_to_end() {
        let a = tone(220.0, 0.5, 48000, 0.5);
        let interleaved: Vec<f32> = a.iter().flat_map(|&s| [s, s]).collect();
        let mut p = Pipeline::new(2, 48000, 16000);
        push_awkwardly(&mut p, &interleaved);
        let out = p.finish();
        assert_eq!(out.len(), 8000);
        let want = 0.5 / 2f32.sqrt();
        assert!((dsp::rms(&out) - want).abs() / want < 0.10);
    }

    #[test]
    fn reframes_awkward_callbacks_into_512_blocks() {
        let ramp: Vec<f32> = (0..1441).map(|i| i as f32).collect();
        let mut framer = Framer::new(512);
        let mut blocks: Vec<Vec<f32>> = Vec::new();
        framer.push(&ramp[..441], |b| blocks.push(b.to_vec()));
        assert!(blocks.is_empty());
        assert_eq!(framer.tail().len(), 441);
        framer.push(&ramp[441..], |b| blocks.push(b.to_vec()));
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], ramp[..512]);
        assert_eq!(blocks[1], ramp[512..1024]);
        assert_eq!(framer.tail(), &ramp[1024..]);
        assert_eq!(framer.tail().len(), 417);
    }

    #[test]
    fn a_level_is_emitted_per_block() {
        let a = tone(220.0, 1.0, 16000, 0.3);
        let mut p = Pipeline::new(1, 16000, 16000);
        p.push(&a[..441]);
        assert_eq!(p.drain_levels().count(), 0);
        p.push(&a[441..1441]);
        let levels: Vec<f32> = p.drain_levels().collect();
        assert_eq!(levels.len(), 2);
        assert!(levels.iter().all(|&l| l > 0.5 && l <= 1.0), "{levels:?}");
        // 417 samples are waiting in the framer: 90 more do not complete a
        // block, the 5 after that do.
        p.push(&a[1441..1441 + 90]);
        assert_eq!(p.drain_levels().count(), 0);
        p.push(&a[1531..1531 + 5]);
        assert_eq!(p.drain_levels().count(), 1);
        p.push(&a[1536..1536 + 1024]);
        assert_eq!(p.drain_levels().count(), 2);
    }

    #[test]
    fn levels_follow_the_resampled_output() {
        // One second at 48 kHz becomes 16000 samples: 31 full blocks of 512.
        let a = tone(220.0, 1.0, 48000, 0.3);
        let mut p = Pipeline::new(1, 48000, 16000);
        push_awkwardly(&mut p, &a);
        let n = p.drain_levels().count();
        assert!((29..=31).contains(&n), "{n} levels before flush");
    }

    #[test]
    fn silence_levels_are_zero() {
        let mut p = Pipeline::new(1, 16000, 16000);
        p.push(&vec![0.0f32; 1024]);
        let levels: Vec<f32> = p.drain_levels().collect();
        assert_eq!(levels, vec![0.0, 0.0]);
    }

    #[test]
    fn gcd_is_right() {
        assert_eq!(gcd(48000, 16000), 16000);
        assert_eq!(gcd(44100, 16000), 100);
        assert_eq!(gcd(44101, 16000), 1);
    }
}
