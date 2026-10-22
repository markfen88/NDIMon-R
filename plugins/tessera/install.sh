#!/bin/bash
set -euo pipefail
exec /usr/bin/python3 -I "$(dirname -- "$0")/priv/helper.py" install "$@"
