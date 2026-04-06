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

# --- RGA device symlink ---
# On newer kernels (6.x) the Rockchip RGA is registered as /dev/video* instead
# of /dev/rga. librga still opens /dev/rga at runtime, so we create a symlink
# and a persistent udev rule so it survives reboots.
if [ "$(id -u)" = "0" ] && [ ! -e /dev/rga ]; then
    RGA_DEV=$(grep -rl "rockchip-rga" /sys/class/video4linux/*/name 2>/dev/null \
              | sed 's|/sys/class/video4linux/\(video[0-9]*\)/name|\1|' | head -1)
    if [ -n "$RGA_DEV" ]; then
        ln -sf "/dev/$RGA_DEV" /dev/rga
        echo "[install] Created /dev/rga -> /dev/$RGA_DEV"
        # Persist via udev so symlink survives reboot
        echo 'KERNEL=="video*", ATTRS{name}=="rockchip-rga", SYMLINK+="rga"' \
            > /etc/udev/rules.d/99-rockchip-rga.rules
        udevadm control --reload-rules 2>/dev/null || true
        echo "[install] Installed udev rule for /dev/rga"
    fi
fi

# --- Install C++ binaries ---
if [ -f "$PROJECT_DIR/build/ndimon-r" ]; then
    echo "[install] Installing ndimon-r binary to /usr/local/bin..."
    $SUDO cp "$PROJECT_DIR/build/ndimon-r" /usr/local/bin/ndimon-r.new
    $SUDO mv /usr/local/bin/ndimon-r.new /usr/local/bin/ndimon-r
fi
if [ -f "$PROJECT_DIR/build/ndimon-finder" ]; then
    echo "[install] Installing ndimon-finder binary to /usr/local/bin..."
    $SUDO cp "$PROJECT_DIR/build/ndimon-finder" /usr/local/bin/ndimon-finder.new
    $SUDO mv /usr/local/bin/ndimon-finder.new /usr/local/bin/ndimon-finder
fi

# --- Install API to stable location ---
# The API is installed to /opt/ndimon-r/api so its path is independent of
# where the source was cloned (avoids WorkingDirectory pointing at a temp dir).
API_INSTALL_DIR="/opt/ndimon-r"
echo "[install] Installing Node.js API to $API_INSTALL_DIR/api..."
$SUDO mkdir -p "$API_INSTALL_DIR"
$SUDO cp -r "$PROJECT_DIR/api" "$API_INSTALL_DIR/"
(cd "$API_INSTALL_DIR/api" && $SUDO npm install --production) || \
    echo "  WARNING: npm install failed (API may not work)"

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
for svc in "$PROJECT_DIR/systemd/"*.service; do
    name=$(basename "$svc")
    # Replace placeholders: install dir → /opt/ndimon-r, node binary, WantedBy target
    $SUDO sed -e "s|NDIMON_INSTALL_DIR|$API_INSTALL_DIR|g" \
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
# Install watchdog script
echo "[install] Installing watchdog script..."
$SUDO mkdir -p "$API_INSTALL_DIR/scripts"
$SUDO cp "$PROJECT_DIR/scripts/ndimon-watchdog.sh" "$API_INSTALL_DIR/scripts/"
$SUDO chmod +x "$API_INSTALL_DIR/scripts/ndimon-watchdog.sh"

$SYSTEMCTL enable ndimon-r ndimon-finder ndimon-api ndimon-watchdog
$SYSTEMCTL restart ndimon-r ndimon-finder ndimon-api ndimon-watchdog

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
