#!/bin/bash
# Build the vendored Tessera plugin using an explicitly pinned native runtime.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ $# != 3 || $1 != tessera || ( $2 != --bundle && $2 != --ubuntu-experimental-bundle ) ]]; then
    echo 'usage: scripts/build-plugin.sh tessera --bundle /path/to/tessera-bundle_*.tar.gz' >&2
    exit 2
fi
# Retain the original ARM-only invocation for existing operator scripts.
if [[ $2 == --ubuntu-experimental-bundle && $3 != */tessera-bundle_*_arm64_ubuntu-noble-experimental.tar.gz ]]; then
    echo 'experimental Ubuntu noble arm64 bundle required' >&2
    exit 2
fi
exec python3 scripts/build-plugin.py --plugin tessera --bundle "$3" --output dist/plugins
