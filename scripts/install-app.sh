#!/usr/bin/env bash
# Install Flow for this user: the program in ~/.local/lib/flow, a `flow`
# command in ~/.local/bin, the desktop entry, the icon and the user service.
# Nothing needs root and nothing starts at login.
#
# Usage: scripts/install-app.sh [--cuda | --cpu]
#   The default build recognises speech on any graphics card through WebGPU
#   (Vulkan), and on the CPU where no card is worth it; it needs nothing
#   beyond the graphics driver. --cuda builds for NVIDIA's CUDA instead,
#   about 20% faster on an NVIDIA card but needing CUDA 12 and cuDNN 9;
#   --cpu leaves the GPU out. `flow gpu` shows what the result will use.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_DIR="$HOME/.local/lib/flow"
BIN="$HOME/.local/bin/flow"

case "${1:-}" in
    "") backend=webgpu ;;
    --cuda) backend=cuda ;;
    --cpu) backend=cpu ;;
    *) echo "usage: $0 [--cuda | --cpu]" >&2; exit 2 ;;
esac
FEATURES=()
[[ $backend != cpu ]] && FEATURES=(--features "$backend")
echo "building for: $backend"

# `tauri build`, not `cargo build`: only the former embeds the web UI. A
# plain cargo build leaves the windows pointing at the dev server.
echo "building (a release build takes a few minutes)..."
(cd "$ROOT" && pnpm install --frozen-lockfile >/dev/null && pnpm tauri build --no-bundle "${FEATURES[@]}")
OUT="$ROOT/target/release"
[[ -x "$OUT/flow" ]] || { echo "build did not produce $OUT/flow" >&2; exit 1; }

# The libraries the GPU backend needs sit next to the real program: Dawn
# for WebGPU (found through the program's rpath), ONNX Runtime's provider
# for CUDA (loaded from beside the program as it was started, links not
# followed). Hence ~/.local/bin/flow is a wrapper that starts it by path.
install -Dm755 "$OUT/flow" "$APP_DIR/flow"
rm -f "$APP_DIR"/libonnxruntime_providers_*.so "$APP_DIR"/libwebgpu_dawn.so
case $backend in
    webgpu) libs=(libwebgpu_dawn.so) ;;
    cuda) libs=(libonnxruntime_providers_shared.so libonnxruntime_providers_cuda.so) ;;
    *) libs=() ;;
esac
for lib in "${libs[@]}"; do
    install -m644 "$OUT/$lib" "$APP_DIR/$lib"
done
mkdir -p "$(dirname "$BIN")"
# Remove first: an older install left a symlink here, and writing through
# it would overwrite whatever it points at.
rm -f "$BIN"
printf '#!/bin/sh\nexec "%s" "$@"\n' "$APP_DIR/flow" >"$BIN"
chmod 755 "$BIN"

install -Dm644 "$ROOT/packaging/flow-dictation.svg" \
    "$HOME/.local/share/icons/hicolor/scalable/apps/flow-dictation.svg"
# An absolute Exec: the desktop session's PATH often lacks ~/.local/bin.
mkdir -p "$HOME/.local/share/applications"
sed "s|^Exec=flow\$|Exec=\"$BIN\"|" "$ROOT/packaging/flow-dictation.desktop" \
    >"$HOME/.local/share/applications/flow-dictation.desktop"
install -Dm644 "$ROOT/packaging/flow.service" "$HOME/.config/systemd/user/flow.service"

# The unit runs `flow --headless`, which needs the GNOME Shell extension.
if [[ ${XDG_CURRENT_DESKTOP:-} == *GNOME* ]]; then
    "$ROOT/scripts/install.sh"
fi

# Over SSH or in a container there may be no user manager; the unit file is
# in place either way and loads at the next graphical login.
if systemctl --user daemon-reload 2>/dev/null; then
    service=yes
else
    service=no
    echo "note: no systemd user manager here (SSH session or container?); flow.service is" >&2
    echo "      installed and loads at your next desktop login." >&2
fi
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

echo
echo "installed $APP_DIR/flow"
"$BIN" gpu 2>/dev/null || true
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) echo "note: ~/.local/bin is not on your PATH; the app launcher works regardless" ;;
esac
echo
echo "next:"
echo "  flow doctor                       # every part should be ok"
if [[ $service == yes ]]; then
    echo "  systemctl --user start flow       # headless, driven by the Shell extension"
else
    echo "  flow --headless                   # headless, driven by the Shell extension"
fi
echo "  flow                              # or the tray app with the settings window"
