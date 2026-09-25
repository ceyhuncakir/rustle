#!/usr/bin/env python3
"""Screenshot every island state. Run inside scripts/nested-shell.sh with RUSTLE_DEV=1.

Gives the island-first workflow a real feedback loop: change a colour or a
curve, re-run, look at the PNGs.
"""

from __future__ import annotations

import math
import os
import subprocess
import sys
import time
from pathlib import Path

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from flowd.island import IslandClient, IslandUnavailable  # noqa: E402

OUT = Path(os.environ.get("RUSTLE_SHOTS", "/tmp/rustle-shots"))

# The nested monitor is 1600x900; the pill sits bottom-centre. Crop to the
# region it can occupy so the PNGs are readable instead of mostly wallpaper.
CROP = "900x170+350+735"

SPEECH = "Hey, can you take a look at the auth middleware before the deploy?"


def screenshot(bus: Gio.DBusConnection, path: Path) -> bool:
    try:
        ok, _ = bus.call_sync(
            "org.gnome.Shell", "/org/gnome/Shell/Screenshot",
            "org.gnome.Shell.Screenshot", "Screenshot",
            GLib.Variant("(bbs)", (False, False, str(path))),
            GLib.VariantType("(bs)"), Gio.DBusCallFlags.NONE, 10000, None,
        ).unpack()
        return bool(ok)
    except GLib.Error as exc:
        print(f"  screenshot failed: {exc.message}", file=sys.stderr)
        return False


def crop(path: Path) -> None:
    subprocess.run(
        ["magick", str(path), "-crop", CROP, "+repage", str(path)],
        check=False, capture_output=True,
    )


def feed(island: IslandClient, seconds: float) -> None:
    """Push a speech-like envelope so the bars are mid-motion when shot."""
    end = time.monotonic() + seconds
    t0 = time.monotonic()
    while time.monotonic() < end:
        t = time.monotonic() - t0
        level = (0.25 + 0.75 * abs(math.sin(t * 7.0)) ** 1.6) * (0.55 + 0.45 * math.sin(t * 1.1))
        island.push_level(max(0.0, min(1.0, level)))
        time.sleep(1 / 60)


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    try:
        island = IslandClient()
    except IslandUnavailable as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    scenes = [
        ("01-idle",           lambda: (island.set_text(""), island.set_state("idle"))),
        ("02-listening",      lambda: (island.set_text(""), island.set_state("listening"), feed(island, 1.2))),
        ("03-listening-text", lambda: (island.set_text("hey can you take a look at the auth"), feed(island, 0.8))),
        ("04-thinking",       lambda: (island.set_text(""), island.set_state("thinking"))),
        ("05-inserting",      lambda: (island.set_text(SPEECH), island.set_state("inserting"))),
        ("06-error",          lambda: (island.set_text("No speech detected"), island.set_state("error"))),
    ]

    failures = 0
    for name, setup in scenes:
        setup()
        time.sleep(0.9)  # let the morph settle
        path = OUT / f"{name}.png"
        if screenshot(bus, path):
            crop(path)
            print(f"  {path}")
        else:
            failures += 1

    island.set_state("hidden")
    print(f"\n{len(scenes) - failures}/{len(scenes)} captured into {OUT}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
