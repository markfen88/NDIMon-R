#!/bin/bash
# NDIMon-R Watchdog — independent service that monitors all NDIMon components
# and restarts them if they become unresponsive.
#
# Checks every 15 seconds using systemd process state (not HTTP — avoids
# false positives under CPU load). Only restarts if the process is actually
# dead/failed, not just slow.
#
# Each service gets 5 consecutive failures before restart (75s tolerance).
# After restart, a 60s cooldown prevents restart storms.

set -euo pipefail

INTERVAL=15
MAX_FAILURES=5
COOLDOWN=60

declare -A fail_count
declare -A last_restart

for svc in ndimon-r ndimon-api ndimon-finder; do
    fail_count[$svc]=0
    last_restart[$svc]=0
done

log() { echo "[$(date '+%H:%M:%S')] [watchdog] $*"; }

# Check if a service's main process is alive. Uses systemctl + /proc
# rather than HTTP to avoid false positives under heavy CPU load.
check_service() {
    local svc=$1

    # 1. Is systemd happy with it?
    if ! systemctl is-active --quiet "${svc}.service" 2>/dev/null; then
        return 1
    fi

    # 2. Is the main PID actually alive?
    local pid
    pid=$(systemctl show -p MainPID --value "${svc}.service" 2>/dev/null)
    if [ -z "$pid" ] || [ "$pid" = "0" ]; then
        return 1
    fi
    [ -d "/proc/$pid" ] && return 0
    return 1
}

check_ndimon_r()      { check_service ndimon-r; }
check_ndimon_api()    { check_service ndimon-api; }
check_ndimon_finder() { check_service ndimon-finder; }

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

# Tell systemd we're ready
systemd-notify --ready 2>/dev/null || true

while true; do
    for svc in ndimon-r ndimon-api ndimon-finder; do
        if "check_$svc" 2>/dev/null; then
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

    # Watchdog ping to systemd
    if [ -n "${NOTIFY_SOCKET:-}" ]; then
        systemd-notify WATCHDOG=1
    fi

    sleep "$INTERVAL"
done
