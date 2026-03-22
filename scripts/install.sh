#!/bin/bash
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

if [ ! -f build/ndimon-r ]; then
    echo "[install] Build first: ./scripts/build.sh"
    exit 1
fi

# --- Root vs non-root ---
if [ "$(id -u)" = "0" ]; then
    # Running as root → install system-wide services
    SYSTEMD_DIR="/etc/systemd/system"
    SYSTEMCTL="systemctl"
    WANTED_BY="multi-user.target"
    SUDO=""
else
    # Non-root → user services in ~/.config/systemd/user
    SYSTEMD_DIR="$HOME/.config/systemd/user"
    SYSTEMCTL="systemctl --user"
    WANTED_BY="default.target"
    SUDO="sudo"
fi

echo "[install] Installing binaries..."
$SUDO cmake --install build

echo "[install] Installing config files (only if not present)..."
for f in config/*.json; do
    dest="/etc/$(basename $f)"
    if [ ! -f "$dest" ]; then
        $SUDO cp "$f" "$dest"
        echo "  Created $dest"
    else
        echo "  Skipped $dest (already exists)"
    fi
done

# Create param file if missing
[ -f /etc/ndi_src_find_param ] || $SUDO touch /etc/ndi_src_find_param

echo "[install] Installing Node.js API dependencies..."
cd api && npm install --production && cd ..

echo "[install] Granting Node.js permission to bind port 80..."
NODE_BIN=$(which node 2>/dev/null || echo "")
if [ -n "$NODE_BIN" ]; then
    $SUDO setcap cap_net_bind_service=+ep "$NODE_BIN" && \
        echo "  setcap applied to $NODE_BIN" || \
        echo "  WARNING: setcap failed. Run API as root or use PORT=8080 env var."
else
    echo "  WARNING: node not found; cannot apply setcap"
fi

echo "[install] Installing systemd services..."
mkdir -p "$SYSTEMD_DIR"
ACTUAL_NODE=$(which node 2>/dev/null || echo "/usr/bin/node")
for svc in systemd/*.service; do
    name=$(basename $svc)
    # Replace install dir placeholder, node binary path, and WantedBy target
    sed -e "s|NDIMON_INSTALL_DIR|$PROJECT_DIR|g" \
        -e "s|ExecStart=.*/node server\.js|ExecStart=$ACTUAL_NODE server.js|g" \
        -e "s|WantedBy=default\.target|WantedBy=$WANTED_BY|g" \
        "$svc" > "$SYSTEMD_DIR/$name"
    echo "  Installed $name → $SYSTEMD_DIR/$name"
done

if [ "$(id -u)" != "0" ]; then
    echo "[install] Enabling systemd linger so services persist after logout..."
    loginctl enable-linger "$(whoami)"
fi

echo "[install] Reloading systemd..."
$SYSTEMCTL daemon-reload

echo "[install] Enabling and starting services..."
$SYSTEMCTL enable ndimon-r ndimon-finder ndimon-api
$SYSTEMCTL restart ndimon-r ndimon-finder ndimon-api

IP=$(hostname -I | awk '{print $1}')
echo ""
echo "[install] Done!"
echo "  Web UI: http://${IP}/"
echo "  API:    http://${IP}/v1/NDIFinder/List"
echo "  Modes:  http://${IP}/v1/VideoOutput/modes"
echo ""
echo "  (If port 80 is unavailable, set PORT=8080 in the ndimon-api service env)"
echo ""
echo "  Logs:"
if [ "$(id -u)" = "0" ]; then
    echo "    journalctl -u ndimon-r -f"
    echo "    journalctl -u ndimon-finder -f"
    echo "    journalctl -u ndimon-api -f"
else
    echo "    journalctl --user -u ndimon-r -f"
    echo "    journalctl --user -u ndimon-finder -f"
    echo "    journalctl --user -u ndimon-api -f"
fi
