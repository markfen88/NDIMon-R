#!/bin/bash
# Started by ndimon-update.service (pid1, root). Not started by ndimon-api.
set -euo pipefail
dir=$(tr -d '\0' < /etc/ndimon-source-dir | head -n 1 | tr -d '\n')
case "$dir" in
    /*) ;;
    *) echo "source dir is not absolute" >&2; exit 2 ;;
esac
case "$dir" in
    *..*) echo "source dir is not allowed" >&2; exit 2 ;;
esac
[ -f "$dir/install.sh" ] || { echo "install.sh missing" >&2; exit 2; }
git -C "$dir" pull --ff-only
bash "$dir/install.sh" --no-deps
