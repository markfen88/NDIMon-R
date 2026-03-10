#!/bin/bash
# NDIMon-R — Update script
set -e

INSTALL_DIR="/opt/ndimon-r"
SERVICE_NAME="ndimon-r"
cd "$INSTALL_DIR"
git pull origin master
"$INSTALL_DIR/venv/bin/pip" install -q -r "$INSTALL_DIR/requirements.txt"

# Re-link python3-cec into venv (survives venv rebuilds)
PYTHON_VER=$("$INSTALL_DIR/venv/bin/python3" -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")
VENV_SITE="$INSTALL_DIR/venv/lib/python${PYTHON_VER}/site-packages"
if [[ -f /usr/lib/python3/dist-packages/cec.py ]]; then
    ln -sf /usr/lib/python3/dist-packages/cec.py "$VENV_SITE/cec.py"
    for so in /usr/lib/python3/dist-packages/_cec.cpython-*-linux-gnu.so; do
        [[ -f "$so" ]] && ln -sf "$so" "$VENV_SITE/$(basename "$so")" && break
    done
fi

systemctl restart ${SERVICE_NAME}.service
echo "NDIMon-R updated and service restarted."
