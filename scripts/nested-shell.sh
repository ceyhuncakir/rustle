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

FLOW_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FLOW_UUID="flow@ceyhun.dev"
FLOW_LOG="${FLOW_LOG:-/tmp/flow-nested.log}"
FLOW_SOCKET="${FLOW_SOCKET:-flow-nested}"
FLOW_MONITOR="${FLOW_MONITOR:-1600x900}"

# Read by the extension's _enableDevMode; needs no second extension and writes
# nothing to the dconf key the real session shares.
export FLOW_DEV="${FLOW_DEV:-0}"
export FLOW_ROOT FLOW_UUID FLOW_LOG FLOW_SOCKET FLOW_MONITOR

"$FLOW_ROOT/scripts/install.sh" >/dev/null

if [ "$#" -gt 0 ]; then
    export FLOW_SHELL_ARGS="--headless --virtual-monitor $FLOW_MONITOR"
else
    export FLOW_SHELL_ARGS="--nested"
    export MUTTER_DEBUG_DUMMY_MODE_SPECS="$FLOW_MONITOR"
fi

echo "shell log: $FLOW_LOG"

exec dbus-run-session -- bash -c '
    set -u

    # shellcheck disable=SC2086
    gnome-shell --wayland --wayland-display "$FLOW_SOCKET" $FLOW_SHELL_ARGS \
        >"$FLOW_LOG" 2>&1 &
    shell_pid=$!

    # Wait for the Shell to own its bus name before poking at it.
    for _ in $(seq 60); do
        gdbus introspect --session --dest org.gnome.Shell \
            --object-path /org/gnome/Shell >/dev/null 2>&1 && break
        sleep 0.25
    done

    gnome-extensions enable "$FLOW_UUID" >/dev/null 2>&1 || true
    sleep 2

    status=0
    if [ "$#" -gt 0 ]; then
        # Point clients at the child compositor, not the real one.
        export WAYLAND_DISPLAY="$FLOW_SOCKET"
        unset DISPLAY
        "$@" || status=$?
        kill "$shell_pid" 2>/dev/null || true
        wait "$shell_pid" 2>/dev/null || true
        exit "$status"
    fi

    wait "$shell_pid"
' bash "$@"
