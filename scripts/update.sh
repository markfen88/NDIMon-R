#!/bin/bash
# Quick update: git pull → rebuild → reinstall binary + API → restart services
# Run this after each git pull on the deployed machines.
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

echo "[update] Building..."
cmake --build build -j$(nproc)

echo "[update] Stopping ndimon-r to replace binary..."
systemctl stop ndimon-r 2>/dev/null || true

echo "[update] Installing binary..."
cmake --install build

echo "[update] Installing API..."
cp -r api /opt/ndimon-r/

echo "[update] Restarting services..."
systemctl restart ndimon-r ndimon-api ndimon-finder 2>/dev/null || true

echo "[update] Done. Status:"
systemctl is-active ndimon-r ndimon-api ndimon-finder 2>/dev/null || true
