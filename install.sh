#!/usr/bin/env bash
# NDIMon-R — One-command installer
# Usage: sudo bash install.sh [--no-deps] [--no-build]
#
# Options:
#   --no-deps   Skip dependency installation (already installed)
#   --no-build  Skip build step (binary already compiled)

set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

SKIP_DEPS=0; SKIP_BUILD=0
NDIMON_WITH_PLUGINS=''; NDIMON_PLUGIN_FEEDS=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-deps) SKIP_DEPS=1; shift ;;
    --no-build) SKIP_BUILD=1; shift ;;
    --with-plugin|--plugin-feed)
      option=$1
      [[ $# -ge 2 && -n "$2" && "$2" != --* && "$2" != *$'\n'* ]] || { echo "missing or invalid $option value" >&2; exit 2; }
      if [[ "$option" == --with-plugin ]]; then
        name=${2%%=*}
        [[ "$name" =~ ^[a-z][a-z0-9-]{1,31}$ && "$2" != *= ]] || { echo 'invalid plugin name/archive' >&2; exit 2; }
        NDIMON_WITH_PLUGINS+="${NDIMON_WITH_PLUGINS:+$'\n'}$2"
      else
        [[ "$2" =~ ^https?:// ]] || { echo 'invalid plugin feed URL' >&2; exit 2; }
        NDIMON_PLUGIN_FEEDS+="${NDIMON_PLUGIN_FEEDS:+$'\n'}$2"
      fi
      shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
export NDIMON_WITH_PLUGINS NDIMON_PLUGIN_FEEDS
if [[ "${NDIMON_INSTALL_DRY_RUN:-}" == 1 ]]; then
  exec bash scripts/install.sh
fi

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
    sudo env NDIMON_WITH_PLUGINS="$NDIMON_WITH_PLUGINS" NDIMON_PLUGIN_FEEDS="$NDIMON_PLUGIN_FEEDS" bash scripts/install.sh
else
    bash scripts/install.sh
fi
