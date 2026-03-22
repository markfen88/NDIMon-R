#!/bin/bash
echo "=== NDIMon-R Services ==="
systemctl --user status ndimon-r ndimon-finder ndimon-api --no-pager -l 2>/dev/null

echo ""
echo "=== Source List ==="
cat /etc/ndimon-sources.json 2>/dev/null | python3 -m json.tool 2>/dev/null || cat /etc/ndimon-sources.json 2>/dev/null

echo ""
echo "=== Decoder Status ==="
cat /etc/ndimon-dec1-status.json 2>/dev/null | python3 -m json.tool 2>/dev/null || cat /etc/ndimon-dec1-status.json 2>/dev/null

echo ""
echo "=== API Status ==="
curl -s http://localhost/api/status 2>/dev/null | python3 -m json.tool 2>/dev/null
