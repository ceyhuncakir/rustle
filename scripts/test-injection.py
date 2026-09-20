#!/usr/bin/env python3
"""Prove text injection works: open an editor, paste into it, screenshot it.

This is the assumption the whole architecture rests on - that the Shell's own
virtual input device can type into an unrelated Wayland client with no root
and no access to /dev/uinput.

Run inside scripts/nested-shell.sh with FLOW_DEV=1.
"""

from __future__ import annotations

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

OUT = Path(os.environ.get("FLOW_SHOTS", "/tmp/flow-shots"))
SENTINEL = "Flow injected this line with no root and no uinput."


class Shell:
    """Eval is only reachable because FLOW_DEV=1 turned on unsafe mode."""

    def __init__(self) -> None:
        self._bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def eval(self, source: str) -> tuple[bool, str]:
        ok, result = self._bus.call_sync(
            "org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell",
            "Eval", GLib.Variant("(s)", (source,)),
            GLib.VariantType("(bs)"), Gio.DBusCallFlags.NONE, 5000, None,
        ).unpack()
        return bool(ok), result

    def screenshot(self, path: Path) -> bool:
        try:
            ok, _ = self._bus.call_sync(
                "org.gnome.Shell", "/org/gnome/Shell/Screenshot",
                "org.gnome.Shell.Screenshot", "Screenshot",
                GLib.Variant("(bbs)", (False, False, str(path))),
                GLib.VariantType("(bs)"), Gio.DBusCallFlags.NONE, 10000, None,
            ).unpack()
            return bool(ok)
        except GLib.Error as exc:
            print(f"screenshot failed: {exc.message}", file=sys.stderr)
            return False

    def activate_window(self, wm_class: str) -> bool:
        ok, _ = self.eval(
            "let a = global.get_window_actors()"
            f".find(a => a.meta_window.get_wm_class() === '{wm_class}');"
            "if (a) a.meta_window.activate(global.get_current_time());"
            "String(!!a)"
        )
        return ok


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    shell = Shell()

    try:
        island = IslandClient()
    except IslandUnavailable as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    # A scratch file, so the editor never reopens something real.
    scratch = OUT / "scratch.txt"
    scratch.write_text("")

    editor = subprocess.Popen(
        ["gnome-text-editor", "--new-window", str(scratch)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    try:
        time.sleep(6)

        # A headless Shell maps windows without giving them seat focus, so the
        # test has to focus the editor itself. A real session does this for us.
        if not shell.activate_window("org.gnome.TextEditor"):
            print("error: editor window never appeared", file=sys.stderr)
            return 1
        time.sleep(1.0)

        # The Overview keeps a keyboard grab even while focus_window reports
        # the editor, and its search entry lives inside gnome-shell - so a
        # paste landing there would prove nothing about crossing processes.
        island.dev_hide_overview()
        time.sleep(1.0)

        context = island.focus_context()
        print(f"focused app:   {context.get('app')!r}")
        print(f"focused title: {context.get('title')!r}")
        if context.get("app") != "org.gnome.TextEditor":
            print("error: editor did not take focus", file=sys.stderr)
            return 1

        island.set_state("inserting")
        island.set_text(SENTINEL)
        island.insert_text(SENTINEL)
        time.sleep(2.5)  # clipboard round trip, then paste

        shot = OUT / "07-injection.png"
        if not shell.screenshot(shot):
            return 1
        print(f"screenshot:    {shot}")
        return 0
    finally:
        editor.terminate()
        try:
            editor.wait(timeout=5)
        except subprocess.TimeoutExpired:
            editor.kill()


if __name__ == "__main__":
    sys.exit(main())
