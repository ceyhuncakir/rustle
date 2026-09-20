#!/usr/bin/env bash
# Symlink the extension into place so edits take effect without reinstalling.
set -euo pipefail

UUID="flow@ceyhun.dev"
SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/../extension" && pwd)"
DEST="$HOME/.local/share/gnome-shell/extensions/$UUID"

# The compiled schema is a build artifact, so it is not in the repository.
# Without it the extension cannot read its settings and the hotkey never binds.
glib-compile-schemas "$SRC/schemas"

mkdir -p "$(dirname "$DEST")"
rm -rf "$DEST"
ln -s "$SRC" "$DEST"
echo "linked $DEST -> $SRC"

gnome-extensions enable "$UUID" 2>/dev/null \
    && echo "enabled $UUID" \
    || echo "could not enable yet - log out and back in, then: gnome-extensions enable $UUID"
