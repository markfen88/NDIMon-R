#!/bin/bash
# NDIMon-R — Update script
set -e

INSTALL_DIR="/opt/ndi-monitor"
SERVICE_NAME="ndimon-r"
cd "$INSTALL_DIR"
git pull origin master
"$INSTALL_DIR/venv/bin/pip" install -q -r "$INSTALL_DIR/requirements.txt"
systemctl restart ${SERVICE_NAME}.service
echo "NDIMon-R updated and service restarted."
