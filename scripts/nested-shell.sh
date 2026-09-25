#!/usr/bin/env bash
# Run the extension in a throwaway GNOME Shell, isolated from the real session.
#
# On Wayland the Shell cannot be restarted in place - there is no Alt+F2 "r" -
# so the only ways to load a changed extension are to log out or to run a
# second Shell. This does the latter.
#
#   scripts/nested-shell.sh                        # visible window, poke at it
#   scripts/nested-shell.sh scripts/capture.py     # headless, scripted, exits
#
# With a command, the Shell runs headless against a virtual monitor: nothing
# appears on your desktop and the screenshot API still works. Without one, it
# opens a nested window you can interact with.
#
# The child Shell creates its own Wayland socket under a fixed name, and the
# command runs with WAYLAND_DISPLAY pointed at it - otherwise apps launched by
# the command would connect to the real compositor and show up on your desktop.
set -euo pipefail

RUSTLE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUSTLE_UUID="rustle@ceyhun.dev"
RUSTLE_LOG="${RUSTLE_LOG:-/tmp/rustle-nested.log}"
RUSTLE_SOCKET="${RUSTLE_SOCKET:-rustle-nested}"
RUSTLE_MONITOR="${RUSTLE_MONITOR:-1600x900}"

# Read by the extension's _enableDevMode; needs no second extension and writes
# nothing to the dconf key the real session shares.
export RUSTLE_DEV="${RUSTLE_DEV:-0}"
export RUSTLE_ROOT RUSTLE_UUID RUSTLE_LOG RUSTLE_SOCKET RUSTLE_MONITOR

"$RUSTLE_ROOT/scripts/install.sh" >/dev/null

if [ "$#" -gt 0 ]; then
    export RUSTLE_SHELL_ARGS="--headless --virtual-monitor $RUSTLE_MONITOR"
else
    export RUSTLE_SHELL_ARGS="--nested"
    export MUTTER_DEBUG_DUMMY_MODE_SPECS="$RUSTLE_MONITOR"
fi

echo "shell log: $RUSTLE_LOG"

# The inner script expands its variables itself, in the child session.
# shellcheck disable=SC2016
exec dbus-run-session -- bash -c '
    set -u

    # shellcheck disable=SC2086
    gnome-shell --wayland --wayland-display "$RUSTLE_SOCKET" $RUSTLE_SHELL_ARGS \
        >"$RUSTLE_LOG" 2>&1 &
    shell_pid=$!

    # Wait for the Shell to own its bus name before poking at it.
    for _ in $(seq 60); do
        gdbus introspect --session --dest org.gnome.Shell \
            --object-path /org/gnome/Shell >/dev/null 2>&1 && break
        sleep 0.25
    done

    gnome-extensions enable "$RUSTLE_UUID" >/dev/null 2>&1 || true
    sleep 2

    status=0
    if [ "$#" -gt 0 ]; then
        # Point clients at the child compositor, not the real one.
        export WAYLAND_DISPLAY="$RUSTLE_SOCKET"
        unset DISPLAY
        "$@" || status=$?
        kill "$shell_pid" 2>/dev/null || true
        wait "$shell_pid" 2>/dev/null || true
        exit "$status"
    fi

    wait "$shell_pid"
' bash "$@"
