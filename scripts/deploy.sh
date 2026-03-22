#!/bin/bash
# deploy.sh — build and deploy NDIMon-R to one or all targets
# Usage:
#   ./scripts/deploy.sh           # deploy to all targets
#   ./scripts/deploy.sh local     # build and install on this machine only
#   ./scripts/deploy.sh test5b    # deploy to test 5B (10.25.0.168)
#   ./scripts/deploy.sh 4c        # deploy to 4C (10.25.0.149)
#   ./scripts/deploy.sh rpi       # deploy to Raspberry Pi (10.25.0.132)
#   ./scripts/deploy.sh all       # deploy to all remote targets
#   ./scripts/deploy.sh rpi --setup  # first-time full setup on the Pi
set -e

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BUILD_DIR="$REPO_DIR/build"

# ── helpers ──────────────────────────────────────────────────────────────────
log()  { echo "▶  $*"; }
ok()   { echo "✓  $*"; }
fail() { echo "✗  $*" >&2; exit 1; }

# Push source tree to a remote host (fixed --exclude placement)
push_source() {
    local USER="$1" HOST="$2" WORK="$3"
    tar --exclude='api/node_modules' --exclude='.git' --exclude='build' \
        -C "$REPO_DIR" -czf - \
        src/ api/ finder/ CMakeLists.txt \
        scripts/ systemd/ config/ \
        | sshpass -p radxa ssh "${USER}@${HOST}" \
            "mkdir -p '$WORK' && tar xzf - -C '$WORK'"
}

build_local() {
    log "Building on $(hostname)…"
    touch "$REPO_DIR"/src/*.cpp "$REPO_DIR"/src/*.h
    make -C "$BUILD_DIR" -j"$(nproc)"
    ok "Build complete"
}

install_local() {
    log "Installing on $(hostname)…"
    pkill ndimon-r || true
    sleep 1
    cp "$BUILD_DIR/ndimon-r" /usr/local/bin/ndimon-r.new
    mv /usr/local/bin/ndimon-r.new /usr/local/bin/ndimon-r
    # Detect systemd scope (user vs system)
    if systemctl --user is-active ndimon-r &>/dev/null || systemctl --user is-enabled ndimon-r &>/dev/null; then
        systemctl --user restart ndimon-r
        systemctl --user restart ndimon-api 2>/dev/null || true
    else
        systemctl restart ndimon-r
        systemctl restart ndimon-api 2>/dev/null || true
    fi
    ok "Installed on $(hostname)"
}

# Standard remote deploy (root SSH, existing cmake build dir)
# $4 (optional): extra cmake -D flags to pass on reconfigure, e.g. "-DENABLE_MPP=ON"
deploy_remote() {
    local HOST="$1"
    local WORK="$2"
    local SYSTEMD="$3"   # "user" or "system"
    local CMAKE_EXTRA="${4:-}"
    log "Deploying to $HOST ($WORK)…"

    # Push source and rebuild
    push_source root "$HOST" "$WORK"
    if [ -n "$CMAKE_EXTRA" ]; then
        log "  Re-running cmake with $CMAKE_EXTRA…"
        sshpass -p radxa ssh "root@$HOST" \
            "cmake -S '$WORK' -B '$WORK/build' $CMAKE_EXTRA 2>&1 | tail -5"
    fi
    sshpass -p radxa ssh "root@$HOST" \
        "touch '$WORK'/src/*.cpp '$WORK'/src/*.h && \
         make -C '$WORK/build' -j\$(nproc) 2>&1 | tail -5"

    # Atomic binary replace + service restart
    if [ "$SYSTEMD" = "user" ]; then
        sshpass -p radxa ssh "root@$HOST" \
            "pkill ndimon-r || true; sleep 1; \
             cp '$WORK/build/ndimon-r' /usr/local/bin/ndimon-r.new && \
             mv /usr/local/bin/ndimon-r.new /usr/local/bin/ndimon-r && \
             systemctl --user restart ndimon-r && \
             systemctl --user restart ndimon-api 2>/dev/null || true"
    else
        sshpass -p radxa ssh "root@$HOST" \
            "pkill ndimon-r || true; sleep 1; \
             cp '$WORK/build/ndimon-r' /usr/local/bin/ndimon-r.new && \
             mv /usr/local/bin/ndimon-r.new /usr/local/bin/ndimon-r && \
             systemctl restart ndimon-r && \
             systemctl restart ndimon-api 2>/dev/null || true"
    fi
    ok "Deployed to $HOST"
}

# ── Raspberry Pi deploy (SSH as radxa, sudo for privileged ops) ──────────────
RPi_HOST="10.25.0.132"
RPi_WORK="/home/radxa/NDIMon-R"
RPi_USER="radxa"

# rpi_ssh — run commands on the Pi as radxa (with sudo available)
rpi_ssh() { sshpass -p radxa ssh "${RPi_USER}@${RPi_HOST}" "$@"; }
rpi_sudo() { rpi_ssh "sudo bash -c '$*'"; }

deploy_rpi_setup() {
    log "First-time setup on Raspberry Pi ${RPi_HOST}…"

    # 1. Push all source files
    log "  Syncing source files…"
    push_source "$RPi_USER" "$RPi_HOST" "$RPi_WORK"

    # 2. Install system dependencies (as root)
    log "  Installing dependencies (this may take a few minutes)…"
    rpi_ssh "sudo bash '$RPi_WORK/scripts/setup-deps.sh' 2>&1"

    # 3. Configure + build
    log "  Configuring cmake…"
    rpi_ssh "mkdir -p '$RPi_WORK/build' && \
             cmake -B '$RPi_WORK/build' \
                   -DCMAKE_BUILD_TYPE=Release \
                   -DCMAKE_INSTALL_PREFIX=/usr/local \
                   -S '$RPi_WORK' 2>&1 | tail -20"

    log "  Building…"
    rpi_ssh "make -C '$RPi_WORK/build' -j\$(nproc) 2>&1 | tail -10"

    # 4. Install (service files, config, binary)
    log "  Installing services…"
    rpi_ssh "sudo bash '$RPi_WORK/scripts/install.sh' 2>&1"

    ok "Raspberry Pi setup complete!"
    rpi_ssh "sudo systemctl is-active ndimon-r ndimon-api ndimon-finder 2>/dev/null" || true
}

deploy_rpi() {
    log "Deploying update to Raspberry Pi ${RPi_HOST}…"

    # Push source and rebuild
    push_source "$RPi_USER" "$RPi_HOST" "$RPi_WORK"
    rpi_ssh "touch '$RPi_WORK'/src/*.cpp '$RPi_WORK'/src/*.h && \
             make -C '$RPi_WORK/build' -j\$(nproc) 2>&1 | tail -5"

    # Swap binary and restart
    rpi_ssh "pkill ndimon-r; sleep 1; \
             sudo cp '$RPi_WORK/build/ndimon-r' /usr/local/bin/ndimon-r.new && \
             sudo mv /usr/local/bin/ndimon-r.new /usr/local/bin/ndimon-r && \
             sudo systemctl restart ndimon-r && \
             sudo systemctl restart ndimon-api 2>/dev/null || true"
    ok "Deployed to Raspberry Pi ${RPi_HOST}"
}

# ── targets ──────────────────────────────────────────────────────────────────
do_local()  { build_local && install_local; }
do_test5b() { deploy_remote 10.25.0.168 /home/radxa/NDIMon-R user; }
do_4c()     { deploy_remote 10.25.0.149 /root/NDIMon-R system "-DENABLE_MPP=ON"; }
do_rpi()    {
    if [ "${2:-}" = "--setup" ] || ! rpi_ssh "test -f '$RPi_WORK/build/ndimon-r'" 2>/dev/null; then
        deploy_rpi_setup
    else
        deploy_rpi
    fi
}
do_all()    {
    do_test5b &
    do_4c &
    do_rpi &
    wait
}

# ── main ─────────────────────────────────────────────────────────────────────
TARGET="${1:-all}"
case "$TARGET" in
    local)   do_local ;;
    test5b)  do_test5b ;;
    4c)      do_4c ;;
    rpi)     do_rpi "$@" ;;
    all|"")  do_all ;;
    *) fail "Unknown target: $TARGET. Use: local | test5b | 4c | rpi | all" ;;
esac

log "Done."
