#!/usr/bin/env bash
# Install Ollama into ~/.local without root, and pull the cleanup model.
#
# The official installer writes to /usr/local and wants sudo. The release
# tarball is the same binary and extracts anywhere, so Flow uses that instead.
set -euo pipefail

PREFIX="$HOME/.local"
UNIT_DIR="$HOME/.config/systemd/user"
ASSET="ollama-linux-amd64.tar.zst"
MODEL="${FLOW_CLEANUP_MODEL:-qwen3:14b}"

mkdir -p "$PREFIX/bin" "$UNIT_DIR"

if [ -x "$PREFIX/bin/ollama" ]; then
    echo "ollama already installed at $PREFIX/bin/ollama"
else
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
Description=Ollama (local model server for Flow)
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

systemctl --user daemon-reload
systemctl --user enable --now ollama.service
echo "ollama.service started"

for _ in $(seq 60); do
    curl -sf -m 2 http://127.0.0.1:11434/api/tags >/dev/null 2>&1 && break
    sleep 1
done

echo "pulling $MODEL ..."
"$PREFIX/bin/ollama" pull "$MODEL"

echo
echo "done. Check it with: flow doctor"
