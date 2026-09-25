#!/usr/bin/env bash
# Install Ollama into ~/.local without root, and pull the cleanup model.
#
# The official installer writes to /usr/local and wants sudo. The release
# tarball is the same binary and extracts anywhere, so Rustle uses that instead.
set -euo pipefail

PREFIX="$HOME/.local"
UNIT_DIR="$HOME/.config/systemd/user"
MODEL="${RUSTLE_CLEANUP_MODEL:-qwen3:14b}"

case "$(uname -m)" in
    x86_64 | amd64) arch=amd64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *)
        echo "error: Ollama publishes no Linux build for $(uname -m); see https://ollama.com/download" >&2
        exit 1
        ;;
esac
ASSET="ollama-linux-$arch.tar.zst"

# Name every missing tool at once rather than failing on the first.
require() {
    local missing=()
    for tool in "$@"; do
        command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
    done
    if ((${#missing[@]})); then
        echo "error: missing ${missing[*]}. Install with your package manager, e.g." >&2
        echo "       'sudo dnf install ${missing[*]}' or 'sudo apt install ${missing[*]}', then run this again." >&2
        exit 1
    fi
}
require curl

mkdir -p "$PREFIX/bin" "$UNIT_DIR"

if [ -x "$PREFIX/bin/ollama" ]; then
    echo "ollama already installed at $PREFIX/bin/ollama"
else
    # python3 reads the release list; tar needs zstd for the .tar.zst.
    require python3 tar zstd
    # Resolve the asset from the release API rather than hardcoding a URL;
    # ollama.com/download redirects to a path that no longer exists, and the
    # asset name has changed format before (.tgz -> .tar.zst).
    url="$(curl -fsSL https://api.github.com/repos/ollama/ollama/releases/latest \
        | python3 -c "
import json, sys
assets = json.load(sys.stdin).get('assets', [])
for asset in assets:
    if asset['name'] == '$ASSET':
        print(asset['browser_download_url'])
        break
")"

    if [ -z "$url" ]; then
        echo "error: could not find $ASSET in the latest ollama release" >&2
        exit 1
    fi

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    echo "downloading ollama (about 1.4 GB) ..."
    curl -fL --progress-bar "$url" -o "$tmp/$ASSET"
    echo "extracting into $PREFIX ..."
    tar -C "$PREFIX" --zstd -xf "$tmp/$ASSET"
    echo "installed $PREFIX/bin/ollama"
fi

cat > "$UNIT_DIR/ollama.service" <<UNIT
[Unit]
Description=Ollama (local model server for Rustle)
After=network-online.target

[Service]
Type=simple
ExecStart=%h/.local/bin/ollama serve
Restart=on-failure
RestartSec=3
Environment="OLLAMA_HOST=127.0.0.1:11434"

[Install]
WantedBy=default.target
UNIT

# Over SSH or in a container there may be no user manager to run the
# service; say how to go on by hand instead of failing half-way.
if ! systemctl --user daemon-reload 2>/dev/null; then
    echo "note: no systemd user manager here (SSH session or container?), so ollama.service" >&2
    echo "      was written but not started. Start the server with '$PREFIX/bin/ollama serve'," >&2
    echo "      then fetch the model with '$PREFIX/bin/ollama pull $MODEL'." >&2
    exit 0
fi
systemctl --user enable --now ollama.service
echo "ollama.service started"

for _ in $(seq 60); do
    curl -sf -m 2 http://127.0.0.1:11434/api/tags >/dev/null 2>&1 && break
    sleep 1
done

echo "pulling $MODEL ..."
"$PREFIX/bin/ollama" pull "$MODEL"

echo
echo "done. Check it with: rustle doctor"
