#!/bin/bash
set -e
cd "$(dirname "$0")/.."

echo "[build] Configuring CMake..."
cmake -B build \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX=/usr/local \
    "$@"

echo "[build] Building..."
cmake --build build --parallel $(nproc)

echo "[build] Done. Binaries:"
ls -lh build/ndimon-r build/ndimon-finder 2>/dev/null || true
echo ""
echo "Run './scripts/install.sh' to install (or 'bash install.sh' for one-step)."
