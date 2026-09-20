"""The daemon: hotkey in, pasted text out.

Owns audio and the models; owns no pixels. Everything visible happens in the
Shell extension, which this drives over D-Bus.
"""

from __future__ import annotations

import logging
import threading
import time

from gi.repository import GLib

from .audio import Recorder, trim_silence
from .cleanup import build_cleaner
from .config import Config
from .history import History
from .island import IslandClient
from .learning import Learner, load_profile
from .stt import Transcriber

log = logging.getLogger(__name__)

# The island pushes ~60 levels/sec; D-Bus copes fine but there is no point
# sending more than the waveform can show.
LEVEL_INTERVAL = 1 / 50


class Daemon:
    def __init__(
        self,
        config: Config,
        *,
        island=None,
        transcriber=None,
        cleaner=None,
        recorder=None,
    ) -> None:
        # The collaborators are injectable so the dictation logic can be tested
        # without a Shell, a microphone or a model.
        self.config = config
        self.island = island if island is not None else IslandClient()
        self.transcriber = transcriber if transcriber is not None else Transcriber(
            config.stt.model, config.stt.provider
        )
        self.cleaner = cleaner if cleaner is not None else build_cleaner(config.cleanup)

        self._last_level = 0.0
        self.recorder = recorder if recorder is not None else Recorder(
            sample_rate=config.audio.sample_rate,
            device=config.audio.device or None,
            on_level=self._on_level,
        )

        # Learning is opt-in. With it off nothing is stored and no profile is
        # loaded, so Flow behaves exactly as it did before the feature existed.
        self.history = History() if config.learning.enabled else None
        self.learner = (
            Learner(config.cleanup.endpoint, config.cleanup.model)
            if config.learning.enabled else None
        )
        self._since_refresh = 0
        self._learning_now = False

        self._busy = False
        self._context: dict[str, str] = {}
        self._started_at = 0.0
        self._processing_at = 0.0
        self._last_raw = ""
        self._loop = GLib.MainLoop()

    # -- audio callback thread ----------------------------------------------

    def _on_level(self, level: float) -> None:
        now = time.monotonic()
        if now - self._last_level < LEVEL_INTERVAL:
            return
        self._last_level = now
        self.island.push_level(level)

    # -- hotkey handlers, on the main loop ----------------------------------

    def _on_pressed(self, mode: str = "toggle") -> None:
        if self._busy or self.recorder.recording:
            return

        # Capture context now: this is when the user's target app is focused.
        try:
            self._context = self.island.focus_context()
        except Exception as exc:  # noqa: BLE001
            log.warning("could not read focus context: %s", exc)
            self._context = {}

        log.info("recording (%s) into %s", mode, self._context.get("app") or "?")
        self._started_at = time.monotonic()
        self.island.set_text("")
        self.island.set_state("listening")
        self.recorder.start()

    def _on_released(self) -> None:
        if not self.recorder.recording:
            return

        audio = self.recorder.stop()
        seconds = audio.size / self.config.audio.sample_rate
        log.info("captured %.2fs", seconds)

        if seconds < self.config.audio.min_seconds:
            self._fail("Too short")
            return

        self._busy = True
        self._processing_at = time.monotonic()
        self.island.set_state("thinking")
        threading.Thread(target=self._process, args=(audio,), daemon=True).start()

    def _on_cancel(self) -> None:
        if self.recorder.recording:
            self.recorder.stop()
        self._busy = False
        self.island.set_state("hidden")

    # -- worker thread -------------------------------------------------------

    def _process(self, audio) -> None:
        try:
            if self.config.audio.trim_silence:
                audio = trim_silence(
                    audio, self.config.audio.sample_rate, self.config.audio.silence_rms
                )

            raw = self.transcriber.transcribe(audio, self.config.audio.sample_rate)
            self._last_raw = raw
            log.info("raw: %r", raw)

            if not raw:
                GLib.idle_add(self._fail, "No speech detected")
                return

            text = self.cleaner.clean(raw, self._context)
            log.info("clean: %r", text)
            GLib.idle_add(self._finish, text)
        except Exception as exc:  # noqa: BLE001 - a crash here must not kill the daemon
            log.exception("dictation failed")
            GLib.idle_add(self._fail, str(exc)[:80])

    # -- back on the main loop ----------------------------------------------

    def _remember(self, raw: str, text: str) -> None:
        """Store the dictation and re-mine the profile when enough have piled
        up. Runs on a worker so the paste is never waiting on it."""
        if self.history is None:
            return
        try:
            self.history.record(raw, text, self._context)
        except Exception as exc:  # noqa: BLE001 - learning must not break dictation
            log.warning("could not record dictation: %s", exc)
            return

        self._since_refresh += 1
        cfg = self.config.learning
        if self._learning_now or self._since_refresh < cfg.refresh_every:
            return
        if self.history.count() < cfg.min_dictations:
            return

        self._since_refresh = 0
        self._learning_now = True
        threading.Thread(target=self._refresh_profile, daemon=True).start()

    def _refresh_profile(self) -> None:
        try:
            terms, style = self.learner.refresh(self.history, self.config.learning.max_terms)
            if terms or style:
                self.cleaner.set_profile(terms, style)
        except Exception as exc:  # noqa: BLE001
            log.warning("profile refresh failed: %s", exc)
        finally:
            self._learning_now = False

    def _finish(self, text: str) -> bool:
        self._busy = False
        if not text:
            return self._fail("Nothing to insert")

        # Report the wait the user actually feels - from letting go of the key
        # to text appearing. Timing from the keypress just measures how long
        # they spoke, which says nothing about how fast Flow is.
        now = time.monotonic()
        log.info(
            "inserted in %.2fs (spoke %.1fs)",
            now - self._processing_at, self._processing_at - self._started_at,
        )
        self.island.set_text(text)
        self.island.set_state("inserting")
        self.island.insert_text(text)

        if self.history is not None:
            threading.Thread(
                target=self._remember, args=(self._last_raw, text), daemon=True
            ).start()
        return GLib.SOURCE_REMOVE

    def _fail(self, message: str) -> bool:
        self._busy = False
        log.warning("failed: %s", message)
        self.island.set_text(message)
        self.island.set_state("error")
        # The error state has no auto-hide; clear it after a beat.
        GLib.timeout_add(2500, lambda: (self.island.set_state("hidden"), GLib.SOURCE_REMOVE)[1])
        return GLib.SOURCE_REMOVE

    # -- lifecycle -----------------------------------------------------------

    def run(self) -> int:
        ok, why = self.cleaner.available()
        if not ok:
            log.warning("cleanup unavailable (%s) - transcripts will paste raw", why)

        log.info("loading %s ...", self.config.stt.model)
        self.transcriber.load()

        # Both models are warmed here so the first dictation is as fast as the
        # hundredth. A cold cleanup model costs ~40s; nobody should meet that
        # mid-sentence.
        if ok:
            self.cleaner.warm_up()

        if self.history is not None:
            terms, style = load_profile(self.history)
            self.cleaner.set_profile(terms, style)
            log.info(
                "learning on: %d dictations stored, %d terms learned",
                self.history.count(), len(terms),
            )

        self.island.connect_signals(self._on_pressed, self._on_released, self._on_cancel)
        log.info("ready - press the dictation hotkey")

        try:
            self._loop.run()
        except KeyboardInterrupt:
            pass
        finally:
            if self.recorder.recording:
                self.recorder.stop()
            self.island.set_state("hidden")
            # Hand the GPU back; a 9 GB model should not outlive the daemon.
            self.cleaner.unload()
        return 0

    def dictate_once(self, seconds: float) -> str:
        """Record for a fixed time and insert. Used by `flow dictate` to test
        the whole path without a working hotkey."""
        self._on_pressed("manual")
        time.sleep(seconds)

        audio = self.recorder.stop()
        self.island.set_state("thinking")

        if self.config.audio.trim_silence:
            audio = trim_silence(
                audio, self.config.audio.sample_rate, self.config.audio.silence_rms
            )

        raw = self.transcriber.transcribe(audio, self.config.audio.sample_rate)
        if not raw:
            self._fail("No speech detected")
            return ""

        text = self.cleaner.clean(raw, self._context)
        self.island.set_text(text)
        self.island.set_state("inserting")
        self.island.insert_text(text)
        return text
