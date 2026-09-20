"""Configuration, read from ~/.config/flow/config.toml.

Every field has a working default, so the file is optional; `flow config` writes
a commented copy out when you want to change something.
"""

from __future__ import annotations

import os
import tomllib
from dataclasses import dataclass, field, asdict
from pathlib import Path

CONFIG_DIR = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "flow"
CONFIG_PATH = CONFIG_DIR / "config.toml"
DATA_DIR = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share")) / "flow"


@dataclass
class AudioConfig:
    # PortAudio device name substring; empty means the system default. PipeWire
    # resamples whatever the device runs at down to sample_rate for us.
    device: str = ""
    sample_rate: int = 16000
    # Amplitude below this counts as silence when trimming the take.
    silence_rms: float = 0.006
    trim_silence: bool = True
    # Refuse to transcribe takes shorter than this; usually a mis-press.
    min_seconds: float = 0.35
    max_seconds: float = 300.0


@dataclass
class SttConfig:
    # v3 is the multilingual sibling of v2, at the same speed. It detects the
    # language itself - there is nothing to switch when you change language
    # mid-session. Use v2 if you only ever dictate English.
    model: str = "nemo-parakeet-tdt-0.6b-v3"
    # auto tries CUDA and falls back to CPU, which is still ~36x realtime.
    provider: str = "auto"


@dataclass
class CleanupConfig:
    enabled: bool = True
    # ollama (local) | anthropic | openai | none
    backend: str = "ollama"
    model: str = "qwen3:14b"
    endpoint: str = "http://localhost:11434"
    timeout: float = 20.0
    # How long Ollama keeps the model in VRAM after a request. Loading a 14B
    # model costs ~40s, so an idle timeout mid-session is a brutal first
    # dictation. Flow warms the model on start and unloads it on stop, so
    # holding it for the session is the right trade.
    keep_alive: str = "1h"
    # light | balanced | tidy - how much liberty the model takes with wording.
    style: str = "balanced"
    # Delete ideas the speaker abandoned mid-dictation. The only rule that
    # removes content, so it can be switched off.
    resolve_intent: bool = True
    # auto | never | always. Off by default: on the eval corpus reasoning
    # scored identically while taking 3-21s instead of 0.1-0.3s.
    think: str = "never"
    # Languages you actually dictate in. Told to the cleanup model so it knows
    # what to expect and does not drift into English.
    languages: list[str] = field(default_factory=lambda: ["en", "nl"])
    # same | en | nl. "same" answers in whatever you spoke; a language code
    # translates everything into it.
    output_language: str = "same"
    # Words the recogniser tends to get wrong: names, jargon, product names.
    dictionary: list[str] = field(default_factory=list)
    # wm_class -> instruction appended to the prompt for that app.
    app_rules: dict[str, str] = field(default_factory=dict)


@dataclass
class InsertConfig:
    restore_clipboard: bool = True


@dataclass
class LearningConfig:
    """Off by default. With it off Flow keeps no record of what you say."""

    enabled: bool = False
    # Wait for enough signal before drawing conclusions about someone's voice.
    min_dictations: int = 15
    # Re-mine the profile after this many new dictations.
    refresh_every: int = 25
    # Cap on learned terms, because every one lengthens the cleanup prompt.
    max_terms: int = 40


@dataclass
class Config:
    audio: AudioConfig = field(default_factory=AudioConfig)
    stt: SttConfig = field(default_factory=SttConfig)
    cleanup: CleanupConfig = field(default_factory=CleanupConfig)
    insert: InsertConfig = field(default_factory=InsertConfig)
    learning: LearningConfig = field(default_factory=LearningConfig)

    @classmethod
    def load(cls, path: Path | None = None) -> "Config":
        path = path or CONFIG_PATH
        if not path.exists():
            return cls()

        with path.open("rb") as handle:
            raw = tomllib.load(handle)

        def build(klass, key):
            known = {f for f in klass.__dataclass_fields__}
            supplied = {k: v for k, v in raw.get(key, {}).items() if k in known}
            return klass(**supplied)

        return cls(
            audio=build(AudioConfig, "audio"),
            stt=build(SttConfig, "stt"),
            cleanup=build(CleanupConfig, "cleanup"),
            insert=build(InsertConfig, "insert"),
            learning=build(LearningConfig, "learning"),
        )

    def as_dict(self) -> dict:
        return asdict(self)


DEFAULT_TOML = """\
# Flow configuration. Every value here is the built-in default; delete a line
# to go back to it.

[audio]
device = ""            # PortAudio device name substring, empty = system default
sample_rate = 16000
trim_silence = true
silence_rms = 0.006
min_seconds = 0.35

[stt]
# v3 is multilingual and detects the language itself, at the same speed as
# the English-only v2. Switch to "nemo-parakeet-tdt-0.6b-v2" if you only ever
# dictate in English.
model = "nemo-parakeet-tdt-0.6b-v3"
provider = "auto"      # auto | cuda | cpu

[cleanup]
enabled = true
# Where the cleanup model runs.
#   ollama    - local, offline, free, nothing leaves this machine
#   anthropic - Claude via the official SDK  (key in the GNOME keyring)
#   openai    - GPT via the official SDK     (key in the GNOME keyring)
#   none      - paste the raw transcript, no cleanup
backend = "ollama"
model = "qwen3:14b"
endpoint = "http://localhost:11434"
timeout = 20.0
keep_alive = "1h"     # "0" unloads immediately, "-1" never unloads

# How much liberty the model takes with your wording.
#   light    - punctuation and obvious "um"s only, wording untouched
#   balanced - also drops false starts and resolves changes of mind
#   tidy     - also tightens loose grammar
style = "balanced"

# Delete ideas you abandoned mid-sentence ("actually, forget that, what I
# need is..."). The only rule that removes content; set false to keep
# everything you said.
resolve_intent = true

# Let the model reason before answering. Off by default: measured on
# scripts/eval-cleanup.py it scored the same 11/11 either way, but took
# 3-21s instead of 0.1-0.3s. "auto" reasons only on longer transcripts
# containing a retraction cue; "always" reasons on everything.
think = "never"

# The languages you dictate in. The recogniser detects the language on its
# own; this tells the cleanup model what to expect so it does not drift.
languages = ["en", "nl"]

# What language to write out.
#   same - whatever you spoke, cleaned up in that language
#   en   - always English, translating your Dutch
#   nl   - always Dutch, translating your English
output_language = "same"

# Names and jargon the recogniser keeps getting wrong.
dictionary = []

# Per-application tone. Keys are WM classes - run `flow context` with the
# target app focused to find one.
[cleanup.app_rules]
# "org.gnome.Console" = "Output a shell command only, no prose."
# "Slack" = "Casual, no greeting or sign-off."

[insert]
restore_clipboard = true

# Learning is OFF unless you switch it on. While it is off Flow keeps no
# record of anything you dictate.
#
# Switched on, Flow stores your dictations locally and periodically mines two
# things from them with the same local model: the jargon and project names a
# general recogniser gets wrong, and a short note describing how you talk.
# Both are fed back into the cleanup prompt, so the more you use it the more
# it sounds like you.
#
#   flow learning on      start learning and using what it learns
#   flow learning off     stop both; the profile is kept for next time
#   flow vocab            see what it has picked up
#   flow history --clear  delete everything it has stored
[learning]
enabled = false
min_dictations = 15
refresh_every = 25
max_terms = 40
"""


def write_default_config(path: Path | None = None) -> Path:
    path = path or CONFIG_PATH
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        path.write_text(DEFAULT_TOML)
    return path
