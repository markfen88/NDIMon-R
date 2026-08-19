#!/bin/bash
# Privileged helper for ndimon-api. Invoked as root via sudo -n from the
# dedicated ndimon user. Only the subcommands below are allowed.
set -euo pipefail
umask 022

cmd="${1:-}"
shift || true

valid_hostname() {
    [[ ${#1} -ge 1 && ${#1} -le 63 ]] || return 1
    [[ "$1" =~ ^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?$ ]]
}

valid_ntp() {
    local s="$1"
    [[ -z "$s" ]] && return 0
    [[ ${#s} -le 253 ]] || return 1
    if [[ "$s" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
        local IFS=.
        local o
        for o in $s; do
            [[ "$o" -ge 0 && "$o" -le 255 ]] || return 1
        done
        return 0
    fi
    [[ "$s" =~ ^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?)*$ ]]
}

# Write or remove a systemd timer that reboots at a local wall-clock time.
# enabled: on|off
# time:    HH:MM (24h)
# days:    daily  OR  comma-separated systemd weekday names (Sun,Mon,...)
apply_reboot_schedule() {
    local enabled="${1:-off}"
    local time="${2:-}"
    local days="${3:-}"
    local svc=/etc/systemd/system/ndimon-scheduled-reboot.service
    local tmr=/etc/systemd/system/ndimon-scheduled-reboot.timer

    if [[ "$enabled" != "on" ]]; then
        systemctl disable --now ndimon-scheduled-reboot.timer 2>/dev/null || true
        rm -f "$svc" "$tmr"
        systemctl daemon-reload
        return 0
    fi

    [[ "$time" =~ ^([01][0-9]|2[0-3]):[0-5][0-9]$ ]] || { echo "invalid time" >&2; exit 2; }
    local hh="${time%%:*}"
    local mm="${time##*:}"
    local calendar
    if [[ "$days" == "daily" ]]; then
        calendar="*-*-* ${hh}:${mm}:00"
    else
        [[ -n "$days" ]] || { echo "no days" >&2; exit 2; }
        local IFS=,
        local d seen=" " cal_days=""
        for d in $days; do
            case "$d" in
                Sun|Mon|Tue|Wed|Thu|Fri|Sat) ;;
                *) echo "invalid day" >&2; exit 2 ;;
            esac
            [[ "$seen" == *" $d "* ]] && { echo "duplicate day" >&2; exit 2; }
            seen+=" $d "
            if [[ -n "$cal_days" ]]; then cal_days+=","; fi
            cal_days+="$d"
        done
        calendar="${cal_days} *-*-* ${hh}:${mm}:00"
    fi

    cat > "$svc" <<'EOF'
[Unit]
Description=NDIMon-R scheduled reboot

[Service]
Type=oneshot
ExecStart=/sbin/reboot
EOF

    cat > "$tmr" <<EOF
[Unit]
Description=NDIMon-R scheduled reboot timer

[Timer]
OnCalendar=${calendar}
Persistent=false
AccuracySec=1min

[Install]
WantedBy=timers.target
EOF

    systemctl daemon-reload
    systemctl enable --now ndimon-scheduled-reboot.timer
}

apply_ntp() {
    local server="$1"
    local dropin_dir=/etc/systemd/timesyncd.conf.d
    local dropin="$dropin_dir/ndimon.conf"
    local chrony_dir=/etc/chrony/sources.d
    local chrony="$chrony_dir/ndimon.sources"
    if [[ -n "$server" ]]; then
        mkdir -p "$dropin_dir"
        printf '[Time]\nNTP=%s\n' "$server" > "$dropin"
        if [[ -e /etc/chrony/chrony.conf || -x /usr/sbin/chronyd ]]; then
            mkdir -p "$chrony_dir"
            printf 'server %s iburst\n' "$server" > "$chrony"
        fi
    else
        rm -f "$dropin" "$chrony"
    fi
    timedatectl set-ntp true || true
    if [[ -e /etc/chrony/chrony.conf || -x /usr/sbin/chronyd ]]; then
        systemctl enable --now chrony 2>/dev/null || systemctl enable --now chronyd 2>/dev/null || true
        systemctl try-restart chrony 2>/dev/null || systemctl try-restart chronyd 2>/dev/null || true
    else
        systemctl enable --now systemd-timesyncd || true
        systemctl try-restart systemd-timesyncd || true
    fi
}

case "$cmd" in
    reboot)
        exec /sbin/reboot
        ;;
    hostname)
        name="${1:-}"
        valid_hostname "$name" || { echo "invalid hostname" >&2; exit 2; }
        exec hostnamectl set-hostname "$name"
        ;;
    restart-finder)
        systemctl restart ndimon-finder.service
        ;;
    restart-stack)
        systemctl restart ndimon-r.service ndimon-finder.service ndimon-api.service ndimon-watchdog.service
        ;;
    restart-service)
        svc="${1:-}"
        case "$svc" in
            ndimon-r|ndimon-finder|ndimon-api|ndimon-watchdog) ;;
            *) echo "unknown service" >&2; exit 2 ;;
        esac
        systemctl restart "${svc}.service"
        ;;
    set-ntp)
        server="${1:-}"
        valid_ntp "$server" || { echo "invalid ntp host" >&2; exit 2; }
        apply_ntp "$server"
        ;;
    set-reboot-schedule)
        apply_reboot_schedule "${1:-off}" "${2:-}" "${3:-}"
        ;;
    *)
        echo "usage: ndimon-priv reboot|hostname <name>|restart-finder|restart-stack|restart-service <svc>|set-ntp [host]|set-reboot-schedule on|off [HH:MM] [daily|Sun,Mon,...]" >&2
        exit 2
        ;;
esac
