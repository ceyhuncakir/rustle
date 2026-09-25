#!/usr/bin/env python3
"""Check that the shortcut registers and that its signals reach the daemon.

Run inside scripts/nested-shell.sh with RUSTLE_DEV=1.
"""

from __future__ import annotations

import sys
from pathlib import Path

import gi

gi.require_version("Gio", "2.0")
from gi.repository import GLib  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from flowd.island import IslandClient, IslandUnavailable  # noqa: E402


def main() -> int:
    try:
        island = IslandClient()
    except IslandUnavailable as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    seen: list[str] = []
    loop = GLib.MainLoop()

    island.connect_signals(
        on_pressed=lambda mode: seen.append(f"pressed:{mode}"),
        on_released=lambda: seen.append("released"),
    )

    # Tap once to start, once more to stop - the toggle path.
    GLib.timeout_add(300, lambda: (island.dev_press_hotkey(), False)[1])
    GLib.timeout_add(900, lambda: (island.dev_press_hotkey(), False)[1])
    GLib.timeout_add(1600, lambda: (loop.quit(), False)[1])
    loop.run()

    print("signals:", seen)
    expected = ["pressed:dictating", "released"]
    if seen != expected:
        print(f"error: expected {expected}", file=sys.stderr)
        return 1
    print("hotkey signal round trip OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
