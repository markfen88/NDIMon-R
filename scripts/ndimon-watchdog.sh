#!/bin/bash
# NDIMon-R Watchdog — independent service that monitors all NDIMon components.
#
# Three modes (set via web UI → /etc/ndimon-device-settings.json):
#   disabled — only monitor process liveness (systemd), no health checks
#   passive  — monitor processes + poll /api/health, log issues, no corrective action
#   active   — monitor processes + poll /api/health, take corrective action on stalls
#
# Process monitoring always runs regardless of mode (services must be alive).
# Health monitoring (API-based) is controlled by watchdog_mode.

set -euo pipefail

INTERVAL=15
MAX_FAILURES=5
COOLDOWN=60
HEALTH_URL="http://127.0.0.1:80/api/health"
DEVICE_SETTINGS="/etc/ndimon-device-settings.json"
STATS_FILE="/tmp/ndimon-watchdog-stats.json"

declare -A fail_count
declare -A last_restart

for svc in ndimon-r ndimon-api ndimon-finder; do
    fail_count[$svc]=0
    last_restart[$svc]=0
done

# Watchdog action counters
wd_recv_stalls=0
wd_video_stalls=0
wd_decoder_stalls=0
wd_display_freezes=0
wd_reconnects=0
wd_service_restarts=0

log() { echo "[$(date '+%H:%M:%S')] [watchdog] $*"; }

get_watchdog_mode() {
    if [ -f "$DEVICE_SETTINGS" ]; then
        local mode
        mode=$(python3 -c "import json,sys; print(json.load(open('$DEVICE_SETTINGS')).get('watchdog_mode','passive'))" 2>/dev/null || echo "passive")
        echo "$mode"
    else
        echo "passive"
    fi
}

write_stats() {
    cat > "${STATS_FILE}.tmp" << EOF
{
  "watchdog_mode": "$(get_watchdog_mode)",
  "recv_stalls": $wd_recv_stalls,
  "video_stalls": $wd_video_stalls,
  "decoder_stalls": $wd_decoder_stalls,
  "display_freezes": $wd_display_freezes,
  "reconnects": $wd_reconnects,
  "service_restarts": $wd_service_restarts,
  "timestamp": $(date +%s)
}
EOF
    mv "${STATS_FILE}.tmp" "$STATS_FILE"
}

check_service() {
    local svc=$1
    if ! systemctl is-active --quiet "${svc}.service" 2>/dev/null; then
        return 1
    fi
    local pid
    pid=$(systemctl show -p MainPID --value "${svc}.service" 2>/dev/null)
    if [ -z "$pid" ] || [ "$pid" = "0" ]; then
        return 1
    fi
    [ -d "/proc/$pid" ] && return 0
    return 1
}

restart_service() {
    local svc=$1
    local now
    now=$(date +%s)
    local last=${last_restart[$svc]}
    local elapsed=$((now - last))

    if [ "$elapsed" -lt "$COOLDOWN" ]; then
        log "$svc: in cooldown (${elapsed}s/${COOLDOWN}s), skipping restart"
        return
    fi

    log "$svc: RESTARTING (${fail_count[$svc]} consecutive failures)"
    systemctl restart "$svc.service" 2>/dev/null || true
    fail_count[$svc]=0
    last_restart[$svc]=$now
    wd_service_restarts=$((wd_service_restarts + 1))
}

# Health check via API — returns JSON with per-output health data
check_health() {
    local mode=$1
    if [ "$mode" = "disabled" ]; then
        return
    fi

    local health_json
    health_json=$(curl -s --max-time 5 "$HEALTH_URL" 2>/dev/null) || return

    # Parse health data for each output
    local outputs
    outputs=$(echo "$health_json" | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
    for o in d.get('outputs', []):
        idx = o.get('output', 0)
        health = o.get('health', 'ok')
        stall = o.get('stall_count', 0)
        recv = o.get('recv_stale_ms', 0)
        video = o.get('video_stale_ms', 0)
        decoded = o.get('decoded_stale_ms', 0)
        display = o.get('display_stale_ms', 0)
        connected = o.get('connected', False)
        print(f'{idx}|{health}|{stall}|{recv}|{video}|{decoded}|{display}|{connected}')
except:
    pass
" 2>/dev/null) || return

    while IFS='|' read -r idx health stall recv video decoded display connected; do
        [ -z "$idx" ] && continue
        [ "$connected" != "True" ] && continue

        if [ "$health" != "ok" ] && [ "$health" != "idle" ]; then
            # Detect specific issues
            if [ "$recv" != "-1" ] && [ "$recv" -gt 5000 ] 2>/dev/null; then
                wd_recv_stalls=$((wd_recv_stalls + 1))
                log "output $idx: recv stall (${recv}ms) [$mode]"
            fi
            if [ "$video" = "-1" ]; then
                wd_video_stalls=$((wd_video_stalls + 1))
                log "output $idx: no video frames [$mode]"
            fi
            if [ "$decoded" != "-1" ] && [ "$decoded" -gt 5000 ] 2>/dev/null; then
                wd_decoder_stalls=$((wd_decoder_stalls + 1))
                log "output $idx: decoder stall (${decoded}ms) [$mode]"
            fi
            if [ "$display" != "-1" ] && [ "$display" -gt 3000 ] 2>/dev/null; then
                wd_display_freezes=$((wd_display_freezes + 1))
                log "output $idx: display freeze (${display}ms) [$mode]"
            fi

            # Active mode: take corrective action at escalation threshold
            if [ "$mode" = "active" ] && [ "$stall" -ge 60 ] 2>/dev/null; then
                log "output $idx: 30s stall escalation — triggering reconnect"
                wd_reconnects=$((wd_reconnects + 1))
                # Send reconnect via IPC through the API
                curl -s --max-time 5 -X POST "http://127.0.0.1:80/v1/NDIDecode/connectTo" \
                    -H "Content-Type: application/json" \
                    -d "{\"SourceName\":\"__reconnect__\",\"Output\":$((idx+1))}" \
                    >/dev/null 2>&1 || true
            fi
        fi
    done <<< "$outputs"
}

log "starting — monitoring ndimon-r, ndimon-api, ndimon-finder"
log "watchdog mode: $(get_watchdog_mode)"

log "service type=simple, no sd_notify needed"

while true; do
    # 1. Process liveness checks (always active)
    for svc in ndimon-r ndimon-api ndimon-finder; do
        if check_service "$svc" 2>/dev/null; then
            if [ "${fail_count[$svc]}" -gt 0 ]; then
                log "$svc: recovered (was at ${fail_count[$svc]} failures)"
            fi
            fail_count[$svc]=0
        else
            fail_count[$svc]=$((${fail_count[$svc]} + 1))
            log "$svc: process check FAILED (${fail_count[$svc]}/$MAX_FAILURES)"

            if [ "${fail_count[$svc]}" -ge "$MAX_FAILURES" ]; then
                restart_service "$svc"
            fi
        fi
    done

    # 2. Health checks (mode-dependent)
    mode=$(get_watchdog_mode)
    check_health "$mode"

    # 3. Write stats for the web UI
    write_stats

    sleep "$INTERVAL"
done
