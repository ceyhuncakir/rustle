"""Microphone capture.

PortAudio talks to PipeWire, which resamples the device - a 48 kHz interface,
say - down to the 16 kHz mono the recogniser wants, so nothing here has to care
what hardware is plugged in.
"""

from __future__ import annotations

import logging
import queue
import threading
from collections.abc import Callable

import numpy as np
import sounddevice as sd

log = logging.getLogger(__name__)

BLOCK_FRAMES = 512


def resample(audio: np.ndarray, source_rate: int, target_rate: int) -> np.ndarray:
    """Downsample speech to the recogniser's rate.

    Box-filters before decimating rather than interpolating naively: dropping
    every third sample of 48 kHz audio aliases everything above 8 kHz straight
    back into the speech band, which the recogniser hears as noise.
    """
    if source_rate == target_rate or audio.size == 0:
        return audio.astype(np.float32)

    ratio = source_rate / target_rate
    if ratio > 1:
        width = int(round(ratio))
        if abs(ratio - width) < 1e-6:
            usable = (audio.size // width) * width
            if usable:
                return audio[:usable].reshape(-1, width).mean(axis=1).astype(np.float32)
        # Non-integer ratio: smooth over roughly one output sample, then pick.
        window = max(1, width)
        kernel = np.ones(window, dtype=np.float32) / window
        audio = np.convolve(audio, kernel, mode="same")

    count = int(round(audio.size / ratio))
    if count <= 0:
        return np.zeros(0, dtype=np.float32)
    positions = np.linspace(0, audio.size - 1, count)
    return np.interp(positions, np.arange(audio.size), audio).astype(np.float32)


def rms_to_level(rms: float) -> float:
    """Map an RMS amplitude to the 0..1 the island's waveform expects.

    Linear amplitude looks dead on screen because speech sits low in the range,
    so this applies a perceptual curve rather than plotting raw values.
    """
    return float(min(1.0, (rms / 0.12) ** 0.6)) if rms > 0 else 0.0


class Recorder:
    """Records until stopped. Push-to-talk defines the boundaries, so there is
    no VAD in the capture path - silence trimming happens on the finished take."""

    def __init__(
        self,
        sample_rate: int = 16000,
        device: str | None = None,
        on_level: Callable[[float], None] | None = None,
    ) -> None:
        self.sample_rate = sample_rate
        self.device = device or None
        self._on_level = on_level
        self._chunks: queue.Queue[np.ndarray] = queue.Queue()
        self._stream: sd.InputStream | None = None
        self._lock = threading.Lock()
        # Set when the device refuses the target rate and we capture native.
        self._capture_rate = sample_rate

    def _callback(self, indata, _frames, _time, status) -> None:
        if status:
            # Overflows are common on a busy machine and not worth failing over.
            pass
        block = indata[:, 0].copy()
        self._chunks.put(block)
        if self._on_level is not None:
            self._on_level(rms_to_level(float(np.sqrt(np.mean(block ** 2)))))

    def start(self) -> None:
        with self._lock:
            if self._stream is not None:
                return
            while not self._chunks.empty():
                self._chunks.get_nowait()

            self._capture_rate = self.sample_rate
            try:
                self._stream = self._open(self.sample_rate)
            except sd.PortAudioError:
                # Only PipeWire's "default" device resamples for us. A device
                # named explicitly - a monitor source, or an interface locked
                # to 48 kHz - refuses anything else, so capture native and
                # resample ourselves.
                native = int(sd.query_devices(self.device, "input")["default_samplerate"])
                log.info(
                    "device refused %d Hz, capturing at %d Hz and resampling",
                    self.sample_rate, native,
                )
                self._capture_rate = native
                self._stream = self._open(native)

            self._stream.start()

    def _open(self, rate: int) -> sd.InputStream:
        return sd.InputStream(
            samplerate=rate,
            channels=1,
            dtype="float32",
            blocksize=BLOCK_FRAMES,
            device=self.device,
            callback=self._callback,
        )

    def stop(self) -> np.ndarray:
        """Stop and return the whole take as float32 mono."""
        with self._lock:
            if self._stream is None:
                return np.zeros(0, dtype=np.float32)
            self._stream.stop()
            self._stream.close()
            self._stream = None

        blocks = []
        while not self._chunks.empty():
            blocks.append(self._chunks.get_nowait())

        if not blocks:
            return np.zeros(0, dtype=np.float32)

        audio = np.concatenate(blocks).astype(np.float32)
        return resample(audio, self._capture_rate, self.sample_rate)

    @property
    def recording(self) -> bool:
        return self._stream is not None


def trim_silence(audio: np.ndarray, sample_rate: int, threshold: float) -> np.ndarray:
    """Drop leading and trailing silence.

    Push-to-talk always captures the fumble before you start speaking and the
    beat after you stop; both cost recognition time and can confuse the model.
    """
    if audio.size == 0:
        return audio

    window = max(1, sample_rate // 100)  # 10 ms
    usable = (audio.size // window) * window
    if usable == 0:
        return audio

    frames = audio[:usable].reshape(-1, window)
    loud = np.sqrt((frames ** 2).mean(axis=1)) > threshold
    if not loud.any():
        return np.zeros(0, dtype=np.float32)

    first, last = int(np.argmax(loud)), int(len(loud) - np.argmax(loud[::-1]))
    pad = max(1, sample_rate // 20)  # keep 50 ms either side
    start = max(0, first * window - pad)
    end = min(audio.size, last * window + pad)
    return audio[start:end]


def list_devices() -> list[dict]:
    return [d for d in sd.query_devices() if d["max_input_channels"] > 0]
