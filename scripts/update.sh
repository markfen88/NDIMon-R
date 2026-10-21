#!/bin/bash
# Quick update: git pull → rebuild → reinstall binary + API → restart services
# Run this after each git pull on the deployed machines.
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

echo "[update] Pulling..."
git pull --ff-only

echo "[update] Building..."
cmake --build build -j$(nproc)

echo "[update] Staging API..."
API_DEST="/opt/ndimon-r"
NEW_API="$API_DEST/api.new"
OLD_API="$API_DEST/api.old"
rm -rf "$NEW_API" "$OLD_API"
mkdir -p "$API_DEST"
cp -r api "$NEW_API"

echo "[update] Installing API dependencies in the staged tree..."
if ! (cd "$NEW_API" && npm install --production); then
    echo "[update] npm install failed — leaving the running API and decoder in place" >&2
    rm -rf "$NEW_API"
    exit 1
fi

echo "[update] Installing API runtime directory and service..."
install -D -m 0644 systemd/ndimon.tmpfiles.conf /usr/lib/tmpfiles.d/ndimon.conf
systemd-tmpfiles --create ndimon.conf
NDIMON_NODE=$(command -v node || echo /usr/bin/node)
sed -e "s|NDIMON_INSTALL_DIR|$API_DEST|g" \
    -e "s|ExecStart=.*/node server\.js|ExecStart=$NDIMON_NODE server.js|g" \
    systemd/ndimon-api.service > /etc/systemd/system/ndimon-api.service.tmp
mv /etc/systemd/system/ndimon-api.service.tmp /etc/systemd/system/ndimon-api.service
systemctl daemon-reload

echo "[update] Stopping ndimon-r to replace binary..."
systemctl stop ndimon-r 2>/dev/null || true

echo "[update] Installing binary and decoder policy..."
cmake --install build
install -D -o root -g root -m 0755 scripts/ndimon-cpu-sampling.py /usr/local/lib/ndimon/ndimon-cpu-sampling
sed 's|WantedBy=default.target|WantedBy=multi-user.target|g' systemd/ndimon-r.service > /etc/systemd/system/ndimon-r.service.tmp
mv /etc/systemd/system/ndimon-r.service.tmp /etc/systemd/system/ndimon-r.service
# update.sh used to install only ndimon-r and ndimon-api: a unit change for the
# finder or watchdog (e.g. NDI_CONFIG_DIR) never reached updated devices.
for unit in ndimon-finder ndimon-watchdog; do
    sed -e "s|NDIMON_INSTALL_DIR|$API_DEST|g" \
        -e "s|WantedBy=default\.target|WantedBy=multi-user.target|g" \
        "systemd/$unit.service" > "/etc/systemd/system/$unit.service.tmp"
    mv "/etc/systemd/system/$unit.service.tmp" "/etc/systemd/system/$unit.service"
done
install -D -m 0755 scripts/ndimon-watchdog.sh "$API_DEST/scripts/ndimon-watchdog.sh"
install -D -m 0755 scripts/ndimon-update-run.sh "$API_DEST/scripts/ndimon-update-run.sh"
systemctl daemon-reload

echo "[update] Installing API..."
if [ -d "$API_DEST/api" ]; then
    mv "$API_DEST/api" "$OLD_API"
fi
mv "$NEW_API" "$API_DEST/api"
rm -rf "$OLD_API"

echo "[update] Restarting services..."
systemctl restart ndimon-r ndimon-api ndimon-finder ndimon-watchdog 2>/dev/null || true

echo "[update] Done. Status:"
systemctl is-active ndimon-r ndimon-api ndimon-finder ndimon-watchdog 2>/dev/null || true
