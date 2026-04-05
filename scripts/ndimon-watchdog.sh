#!/bin/bash
# NDIMon-R Watchdog — independent service that monitors all NDIMon components
# and restarts them if they become unresponsive.
#
# Checks every 10 seconds:
#   1. ndimon-r      — must respond to IPC health check within 5s
#   2. ndimon-api    — must respond to HTTP /api/health within 5s
#   3. ndimon-finder — must be running (systemd active state)
#
# Each service gets 3 consecutive failures before restart (30s tolerance).
# After restart, a 30s cooldown prevents restart storms.

set -euo pipefail

INTERVAL=10
MAX_FAILURES=3
COOLDOWN=30

declare -A fail_count
declare -A last_restart

for svc in ndimon-r ndimon-api ndimon-finder; do
    fail_count[$svc]=0
    last_restart[$svc]=0
done

log() { echo "[$(date '+%H:%M:%S')] [watchdog] $*"; }

check_ndimon_r() {
    # Try IPC health check via the API (fastest path)
    local resp
    resp=$(curl -s --max-time 5 http://127.0.0.1/api/health 2>/dev/null) || return 1
    echo "$resp" | grep -q '"alive":true' && return 0
    # API might be down but ndimon-r might be fine — check process directly
    systemctl is-active --quiet ndimon-r.service && return 0
    return 1
}

check_ndimon_api() {
    # HTTP health check — if this fails, the web UI is down
    curl -s --max-time 5 -o /dev/null http://127.0.0.1/api/health 2>/dev/null
}

check_ndimon_finder() {
    systemctl is-active --quiet ndimon-finder.service
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
}

log "starting — monitoring ndimon-r, ndimon-api, ndimon-finder"

while true; do
    for svc in ndimon-r ndimon-api ndimon-finder; do
        if "check_$svc" 2>/dev/null; then
            if [ "${fail_count[$svc]}" -gt 0 ]; then
                log "$svc: recovered (was at ${fail_count[$svc]} failures)"
            fi
            fail_count[$svc]=0
        else
            fail_count[$svc]=$((${fail_count[$svc]} + 1))
            log "$svc: health check FAILED (${fail_count[$svc]}/$MAX_FAILURES)"

            if [ "${fail_count[$svc]}" -ge "$MAX_FAILURES" ]; then
                restart_service "$svc"
            fi
        fi
    done

    # Watchdog ping to systemd
    if [ -n "${NOTIFY_SOCKET:-}" ]; then
        systemd-notify WATCHDOG=1
    fi

    sleep "$INTERVAL"
done
