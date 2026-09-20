#!/usr/bin/env python3
"""Drive the island through a full dictation cycle with synthetic audio.

This is the island-first development loop: it exercises every state and
transition over the same D-Bus surface the real daemon will use, so nothing
here is throwaway scaffolding.
"""

from __future__ import annotations

import math
import random
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from flowd.island import IslandClient, IslandUnavailable  # noqa: E402

FRAME = 1 / 60

PARTIALS = [
    "hey can you",
    "hey can you take a look",
    "hey can you take a look at the auth",
    "hey can you take a look at the auth middleware before",
    "hey can you take a look at the auth middleware before the deploy",
]

FINAL = "Hey, can you take a look at the auth middleware before the deploy?"


def speech_envelope(t: float) -> float:
    """Amplitude that looks like speech: syllable bursts riding a slow phrase
    contour, with pauses between words and a little noise."""
    phrase = 0.5 + 0.5 * math.sin(t * 0.7)
    syllable = abs(math.sin(t * 7.5)) ** 1.8
    pause = 0.0 if (t * 1.3) % 4.0 > 3.4 else 1.0
    return min(1.0, (0.18 + 0.82 * syllable) * phrase * pause + random.uniform(0, 0.06))


def hold(island: IslandClient, seconds: float, *, levels: bool, t0: float) -> None:
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if levels:
            island.push_level(speech_envelope(time.monotonic() - t0))
        time.sleep(FRAME)


def main() -> int:
    try:
        island = IslandClient()
    except IslandUnavailable as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    print("idle ...")
    island.set_text("")
    island.set_state("idle")
    hold(island, 1.0, levels=False, t0=0)

    print("listening ...")
    island.set_state("listening")
    t0 = time.monotonic()
    hold(island, 1.2, levels=True, t0=t0)

    for partial in PARTIALS:
        island.set_text(partial)
        hold(island, 0.55, levels=True, t0=t0)

    print("thinking ...")
    island.set_text("")
    island.set_state("thinking")
    hold(island, 1.4, levels=False, t0=t0)

    print("inserting ...")
    island.set_text(FINAL)
    island.set_state("inserting")
    time.sleep(2.2)

    print("done - island auto-hides after the success state")
    return 0


if __name__ == "__main__":
    sys.exit(main())
