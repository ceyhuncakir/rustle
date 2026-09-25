#!/usr/bin/env bash
# Symlink the extension into place so edits take effect without reinstalling.
set -euo pipefail

UUID="rustle@ceyhun.dev"
SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/../extension" && pwd)"
DEST="$HOME/.local/share/gnome-shell/extensions/$UUID"

# The compiled schema is a build artifact, so it is not in the repository.
# Without it the extension cannot read its settings and the hotkey never binds.
glib-compile-schemas "$SRC/schemas"

mkdir -p "$(dirname "$DEST")"
rm -rf "$DEST"
ln -s "$SRC" "$DEST"
echo "linked $DEST -> $SRC"

# The extension used to be flow@ceyhun.dev. Left in place, that copy would
# load again at the next login and bind the same shortcut. A copy the Shell
# is running now keeps running until then.
OLD="$(dirname "$DEST")/flow@ceyhun.dev"
if [[ -L $OLD || -d $OLD ]]; then
    rm -rf "$OLD"
    echo "removed the old Flow extension at $OLD"
fi

# `gnome-extensions enable` only knows extensions the running Shell has
# loaded, which a new one is not until the next login; adding it to the
# list directly works either way.
if gnome-extensions enable "$UUID" 2>/dev/null; then
    echo "enabled $UUID"
else
    enabled="$(gsettings get org.gnome.shell enabled-extensions)"
    case "$enabled" in
        *"'$UUID'"*) ;;
        "@as []") gsettings set org.gnome.shell enabled-extensions "['$UUID']" ;;
        *) gsettings set org.gnome.shell enabled-extensions "${enabled%]}, '$UUID']" ;;
    esac
    echo "enabled $UUID; it loads when you log out and back in"
fi
