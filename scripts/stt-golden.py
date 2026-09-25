#!/usr/bin/env python3
"""Generate the speech-recognition parity fixtures for crates/rustle-stt.

Run from the repository root with the project venv:

    .venv/bin/python scripts/stt-golden.py [--no-synth | --synth-only] [--no-cuda] [--no-daemon] [--daemon-python PY]

It synthesises tests/fixtures/stt/*.wav with espeak-ng (English and Dutch
sentences, silence, a very short clip, a 60 s concatenation, a clipped
sentence and one rendered at 48 kHz), then records what onnx-asr 0.12
produces for each - token ids, encoder frame indices and text - using the
exact graphs the Rust crate runs: the model files in Rustle's data dir
(int8 encoder + int8 decoder_joint on the CPU; fp32 on CUDA when available)
with the packaged `nemo128.onnx` preprocessor on the CPU. The features for
one short file are stored too, so the Rust preprocessor session can be
checked numerically.

Requires the model in Rustle's data dir: `import_from_hf_cache` plus a
`download(..., Precision::Int8)` from the Rust side, or the equivalent.

Run it with an onnxruntime of the same minor version as the one the `ort`
crate links (ORT_MINOR below; see ort-sys' dist table for the exact build).
The int8 CPU kernels changed between onnxruntime 1.22 and 1.24: a golden
from the daemon's own 1.22 venv differs from the crate's output on about
half the files, by a frame index or the odd token, while fp32 on CUDA is
identical across both. A scratch venv does:

    uv venv --python 3.13 /tmp/ort124
    uv pip install --python /tmp/ort124/bin/python "onnxruntime-gpu==1.24.*" "onnx-asr==0.12.0" numpy
    LD_LIBRARY_PATH=.venv/lib/python3.13/site-packages/nvidia/cudnn/lib:/usr/local/cuda-12.9/lib64 \
        /tmp/ort124/bin/python scripts/stt-golden.py

The `daemon_text` entries are what flowd/stt.py itself says (auto provider),
run in `--daemon-python` (the project venv, say) or in this interpreter;
they are informative only.

The golden records `onnxruntime.get_build_info()`. The Rust parity test
compares it with `ort::info()`: the same build must reproduce the golden
bit for bit, a different build (pyke's binaries versus Microsoft's wheel)
is held to a tolerance, since int8 dynamic quantisation turns 1e-6 float
differences into the odd token. To run the strict check on the wheel's
runtime:

    ORT_DYLIB_PATH=/tmp/ort124/lib/python3.13/site-packages/onnxruntime/capi/libonnxruntime.so.1.24.4 \
        cargo test -p rustle-stt --features ort/load-dynamic --test parity
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import wave
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
FIXTURES = ROOT / "tests" / "fixtures" / "stt"
MODEL_ID = "nemo-parakeet-tdt-0.6b-v3"
NEMO128_SHA256 = "5b4a84c52eeaa615dc46d781cc7e4598f9b432184831d57493c48adcd01371a9"
# The onnxruntime minor the `ort` crate pinned in crates/rustle-stt/Cargo.toml links.
ORT_MINOR = "1.24"
SAMPLE_RATE = 16_000

EN = [
    "The quick brown fox jumps over the lazy dog.",
    "Please schedule the meeting for Tuesday at half past nine.",
    "I think we should ship the new version before the end of the month.",
    "Can you send me the report? I need it by tomorrow morning.",
    "Twenty-three people signed up for the workshop, which is more than we expected.",
    "Turn left at the second traffic light and the station is on your right.",
    "She said the results were surprising, but nobody was really convinced.",
    "Let's add error handling around the network call and retry three times.",
    "The weather forecast promises rain in the afternoon and sunshine on Sunday.",
    "My phone number changed last week, so please update your contacts.",
    "Honestly, the second draft reads much better than the first one did.",
    "We arrived in Amsterdam at noon and walked along the canals until dinner.",
]

NL = [
    "Goedemorgen, ik wil graag een afspraak maken voor volgende week.",
    "De trein naar Utrecht vertrekt over tien minuten van spoor vier.",
    "Kun je me het verslag sturen voordat de vergadering begint?",
    "Het regent al de hele dag, dus we blijven vanavond gewoon thuis.",
    "Mijn zus woont in Rotterdam en werkt bij een klein softwarebedrijf.",
    "Vergeet niet om morgen de boodschappen te doen en de planten water te geven.",
]


def data_dir() -> Path:
    if env := os.environ.get("RUSTLE_DATA_DIR"):
        return Path(env)
    base = os.environ.get("XDG_DATA_HOME")
    return (Path(base) if base else Path.home() / ".local" / "share") / "rustle"


MODEL_DIR = data_dir() / "models" / MODEL_ID


# --- audio ---------------------------------------------------------------


def read_wav(path: Path) -> tuple[np.ndarray, int]:
    with wave.open(str(path), "rb") as f:
        assert f.getnchannels() == 1 and f.getsampwidth() == 2, path
        data = np.frombuffer(f.readframes(f.getnframes()), dtype="<i2")
        return data, f.getframerate()


def write_wav(path: Path, samples: np.ndarray, rate: int) -> None:
    assert samples.dtype == np.int16
    with wave.open(str(path), "wb") as f:
        f.setnchannels(1)
        f.setsampwidth(2)
        f.setframerate(rate)
        f.writeframes(samples.tobytes())


def to_float(samples: np.ndarray) -> np.ndarray:
    """Exactly what the Rust side does with 16-bit PCM."""
    return (samples.astype(np.float32) / np.float32(32768.0)).astype(np.float32)


def espeak(text: str, voice: str, out: Path, rate: int = SAMPLE_RATE, speed: int = 150, pitch: int = 50) -> None:
    with tempfile.TemporaryDirectory() as tmp:
        raw = Path(tmp) / "raw.wav"
        subprocess.run(
            ["espeak-ng", "-v", voice, "-s", str(speed), "-p", str(pitch), "-w", str(raw), text],
            check=True,
        )
        subprocess.run(
            [
                "ffmpeg", "-y", "-loglevel", "error", "-i", str(raw),
                "-ar", str(rate), "-ac", "1", "-c:a", "pcm_s16le",
                "-map_metadata", "-1", "-fflags", "+bitexact", "-flags:a", "+bitexact",
                str(out),
            ],
            check=True,
        )


def synthesise() -> None:
    FIXTURES.mkdir(parents=True, exist_ok=True)
    for old in FIXTURES.glob("*.wav"):
        old.unlink()

    clips: list[np.ndarray] = []
    for i, text in enumerate(EN, 1):
        voice = ["en-us", "en-gb", "en-us+f3", "en-gb+m4"][i % 4]
        path = FIXTURES / f"en-{i:02d}.wav"
        espeak(text, voice, path, speed=140 + (i % 3) * 15, pitch=40 + (i % 4) * 8)
        clips.append(read_wav(path)[0])
    for i, text in enumerate(NL, 1):
        voice = ["nl", "nl+f2", "nl+m3"][i % 3]
        path = FIXTURES / f"nl-{i:02d}.wav"
        espeak(text, voice, path, speed=145 + (i % 2) * 10, pitch=45 + (i % 3) * 10)
        clips.append(read_wav(path)[0])

    write_wav(FIXTURES / "silence-1s.wav", np.zeros(SAMPLE_RATE, dtype=np.int16), SAMPLE_RATE)

    with tempfile.TemporaryDirectory() as tmp:
        yes = Path(tmp) / "yes.wav"
        espeak("Yes.", "en-us", yes, speed=170)
        samples = read_wav(yes)[0][: int(0.4 * SAMPLE_RATE)]
        samples = np.pad(samples, (0, int(0.4 * SAMPLE_RATE) - len(samples)))
        write_wav(FIXTURES / "short-0.4s.wav", samples, SAMPLE_RATE)

    gap = np.zeros(int(0.3 * SAMPLE_RATE), dtype=np.int16)
    pieces: list[np.ndarray] = []
    total = 0
    while total < 60 * SAMPLE_RATE:
        for clip in clips:
            pieces += [clip, gap]
            total += len(clip) + len(gap)
            if total >= 60 * SAMPLE_RATE:
                break
    write_wav(FIXTURES / "long-60s.wav", np.concatenate(pieces)[: 60 * SAMPLE_RATE], SAMPLE_RATE)

    loud = np.clip(to_float(read_wav(FIXTURES / "en-05.wav")[0]) * 8.0, -1.0, 32767 / 32768)
    write_wav(FIXTURES / "clipped.wav", np.round(loud * 32768).astype(np.int16), SAMPLE_RATE)

    espeak(EN[0], "en-us", FIXTURES / "en-48k.wav", rate=48_000)


# --- golden --------------------------------------------------------------


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def model_view() -> Path:
    """A directory onnx-asr can load: Rustle's model files plus the config.json
    it needs to pick the 128-mel preprocessor."""
    missing = [f for f in ("vocab.txt", "nemo128.onnx", "encoder-model.int8.onnx", "decoder_joint-model.int8.onnx")
               if not (MODEL_DIR / f).is_file()]
    if missing:
        sys.exit(f"model files missing in {MODEL_DIR}: {missing} (import_from_hf_cache + download int8 first)")
    actual = sha256(MODEL_DIR / "nemo128.onnx")
    if actual != NEMO128_SHA256:
        sys.exit(f"{MODEL_DIR / 'nemo128.onnx'} has sha256 {actual}, expected {NEMO128_SHA256}")
    view = Path(tempfile.mkdtemp(prefix="rustle-stt-golden-"))
    for f in MODEL_DIR.iterdir():
        if f.is_file():
            os.symlink(f, view / f.name)
    (view / "config.json").write_text(
        json.dumps({"model_type": "nemo-conformer-tdt", "features_size": 128, "subsampling_factor": 8})
    )
    return view


def load(view: Path, providers: list[str], quantization: str | None):
    import onnx_asr

    # The packaged nemo128.onnx on the CPU, not the NumPy or Conv variants
    # onnx-asr would otherwise pick, so Python runs exactly the Rust graphs.
    return onnx_asr.load_model(
        MODEL_ID,
        str(view),
        quantization=quantization,
        providers=providers,
        preprocessor_config={
            "providers": ["CPUExecutionProvider"],
            "use_numpy_preprocessors": False,
            "use_conv_preprocessors": False,
        },
        resampler_config={"providers": ["CPUExecutionProvider"]},
    )


def preprocessor_of(model):
    pre = model.asr._preprocessor
    return getattr(pre, "_preprocessor", None)


def run(model, audio: np.ndarray, rate: int) -> dict:
    """The adapter's recognize(), taken apart to keep ids and frame indices."""
    from onnx_asr.utils import read_wav_files

    asr = model.asr
    waveforms, lens, sr = read_wav_files(audio, rate)
    waveforms, lens = model.resampler(waveforms, lens, sr)
    features, feature_lens = asr._preprocessor(waveforms, lens)
    encoded, encoded_lens = asr._encode(features, feature_lens)
    tokens, frames, _ = next(asr._decoding(encoded, encoded_lens))
    text = asr._decode_tokens(tokens, frames, None).text.strip()
    public = model.recognize(audio, sample_rate=rate).strip()
    assert public == text, (public, text)
    return {"text": text, "tokens": [int(t) for t in tokens], "frames": [int(f) for f in frames]}


def mean_of_three(audio48: np.ndarray) -> np.ndarray:
    """rustle_core::dsp::resample for a 3:1 ratio, operation for operation."""
    n = len(audio48) // 3 * 3
    y = audio48[:n].reshape(-1, 3)
    return (((y[:, 0] + y[:, 1]) + y[:, 2]) / np.float32(3.0)).astype(np.float32)


def features_of(model, audio: np.ndarray) -> dict:
    pre = preprocessor_of(model)
    assert pre is not None, "expected the ONNX preprocessor"
    features, lens = pre.run(
        ["features", "features_lens"],
        {"waveforms": audio[None, :], "waveforms_lens": np.array([len(audio)], dtype=np.int64)},
    )
    return {
        "shape": list(features.shape),
        "valid": int(lens[0]),
        "data": [round(float(x), 7) for x in features.reshape(-1)],
    }


def cuda_model(view: Path):
    import onnxruntime as ort

    from flowd.stt import _preload_cuda_libraries

    _preload_cuda_libraries()
    if "CUDAExecutionProvider" not in ort.get_available_providers():
        print("no CUDAExecutionProvider in onnxruntime, skipping the CUDA golden")
        return None
    model = load(view, ["CUDAExecutionProvider", "CPUExecutionProvider"], None)
    if model.asr._encoder.get_providers()[0] != "CUDAExecutionProvider":
        print("the encoder did not land on CUDA, skipping the CUDA golden")
        return None
    return model


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-synth", action="store_true", help="keep the existing wav files")
    parser.add_argument("--synth-only", action="store_true", help="only (re)generate the wav files, which are git-ignored")
    parser.add_argument("--no-cuda", action="store_true")
    parser.add_argument("--no-daemon", action="store_true", help="skip flowd.stt's own text")
    parser.add_argument("--daemon-python", metavar="PY", help="interpreter to run flowd.stt in (default: this one)")
    args = parser.parse_args()

    if not args.no_synth:
        synthesise()
    if args.synth_only:
        print(f"wrote {len(list(FIXTURES.glob('*.wav')))} wav files under {FIXTURES}")
        return
    wavs = sorted(FIXTURES.glob("*.wav"))
    if not wavs:
        sys.exit("no fixtures")

    import onnx_asr
    import onnxruntime as ort

    if not ort.__version__.startswith(ORT_MINOR + "."):
        print(
            f"warning: onnxruntime {ort.__version__} but the ort crate links {ORT_MINOR}.x; "
            "int8 CPU results may differ from the crate's by a frame index or a token",
            file=sys.stderr,
        )
    view = model_view()
    cpu = load(view, ["CPUExecutionProvider"], "int8")
    cuda = None if args.no_cuda else cuda_model(view)
    daemon_texts: dict[str, str] | None = None
    daemon_meta = None
    if not args.no_daemon:
        daemon_texts, daemon_meta = daemon_reference(wavs, args.daemon_python)

    files: dict[str, dict] = {}
    for path in wavs:
        samples, rate = read_wav(path)
        audio = to_float(samples)
        entry: dict = {"sample_rate": rate, "seconds": round(len(audio) / rate, 3)}
        if rate == SAMPLE_RATE:
            entry["cpu"] = timed(cpu, audio, rate, f"{path.name} cpu")
            if cuda is not None:
                entry["cuda"] = timed(cuda, audio, rate, f"{path.name} cuda")
        else:
            # The Rust side resamples with rustle_core::dsp before recognising;
            # the golden is defined on the same samples. onnx-asr's own
            # resampler result is kept for reference.
            assert rate == 3 * SAMPLE_RATE, rate
            audio16 = mean_of_three(audio)
            entry["cpu"] = timed(cpu, audio16, SAMPLE_RATE, f"{path.name} cpu (mean-of-3 to 16 kHz)")
            entry["cpu_onnx_asr_resampler"] = timed(cpu, audio, rate, f"{path.name} cpu (onnx-asr resampler)")
            if cuda is not None:
                entry["cuda"] = timed(cuda, audio16, SAMPLE_RATE, f"{path.name} cuda (mean-of-3 to 16 kHz)")
        if "cuda" not in entry:
            entry["cuda"] = None
        if daemon_texts is not None:
            entry["daemon_text"] = daemon_texts[path.name]
        files[path.name] = entry

    shortest = min((p for p in wavs if p.name.startswith("en-") and "48k" not in p.name),
                   key=lambda p: p.stat().st_size)
    features_file = f"features-{shortest.stem}.json"
    features = features_of(cpu, to_float(read_wav(shortest)[0]))
    (FIXTURES / features_file).write_text(json.dumps({"file": shortest.name, **features}, separators=(",", ":")))

    golden = {
        "model": MODEL_ID,
        "onnx_asr": onnx_asr.__version__ if hasattr(onnx_asr, "__version__") else "0.12.0",
        "onnxruntime": ort.__version__,
        "build_info": ort.get_build_info(),
        "numpy": np.__version__,
        "nemo128_sha256": NEMO128_SHA256,
        "cpu": {"precision": "int8", "providers": ["CPUExecutionProvider"]},
        "cuda": None if cuda is None else {"precision": "fp32", "providers": ["CUDAExecutionProvider", "CPUExecutionProvider"]},
        "daemon": daemon_meta,
        "features_file": features_file,
        "files": files,
    }
    (FIXTURES / "golden.json").write_text(json.dumps(golden, indent=1, ensure_ascii=False) + "\n")
    shutil.rmtree(view, ignore_errors=True)

    total = sum(p.stat().st_size for p in FIXTURES.iterdir())
    print(f"wrote {len(files)} fixtures, {total / 1e6:.1f} MB under {FIXTURES}")


DAEMON_SNIPPET = """
import json, sys
sys.path.insert(0, sys.argv[1])
import numpy as np, onnxruntime as ort
from flowd.stt import Transcriber
sys.path.insert(0, sys.argv[2])
import importlib.util
spec = importlib.util.spec_from_file_location("golden", sys.argv[3]); g = importlib.util.module_from_spec(spec); spec.loader.exec_module(g)
t = Transcriber(g.MODEL_ID, provider="auto"); t.load()
out = {}
for path in sys.argv[4:]:
    samples, rate = g.read_wav(g.Path(path))
    out[g.Path(path).name] = t.transcribe(g.to_float(samples), rate)
print(json.dumps({"texts": out, "meta": {"provider": "auto", "onnxruntime": ort.__version__, "python": sys.executable}}))
"""


def daemon_reference(wavs: list[Path], python: str | None) -> tuple[dict[str, str], dict]:
    """flowd.stt's own output for each file, as the daemon would produce it."""
    argv = [python or sys.executable, "-c", DAEMON_SNIPPET, str(ROOT), str(ROOT / "scripts"), str(Path(__file__).resolve()), *map(str, wavs)]
    result = subprocess.run(argv, check=True, capture_output=True, text=True)
    payload = json.loads(result.stdout.strip().splitlines()[-1])
    return payload["texts"], payload["meta"]


def timed(model, audio: np.ndarray, rate: int, label: str) -> dict:
    started = time.monotonic()
    result = run(model, audio, rate)
    elapsed = time.monotonic() - started
    print(f"{label}: {len(audio) / rate:.1f}s in {elapsed:.2f}s -> {result['text']!r}")
    return result


if __name__ == "__main__":
    main()
