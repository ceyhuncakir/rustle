#!/usr/bin/env bash
# Install Flow as a launchable app: a `flow` binary on PATH, an entry in the
# Applications grid, and a user service so it can be switched on and off.
#
# Nothing here needs root and nothing is enabled at login - Flow starts when
# you launch it.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="$HOME/.local/bin"
APP_DIR="$HOME/.local/share/applications"
ICON_DIR="$HOME/.local/share/icons/hicolor/scalable/apps"
UNIT_DIR="$HOME/.config/systemd/user"

if [ ! -x "$ROOT/.venv/bin/flow" ]; then
    echo "error: $ROOT/.venv/bin/flow missing - run 'uv sync' first, or:" >&2
    echo "       uv venv --python 3.13 --system-site-packages" >&2
    echo "       uv pip install -e '.[gpu]'" >&2
    exit 1
fi

mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR" "$UNIT_DIR"

# A symlink, not a copy: the venv's console script already points at the venv
# interpreter, so it works from anywhere, and edits to the source take effect
# immediately because the package is installed editable.
ln -sfn "$ROOT/.venv/bin/flow" "$BIN_DIR/flow"
echo "binary   $BIN_DIR/flow"

install -m 644 "$ROOT/packaging/flow-dictation.svg" "$ICON_DIR/flow-dictation.svg"
echo "icon     $ICON_DIR/flow-dictation.svg"

install -m 644 "$ROOT/packaging/flow-dictation.desktop" "$APP_DIR/flow-dictation.desktop"
echo "desktop  $APP_DIR/flow-dictation.desktop"

install -m 644 "$ROOT/packaging/flow.service" "$UNIT_DIR/flow.service"
echo "service  $UNIT_DIR/flow.service"

# The .desktop calls `flow` unqualified, so the grid launch only works if
# ~/.local/bin is on PATH for graphical launches too.
if ! systemctl --user show-environment 2>/dev/null | grep -q "^PATH=.*$BIN_DIR"; then
    systemctl --user import-environment PATH 2>/dev/null || true
fi

systemctl --user daemon-reload
update-desktop-database "$APP_DIR" 2>/dev/null || true
gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo; echo "note: $BIN_DIR is not on your PATH; add it to ~/.zshrc:";
       echo "      export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
esac

cat <<'DONE'

Installed. "Flow Dictation" is now in your Applications grid - launching it
starts the daemon, and right-clicking the icon offers Stop.

From a terminal:
  flow doctor     check every moving part
  flow start      start in the background
  flow stop       stop
  flow logs       follow the daemon log
  flow dictate    record 5s and insert, without needing the hotkey
DONE
