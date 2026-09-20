"""Audio helpers: the bits that decide what the recogniser actually sees."""

from __future__ import annotations

import numpy as np
import pytest

from flowd.audio import rms_to_level, trim_silence

SR = 16000


def tone(seconds: float, amplitude: float = 0.3) -> np.ndarray:
    t = np.linspace(0, seconds, int(SR * seconds), endpoint=False)
    return (np.sin(2 * np.pi * 220 * t) * amplitude).astype(np.float32)


def silence(seconds: float) -> np.ndarray:
    return np.zeros(int(SR * seconds), dtype=np.float32)


def test_trims_leading_and_trailing_silence():
    audio = np.concatenate([silence(1.0), tone(1.0), silence(1.0)])
    trimmed = trim_silence(audio, SR, 0.006)
    # 1s of speech plus the 50ms guard band either side.
    assert 1.0 <= len(trimmed) / SR <= 1.25


def test_keeps_a_guard_band_so_words_are_not_clipped():
    audio = np.concatenate([silence(0.5), tone(0.5)])
    trimmed = trim_silence(audio, SR, 0.006)
    # The onset must survive: trimming flush to it shaves the first phoneme.
    assert len(trimmed) / SR > 0.5


def test_all_silence_becomes_empty():
    assert trim_silence(silence(2.0), SR, 0.006).size == 0


def test_pure_speech_is_left_alone():
    audio = tone(1.0)
    assert len(trim_silence(audio, SR, 0.006)) == len(audio)


def test_empty_input_is_safe():
    assert trim_silence(np.zeros(0, dtype=np.float32), SR, 0.006).size == 0


def test_shorter_than_one_window_is_returned_unchanged():
    tiny = tone(0.005)  # under the 10 ms analysis window
    assert np.array_equal(trim_silence(tiny, SR, 0.006), tiny)


@pytest.mark.parametrize("rms", [0.0, 0.001, 0.05, 0.12, 0.5, 2.0])
def test_level_always_lands_in_range(rms):
    assert 0.0 <= rms_to_level(rms) <= 1.0


def test_level_is_monotonic():
    values = [rms_to_level(r) for r in (0.0, 0.01, 0.05, 0.1, 0.2)]
    assert values == sorted(values)


def test_loud_audio_saturates():
    assert rms_to_level(1.0) == 1.0


# -- resampling -------------------------------------------------------------
# Devices named explicitly (a monitor source, or an interface locked to 48 kHz)
# refuse 16 kHz, so Flow captures native and downsamples itself.


def test_resample_is_a_noop_at_the_same_rate():
    from flowd.audio import resample

    audio = tone(0.5)
    assert np.array_equal(resample(audio, SR, SR), audio)


def test_resample_integer_ratio_length():
    from flowd.audio import resample

    audio = tone(1.0, amplitude=0.3)
    out = resample(np.tile(audio, 3)[: 48000], 48000, 16000)
    assert abs(len(out) - 16000) <= 1


def test_resample_non_integer_ratio_length():
    from flowd.audio import resample

    audio = np.zeros(44100, dtype=np.float32)
    out = resample(audio, 44100, 16000)
    assert abs(len(out) - 16000) <= 2


def test_resample_preserves_a_tone_in_band():
    from flowd.audio import resample

    # A 220 Hz tone is far below the 8 kHz Nyquist limit of 16 kHz, so it must
    # survive downsampling with its amplitude roughly intact.
    seconds = 0.5
    t = np.linspace(0, seconds, int(48000 * seconds), endpoint=False)
    audio = (np.sin(2 * np.pi * 220 * t) * 0.5).astype(np.float32)

    out = resample(audio, 48000, 16000)
    assert 0.30 < float(np.sqrt(np.mean(out ** 2))) < 0.40  # RMS of a 0.5 sine is ~0.354


def test_resample_attenuates_content_above_nyquist():
    from flowd.audio import resample

    # 15 kHz cannot be represented at 16 kHz. Naive decimation would alias it
    # down into the speech band; the box filter must suppress it instead.
    seconds = 0.5
    t = np.linspace(0, seconds, int(48000 * seconds), endpoint=False)
    audio = (np.sin(2 * np.pi * 15000 * t) * 0.5).astype(np.float32)

    out = resample(audio, 48000, 16000)
    assert float(np.sqrt(np.mean(out ** 2))) < 0.10


def test_resample_handles_empty_input():
    from flowd.audio import resample

    assert resample(np.zeros(0, dtype=np.float32), 48000, 16000).size == 0
