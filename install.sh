#!/usr/bin/env bash
# NDIMon-R — One-command installer
# Usage: bash install.sh [--no-deps] [--no-build]
#
# Options:
#   --no-deps   Skip dependency installation (already installed)
#   --no-build  Skip build step (binary already compiled)

set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

SKIP_DEPS=0; SKIP_BUILD=0
for arg in "$@"; do
  [[ "$arg" == "--no-deps"  ]] && SKIP_DEPS=1
  [[ "$arg" == "--no-build" ]] && SKIP_BUILD=1
done

echo "=== NDIMon-R Installer ==="
echo ""

# 1. Dependencies (requires root)
if [[ $SKIP_DEPS -eq 0 ]]; then
    echo "[install] Installing dependencies..."
    if [[ $EUID -ne 0 ]]; then
        echo "  (requires root — running with sudo)"
        sudo bash scripts/setup-deps.sh
    else
        bash scripts/setup-deps.sh
    fi
else
    echo "[install] Skipping dependencies (--no-deps)"
fi

# 2. Build
if [[ $SKIP_BUILD -eq 0 ]]; then
    echo "[install] Building..."
    bash scripts/build.sh
else
    echo "[install] Skipping build (--no-build)"
fi

# 3. Install binaries + services (requires root for system-wide)
echo "[install] Installing binaries and services..."
if [[ $EUID -ne 0 ]]; then
    sudo bash scripts/install.sh
else
    bash scripts/install.sh
fi
