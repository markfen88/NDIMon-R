#!/bin/bash
# NDIMon-R — Update script
set -e

INSTALL_DIR="/opt/ndi-monitor"
cd "$INSTALL_DIR"
git pull origin master
systemctl restart ndi-monitor.service
echo "NDIMon-R updated and service restarted."
