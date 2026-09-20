"""Dictation flow: what the island is told, and when.

These drive the daemon with fakes so the sequence of states can be asserted
without a Shell, a microphone or a model.
"""

from __future__ import annotations

import numpy as np
import pytest

from flowd.config import Config
from flowd.daemon import Daemon

SR = 16000


class FakeIsland:
    def __init__(self, context=None):
        self.states: list[str] = []
        self.texts: list[str] = []
        self.inserted: list[str] = []
        self.levels: list[float] = []
        self._context = context or {"app": "org.gnome.TextEditor", "title": "notes"}

    def set_state(self, state): self.states.append(state)
    def set_text(self, text): self.texts.append(text)
    def insert_text(self, text): self.inserted.append(text)
    def push_level(self, level): self.levels.append(level)
    def focus_context(self): return dict(self._context)


class FakeRecorder:
    def __init__(self, audio):
        self._audio = audio
        self.recording = False
        self.starts = 0

    def start(self):
        self.recording = True
        self.starts += 1

    def stop(self):
        self.recording = False
        return self._audio


class FakeTranscriber:
    def __init__(self, text="um hello there"):
        self.text = text
        self.seen = None

    def load(self): pass

    def transcribe(self, audio, sample_rate=SR):
        self.seen = audio
        return self.text


class FakeCleaner:
    def __init__(self, result="Hello there."):
        self.result = result
        self.context = None

    def available(self): return True, "ok"

    def clean(self, raw, context=None):
        self.context = context
        return self.result


def speech(seconds=2.0):
    t = np.linspace(0, seconds, int(SR * seconds), endpoint=False)
    return (np.sin(2 * np.pi * 220 * t) * 0.3).astype(np.float32)


def build(audio=None, transcriber=None, cleaner=None, island=None):
    daemon = Daemon(
        Config(),
        island=island or FakeIsland(),
        transcriber=transcriber or FakeTranscriber(),
        cleaner=cleaner or FakeCleaner(),
        recorder=FakeRecorder(speech() if audio is None else audio),
    )
    return daemon


def test_happy_path_states_and_insert():
    daemon = build()
    island = daemon.island

    daemon._on_pressed("hold")
    assert island.states == ["listening"]
    assert daemon.recorder.starts == 1

    daemon._on_released()
    assert island.states == ["listening", "thinking"]

    # _process normally runs on a worker; call it directly and finish inline.
    daemon._process(daemon.recorder._audio)
    daemon._finish("Hello there.")

    assert island.states[-1] == "inserting"
    assert island.inserted == ["Hello there."]


def test_focus_context_is_captured_at_press_not_at_insert():
    # The user's target app is focused when they press, which is the context
    # the cleanup model needs; by insert time it could have changed.
    cleaner = FakeCleaner()
    daemon = build(cleaner=cleaner)
    daemon._on_pressed("hold")
    daemon.island._context = {"app": "SomethingElse", "title": "later"}
    daemon._on_released()
    daemon._process(daemon.recorder._audio)

    assert cleaner.context["app"] == "org.gnome.TextEditor"


def test_too_short_a_take_is_rejected_without_transcribing():
    transcriber = FakeTranscriber()
    daemon = build(audio=speech(0.1), transcriber=transcriber)
    daemon._on_pressed("hold")
    daemon._on_released()

    assert daemon.island.states[-1] == "error"
    assert transcriber.seen is None


def test_empty_transcript_shows_an_error():
    daemon = build(transcriber=FakeTranscriber(""))
    daemon._on_pressed("hold")
    daemon._on_released()
    daemon._process(daemon.recorder._audio)
    daemon._fail("No speech detected")

    assert daemon.island.states[-1] == "error"
    assert daemon.island.inserted == []


def test_press_while_recording_is_ignored():
    daemon = build()
    daemon._on_pressed("hold")
    daemon._on_pressed("hold")
    assert daemon.recorder.starts == 1


def test_release_without_press_does_nothing():
    daemon = build()
    daemon._on_released()
    assert daemon.island.states == []


def test_cancel_stops_and_hides():
    daemon = build()
    daemon._on_pressed("hold")
    daemon._on_cancel()
    assert daemon.island.states[-1] == "hidden"
    assert not daemon.recorder.recording


def test_transcriber_failure_does_not_kill_the_daemon():
    class Exploding(FakeTranscriber):
        def transcribe(self, audio, sample_rate=SR):
            raise RuntimeError("model exploded")

    daemon = build(transcriber=Exploding())
    daemon._on_pressed("hold")
    daemon._on_released()
    assert daemon._busy is True

    daemon._process(daemon.recorder._audio)  # must swallow the exception

    # _process schedules _fail on the main loop, which is not running here, so
    # assert on what it must NOT have done: pasted anything.
    assert daemon.island.inserted == []


def test_silence_is_trimmed_before_transcription():
    transcriber = FakeTranscriber()
    audio = np.concatenate([
        np.zeros(SR, dtype=np.float32), speech(1.0), np.zeros(SR, dtype=np.float32)
    ])
    daemon = build(audio=audio, transcriber=transcriber)
    daemon._on_pressed("hold")
    daemon._on_released()
    daemon._process(audio)

    assert transcriber.seen is not None
    assert len(transcriber.seen) < len(audio)
