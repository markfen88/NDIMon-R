#!/usr/bin/env bash
# NDI Monitor — Update script
set -euo pipefail
INSTALL_DIR="/opt/ndi-monitor"
echo "[update] Pulling latest from git..."
cd "$INSTALL_DIR"
git pull --ff-only
echo "[update] Updating Python dependencies..."
"$INSTALL_DIR/venv/bin/pip" install -q --upgrade flask flask-login gunicorn pillow psutil
echo "[update] Restarting service..."
systemctl restart ndi-monitor.service
echo "[update] Done. Service is running."
