//! The small amount of signal processing dictation needs: a perceptual
//! level for the waveform, silence trimming, and a resampler for devices
//! that refuse 16 kHz.

/// Map an RMS amplitude to the 0..1 the island's waveform expects.
///
/// Linear amplitude looks dead on screen because speech sits low in the
/// range, so this applies a perceptual curve rather than plotting raw values.
pub fn rms_to_level(rms: f32) -> f32 {
    if rms > 0.0 {
        ((rms / 0.12).powf(0.6)).min(1.0)
    } else {
        0.0
    }
}

pub fn rms(block: &[f32]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    (block.iter().map(|x| x * x).sum::<f32>() / block.len() as f32).sqrt()
}

/// Drop leading and trailing silence.
///
/// Push-to-talk always captures the fumble before you start speaking and the
/// beat after you stop; both cost recognition time and can confuse the model.
pub fn trim_silence(audio: &[f32], sample_rate: u32, threshold: f32) -> Vec<f32> {
    if audio.is_empty() {
        return Vec::new();
    }

    let window = ((sample_rate / 100) as usize).max(1); // 10 ms
    let usable = (audio.len() / window) * window;
    if usable == 0 {
        return audio.to_vec();
    }

    let loud: Vec<bool> = audio[..usable].chunks_exact(window).map(|frame| rms(frame) > threshold).collect();

    let Some(first) = loud.iter().position(|&l| l) else {
        return Vec::new();
    };
    let last = loud.iter().rposition(|&l| l).unwrap() + 1;

    let pad = ((sample_rate / 20) as usize).max(1); // keep 50 ms either side
    let start = (first * window).saturating_sub(pad);
    let end = (last * window + pad).min(audio.len());
    audio[start..end].to_vec()
}

/// Downsample speech to the recogniser's rate.
///
/// Box-filters before decimating rather than interpolating naively: dropping
/// every third sample of 48 kHz audio aliases everything above 8 kHz straight
/// back into the speech band, which the recogniser hears as noise.
pub fn resample(audio: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if source_rate == target_rate || audio.is_empty() {
        return audio.to_vec();
    }

    let ratio = source_rate as f64 / target_rate as f64;
    let width = ratio.round() as usize;
    if ratio > 1.0 && (ratio - width as f64).abs() < 1e-6 {
        let usable = (audio.len() / width) * width;
        if usable > 0 {
            return audio[..usable]
                .chunks_exact(width)
                .map(|chunk| chunk.iter().sum::<f32>() / width as f32)
                .collect();
        }
    }

    // Non-integer ratio: smooth over roughly one output sample, then pick.
    let smoothed = if ratio > 1.0 { boxcar(audio, width.max(1)) } else { audio.to_vec() };
    let count = (smoothed.len() as f64 / ratio).round() as usize;
    if count == 0 {
        return Vec::new();
    }
    linear_pick(&smoothed, count)
}

/// `np.convolve(audio, ones(w)/w, mode="same")`.
fn boxcar(audio: &[f32], window: usize) -> Vec<f32> {
    if window <= 1 {
        return audio.to_vec();
    }
    let n = audio.len();
    // "same" mode centres the kernel; for even windows numpy shifts left by one.
    let lead = (window - 1) / 2;
    (0..n)
        .map(|i| {
            let start = (i + lead + 1).saturating_sub(window);
            let end = (i + lead + 1).min(n);
            audio[start..end].iter().sum::<f32>() / window as f32
        })
        .collect()
}

/// `np.interp(np.linspace(0, n-1, count), arange(n), audio)`.
fn linear_pick(audio: &[f32], count: usize) -> Vec<f32> {
    let n = audio.len();
    if n == 1 || count == 1 {
        return vec![audio[0]; count];
    }
    let step = (n - 1) as f64 / (count - 1) as f64;
    (0..count)
        .map(|i| {
            let pos = i as f64 * step;
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(n - 1);
            let frac = (pos - lo as f64) as f32;
            audio[lo] * (1.0 - frac) + audio[hi] * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const SR: u32 = 16000;

    fn tone(hz: f32, seconds: f32, rate: u32, amp: f32) -> Vec<f32> {
        (0..(seconds * rate as f32) as usize)
            .map(|i| amp * (2.0 * PI * hz * i as f32 / rate as f32).sin())
            .collect()
    }

    fn take(pre: f32, speech: f32, post: f32) -> Vec<f32> {
        let mut v = vec![0.0; (pre * SR as f32) as usize];
        v.extend(tone(220.0, speech, SR, 0.3));
        v.extend(vec![0.0; (post * SR as f32) as usize]);
        v
    }

    #[test]
    fn trims_leading_and_trailing_silence() {
        let out = trim_silence(&take(1.0, 1.0, 1.0), SR, 0.006);
        let secs = out.len() as f32 / SR as f32;
        assert!((1.0..=1.25).contains(&secs), "{secs}");
    }

    #[test]
    fn keeps_a_guard_band_so_words_are_not_clipped() {
        let out = trim_silence(&take(1.0, 1.0, 1.0), SR, 0.006);
        // The first 50 ms of the result is the guard band: (near) silence.
        let pad = (SR / 20) as usize;
        assert!(rms(&out[..pad]) < 0.05);
        assert!(out.len() > (SR as usize) + pad);
    }

    #[test]
    fn all_silence_becomes_empty() {
        assert!(trim_silence(&vec![0.0; SR as usize], SR, 0.006).is_empty());
    }

    #[test]
    fn empty_input_is_safe() {
        assert!(trim_silence(&[], SR, 0.006).is_empty());
    }

    #[test]
    fn pure_speech_is_left_alone() {
        let speech = tone(220.0, 1.0, SR, 0.3);
        assert_eq!(trim_silence(&speech, SR, 0.006).len(), speech.len());
    }

    #[test]
    fn shorter_than_one_window_is_returned_unchanged() {
        let tiny = vec![0.0; 100];
        assert_eq!(trim_silence(&tiny, SR, 0.006).len(), 100);
    }

    #[test]
    fn level_always_lands_in_range() {
        for rms in [0.0, 0.001, 0.05, 0.12, 0.5, 2.0] {
            let l = rms_to_level(rms);
            assert!((0.0..=1.0).contains(&l), "{rms} -> {l}");
        }
    }

    #[test]
    fn level_is_monotonic() {
        let mut last = -1.0;
        for i in 0..100 {
            let l = rms_to_level(i as f32 / 100.0);
            assert!(l >= last);
            last = l;
        }
    }

    #[test]
    fn loud_audio_saturates() {
        assert_eq!(rms_to_level(1.0), 1.0);
    }

    #[test]
    fn resample_is_a_noop_at_the_same_rate() {
        let a = tone(220.0, 0.1, SR, 0.3);
        assert_eq!(resample(&a, SR, SR), a);
    }

    #[test]
    fn resample_integer_ratio_length() {
        let a = tone(220.0, 1.0, 48000, 0.3);
        let out = resample(&a, 48000, 16000);
        assert!((out.len() as i64 - 16000).abs() <= 1, "{}", out.len());
    }

    #[test]
    fn resample_non_integer_ratio_length() {
        let a = tone(220.0, 1.0, 44100, 0.3);
        let out = resample(&a, 44100, 16000);
        assert!((out.len() as i64 - 16000).abs() <= 2, "{}", out.len());
    }

    #[test]
    fn resample_preserves_a_tone_in_band() {
        let a = tone(220.0, 1.0, 48000, 0.5);
        let out = resample(&a, 48000, 16000);
        let r = rms(&out);
        assert!((0.30..=0.40).contains(&r), "{r}");
    }

    #[test]
    fn resample_attenuates_content_above_nyquist() {
        let a = tone(15000.0, 1.0, 48000, 0.5);
        let out = resample(&a, 48000, 16000);
        assert!(rms(&out) < 0.10, "{}", rms(&out));
    }

    #[test]
    fn resample_handles_empty_input() {
        assert!(resample(&[], 48000, 16000).is_empty());
    }
}
