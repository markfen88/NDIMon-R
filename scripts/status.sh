#!/bin/bash
# Quick status dump for a running appliance.
set -euo pipefail

SERVICES="ndimon-r ndimon-finder ndimon-api ndimon-watchdog"

echo "=== NDIMon-R Services ==="
if systemctl is-enabled ndimon-r >/dev/null 2>&1; then
    systemctl status $SERVICES --no-pager -l 2>/dev/null || true
else
    systemctl --user status $SERVICES --no-pager -l 2>/dev/null || true
fi

echo ""
echo "=== Health (loopback, no auth) ==="
curl -sS --max-time 3 http://127.0.0.1/api/health 2>/dev/null | python3 -m json.tool 2>/dev/null \
    || curl -sS --max-time 3 http://127.0.0.1/api/health 2>/dev/null \
    || echo "(API not reachable on port 80)"

echo ""
echo "=== Sources ==="
python3 -m json.tool /etc/ndimon-sources.json 2>/dev/null \
    || cat /etc/ndimon-sources.json 2>/dev/null \
    || echo "(no /etc/ndimon-sources.json)"
