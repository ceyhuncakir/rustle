"""Speech recognition.

Parakeet TDT 0.6B v2 through onnx-asr: English-only, and the fastest accurate
model available. Going through ONNX rather than NeMo keeps the dependency
footprint to numpy plus onnxruntime instead of several gigabytes of torch.
"""

from __future__ import annotations

import ctypes
import logging
import site
import time
from pathlib import Path

import numpy as np

log = logging.getLogger(__name__)

# Downloaded from Hugging Face on first use and cached under ~/.cache/huggingface.
DEFAULT_MODEL = "nemo-parakeet-tdt-0.6b-v3"

# The recognisers worth offering. onnx-asr supports more, but the rest are
# single-language models for languages this user does not dictate in.
MODELS = {
    "nemo-parakeet-tdt-0.6b-v3": (
        "Parakeet TDT v3",
        "English and Dutch, detected automatically",
    ),
    "nemo-parakeet-tdt-0.6b-v2": (
        "Parakeet TDT v2",
        "English only, marginally better on English",
    ),
    "nemo-canary-1b-v2": (
        "Canary 1B v2",
        "More accurate, but twice the size and slower",
    ),
    "whisper-base": (
        "Whisper base",
        "Small and fast, noticeably less accurate",
    ),
}


def is_downloaded(model: str) -> bool:
    """Whether the weights are already in the Hugging Face cache.

    The GUI shows this so picking a model does not silently start a several
    hundred megabyte download the user did not ask for.
    """
    from pathlib import Path

    cache = Path.home() / ".cache/huggingface/hub"
    if not cache.is_dir():
        return False
    stem = model.split("/")[-1].replace("nemo-", "")
    return any(stem in d.name for d in cache.iterdir() if d.is_dir())


def _preload_cuda_libraries() -> None:
    """Make onnxruntime able to find cuDNN and cuBLAS from the venv.

    The pip nvidia-* wheels drop their shared objects under
    site-packages/nvidia/*/lib, which is on no loader search path. PyTorch
    solves this by dlopen-ing them at import; onnxruntime does not, so it
    reports "Failed to load libonnxruntime_providers_cuda.so" and silently
    falls back to CPU. Opening them RTLD_GLOBAL first puts the symbols where
    the provider can find them.
    """
    roots = [Path(p) / "nvidia" for p in site.getsitepackages()]
    # Order matters: cuDNN links against cuBLAS.
    for subdir, pattern in (("cublas", "libcublas*.so.12"),
                            ("cuda_nvrtc", "libnvrtc*.so.12"),
                            ("cudnn", "libcudnn*.so.9")):
        for root in roots:
            for lib in sorted((root / subdir / "lib").glob(pattern)):
                try:
                    ctypes.CDLL(str(lib), mode=ctypes.RTLD_GLOBAL)
                except OSError as exc:
                    log.debug("could not preload %s: %s", lib.name, exc)


def _providers(preference: str) -> list[str] | None:
    import onnxruntime as ort

    if preference != "cpu":
        _preload_cuda_libraries()

    available = ort.get_available_providers()

    if preference == "cpu":
        return ["CPUExecutionProvider"]
    if preference == "cuda":
        if "CUDAExecutionProvider" not in available:
            raise RuntimeError(
                "provider = 'cuda' but onnxruntime has no CUDA provider; "
                "install the gpu extra, or set provider = 'auto'"
            )
        return ["CUDAExecutionProvider", "CPUExecutionProvider"]

    # auto: prefer CUDA, but CPU is still around 36x realtime - usable, not fatal.
    if "CUDAExecutionProvider" in available:
        return ["CUDAExecutionProvider", "CPUExecutionProvider"]
    log.warning("no CUDA execution provider, falling back to CPU")
    return ["CPUExecutionProvider"]


class Transcriber:
    def __init__(self, model: str = DEFAULT_MODEL, provider: str = "auto") -> None:
        self.model_name = model
        self.provider = provider
        self._model = None

    def load(self) -> None:
        """Load the model. Slow on first run - it downloads a few hundred MB -
        so the daemon does this at startup rather than at the first keypress."""
        if self._model is not None:
            return

        import onnx_asr

        started = time.monotonic()
        self._model = onnx_asr.load_model(self.model_name, providers=_providers(self.provider))
        log.info("loaded %s in %.1fs", self.model_name, time.monotonic() - started)
        self._warm_up()

    def _warm_up(self) -> None:
        """Run one throwaway inference.

        The first call compiles CUDA kernels and costs around 0.3s against
        0.02s afterwards. Pay it at startup rather than on the user's first
        keypress.
        """
        try:
            started = time.monotonic()
            noise = (np.random.default_rng(0).standard_normal(16000) * 1e-4).astype(np.float32)
            self._model.recognize(noise, sample_rate=16000)
            log.info("warm-up took %.2fs", time.monotonic() - started)
        except Exception as exc:  # noqa: BLE001 - warm-up is an optimisation
            log.debug("warm-up failed: %s", exc)

    @property
    def loaded(self) -> bool:
        return self._model is not None

    def transcribe(self, audio: np.ndarray, sample_rate: int = 16000) -> str:
        if self._model is None:
            self.load()
        if audio.size == 0:
            return ""

        started = time.monotonic()
        text = self._model.recognize(audio, sample_rate=sample_rate)
        elapsed = time.monotonic() - started

        seconds = audio.size / sample_rate
        log.info(
            "transcribed %.1fs of audio in %.2fs (%.0fx realtime)",
            seconds, elapsed, seconds / elapsed if elapsed else 0,
        )
        return (text or "").strip()
