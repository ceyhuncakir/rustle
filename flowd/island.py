"""D-Bus client for the Shell-side island.

flowd never draws anything itself; it reports state to the extension, which
owns the only actor that can float above other windows on Wayland.
"""

from __future__ import annotations

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

BUS_NAME = "ai.flow.Island"
OBJECT_PATH = "/ai/flow/Island"

STATES = ("hidden", "idle", "listening", "thinking", "inserting", "error")


class IslandUnavailable(RuntimeError):
    """The Shell extension is not loaded, so there is nothing to draw on."""


class IslandClient:
    def __init__(self) -> None:
        self._bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        # Fail loudly at construction rather than silently dropping every
        # later call: a missing extension is a setup error, not a runtime one.
        if not self._name_has_owner():
            raise IslandUnavailable(
                f"{BUS_NAME} is not on the session bus - is the Flow "
                f"extension enabled? (gnome-extensions enable flow@ceyhun.dev)"
            )

    def _name_has_owner(self) -> bool:
        reply = self._bus.call_sync(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            GLib.Variant("(s)", (BUS_NAME,)),
            GLib.VariantType("(b)"),
            Gio.DBusCallFlags.NONE,
            1000,
            None,
        )
        return bool(reply.unpack()[0])

    def _call(self, method: str, args: GLib.Variant | None = None) -> None:
        """Fire and forget. Level pushes run at frame rate, so a blocking
        round trip per sample would show up as jitter in the waveform."""
        self._bus.call(
            BUS_NAME, OBJECT_PATH, BUS_NAME, method, args, None,
            Gio.DBusCallFlags.NO_AUTO_START, 500, None, None, None,
        )

    def _call_sync(self, method: str, args, reply_type: str):
        return self._bus.call_sync(
            BUS_NAME, OBJECT_PATH, BUS_NAME, method, args,
            GLib.VariantType(reply_type), Gio.DBusCallFlags.NONE, 2000, None,
        ).unpack()

    # -- island control -----------------------------------------------------

    def show(self) -> None:
        self._call("Show")

    def hide(self) -> None:
        self._call("Hide")

    def set_state(self, state: str) -> None:
        if state not in STATES:
            raise ValueError(f"unknown state {state!r}, expected one of {STATES}")
        self._call("SetState", GLib.Variant("(s)", (state,)))

    def set_text(self, text: str) -> None:
        self._call("SetText", GLib.Variant("(s)", (text,)))

    def push_level(self, level: float) -> None:
        self._call("PushLevel", GLib.Variant("(d)", (float(level),)))

    # -- focus and injection ------------------------------------------------

    def insert_text(self, text: str) -> None:
        self._call("InsertText", GLib.Variant("(s)", (text,)))

    # -- signals from the Shell ---------------------------------------------

    def connect_signals(self, on_pressed, on_released, on_cancel=None) -> list[int]:
        """Subscribe to the extension's hotkey signals.

        Callbacks run on the GLib main loop, so they must not block - anything
        slow belongs on a worker thread.
        """
        subs = []

        def subscribe(name, handler):
            def trampoline(_conn, _sender, _path, _iface, _signal, params):
                handler(*params.unpack())

            return self._bus.signal_subscribe(
                BUS_NAME, BUS_NAME, name, OBJECT_PATH, None,
                Gio.DBusSignalFlags.NONE, trampoline,
            )

        subs.append(subscribe("HotkeyPressed", on_pressed))
        subs.append(subscribe("HotkeyReleased", on_released))
        if on_cancel is not None:
            subs.append(subscribe("CancelRequested", on_cancel))
        return subs

    def dev_hide_overview(self) -> None:
        """Test-only; the extension ignores this unless FLOW_DEV=1."""
        self._call("DevHideOverview")

    def dev_press_hotkey(self) -> None:
        """Test-only; the extension ignores this unless FLOW_DEV=1."""
        self._call("DevPressHotkey")

    def focus_context(self) -> dict[str, str]:
        (context,) = self._call_sync("GetFocusContext", None, "(a{ss})")
        return dict(context)
