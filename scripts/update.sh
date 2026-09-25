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

echo "[update] Stopping ndimon-r to replace binary..."
systemctl stop ndimon-r 2>/dev/null || true

echo "[update] Installing binary..."
cmake --install build

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
