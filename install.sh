#!/bin/bash
# NDIMon-R Installation Script
# Supports: Radxa Rock 5B (RK3588), Radxa Rock Pi 4B+ (RK3399), Rock 4C (RK3399), Raspberry Pi 5
# Tested on: Armbian Bookworm, Radxa Debian 12
# Run as root: sudo bash install.sh
#
# Optional env overrides:
#   REPO_URL=git@myserver:/path/ndimon-r.git bash install.sh
#   INSTALL_DIR=/opt/ndimon-r bash install.sh
set -e

###############################################################################
# CONFIG
###############################################################################
INSTALL_DIR="${INSTALL_DIR:-/opt/ndimon-r}"
REPO_URL="${REPO_URL:-https://github.com/markfen88/NDIMon-R.git}"
SERVICE_NAME="ndimon-r"
OLD_SERVICE_NAME="ndi-monitor"   # migrate from previous installs
PYTHON_MIN="3.10"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info()    { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()      { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error()   { echo -e "${RED}[ERROR]${NC} $*"; exit 1; }
section() { echo -e "\n${CYAN}━━━ $* ━━━${NC}"; }

###############################################################################
# ROOT CHECK
###############################################################################
[[ $EUID -ne 0 ]] && error "Run as root: sudo bash install.sh"

###############################################################################
# BOARD DETECTION
###############################################################################
section "Detecting hardware"

BOARD="generic"
SoC="unknown"
BOARD_NAME="Unknown"

# Use compatible string (null-separated) which reliably contains SoC ID
DT_MODEL=$(tr -d "\0" < /sys/firmware/devicetree/base/model 2>/dev/null || true)
DT_COMPAT=$(tr "\0" "\n" < /sys/firmware/devicetree/base/compatible 2>/dev/null || true)
CPUINFO=$(cat /proc/cpuinfo 2>/dev/null || true)
DETECT_SRC="$DT_MODEL $DT_COMPAT $CPUINFO"

if echo "$DETECT_SRC" | grep -qi "rk3588"; then
    BOARD="rk3588"; SoC="RK3588"
    echo "$DT_MODEL" | grep -qi "rock 5b" && BOARD_NAME="Rock 5B" || BOARD_NAME="RK3588 board"
elif echo "$DETECT_SRC" | grep -qi "rk3399"; then
    BOARD="rk3399"; SoC="RK3399"
    if echo "$DT_MODEL" | grep -qi "rock.*4c\|rockpi4c"; then
        BOARD_NAME="Rock 4C"
    elif echo "$DT_MODEL" | grep -qi "rock pi 4\|rockpi 4\|rock 4b"; then
        BOARD_NAME="Rock Pi 4B+"
    else
        BOARD_NAME="RK3399 board"
    fi
elif echo "$DETECT_SRC" | grep -qi "raspberry pi 5"; then
    BOARD="rpi5"; SoC="BCM2712"; BOARD_NAME="Raspberry Pi 5"
fi

ARCH=$(uname -m)
info "Board: ${BOARD_NAME} | SoC: ${SoC} | Arch: ${ARCH}"
[[ "$ARCH" != "aarch64" ]] && warn "Unsupported architecture: ${ARCH}. Only aarch64 is tested."

###############################################################################
# NDI SDK — must be pre-installed; script will auto-configure symlinks
###############################################################################
section "Setting up NDI SDK"

NDI_SDK_INSTALL_DIR="/usr/local/ndisdk"
NDI_LIB_DEST="/usr/local/lib"
NDI_LIB_FOUND=""

# Look for already-copied versioned lib
for candidate in "${NDI_LIB_DEST}"/libndi.so.*.* "${NDI_SDK_INSTALL_DIR}/NDI SDK for Linux/lib/aarch64-linux-gnu"/libndi.so.*.* \
                 "${NDI_SDK_INSTALL_DIR}/NDI SDK for Linux/lib/aarch64-rpi4-linux-gnueabi"/libndi.so.*.*; do
    # shellcheck disable=SC2086
    f=$(ls $candidate 2>/dev/null | head -1)
    [[ -n "$f" && -f "$f" ]] && NDI_LIB_FOUND="$f" && break
done

if [[ -z "$NDI_LIB_FOUND" ]]; then
    # Look for NDI SDK installer in common locations
    NDI_INSTALLER=""
    for search_dir in . "$HOME" /tmp /root /home; do
        f=$(find "$search_dir" -maxdepth 2 -name "Install_NDI_SDK*Linux*.sh" 2>/dev/null | head -1)
        [[ -n "$f" ]] && NDI_INSTALLER="$f" && break
    done

    if [[ -n "$NDI_INSTALLER" ]]; then
        info "Found NDI SDK installer: $NDI_INSTALLER — running..."
        bash "$NDI_INSTALLER"
        # Re-check after install
        for candidate in "${NDI_SDK_INSTALL_DIR}/NDI SDK for Linux/lib/aarch64-linux-gnu"/libndi.so.*.* \
                         "${NDI_SDK_INSTALL_DIR}/NDI SDK for Linux/lib/aarch64-rpi4-linux-gnueabi"/libndi.so.*.*; do
            f=$(ls $candidate 2>/dev/null | head -1)
            [[ -n "$f" && -f "$f" ]] && NDI_LIB_FOUND="$f" && break
        done
        [[ -z "$NDI_LIB_FOUND" ]] && error "NDI SDK installer ran but library not found. Check SDK for your architecture."
    else
        echo ""
        warn "NDI SDK not found. Install it first:"
        echo "  1. Download from: https://ndi.video/for-developers/ndi-sdk/"
        echo "  2. Run:  bash Install_NDI_SDK_v6_Linux.sh"
        echo "  3. Re-run this script."
        echo ""
        echo "  The installer places files under /usr/local/ndisdk/"
        exit 1
    fi
fi

ok "NDI SDK library found: $NDI_LIB_FOUND"

# Set up symlinks so ctypes can find libndi.so.6
LIB_FILE=$(basename "$NDI_LIB_FOUND")
if [[ "$NDI_LIB_FOUND" != "${NDI_LIB_DEST}/${LIB_FILE}" ]]; then
    cp -f "$NDI_LIB_FOUND" "${NDI_LIB_DEST}/"
fi
ln -sf "${NDI_LIB_DEST}/${LIB_FILE}" "${NDI_LIB_DEST}/libndi.so.6"
ln -sf "${NDI_LIB_DEST}/libndi.so.6" "${NDI_LIB_DEST}/libndi.so"
ldconfig
ok "NDI symlinks: ${LIB_FILE} → libndi.so.6 → libndi.so"

###############################################################################
# APT DEPENDENCIES
###############################################################################
section "Installing system packages"

apt-get update -qq

PKGS=(
    # Python runtime
    python3 python3-pip python3-venv python3-dev
    # Build tools
    git build-essential curl
    # GStreamer core + plugins
    gstreamer1.0-tools
    gstreamer1.0-plugins-base
    gstreamer1.0-plugins-good
    gstreamer1.0-plugins-bad
    gstreamer1.0-plugins-ugly
    gstreamer1.0-x
    gstreamer1.0-alsa
    libgstreamer1.0-0
    libgstreamer-plugins-base1.0-0
    # KMS/DRM tools (modetest for display detection)
    libdrm-tests
    # V4L2 (video device utilities)
    v4l-utils
    # Kernel modules
    kmod
    # HDMI CEC control
    cec-utils
    # mDNS (NDI source discovery)
    avahi-utils
    avahi-daemon
    # Networking
    iproute2
    netplan.io
)

apt-get install -y --no-install-recommends "${PKGS[@]}" \
    || warn "Some packages may have failed — continuing"

# modetest may be in libdrm-tests or standalone — verify
command -v modetest &>/dev/null || warn "modetest not found — display detection may be limited"

###############################################################################
# ROCKCHIP MPP (hardware video decode — RK3588 / RK3399)
###############################################################################
if [[ "$BOARD" == "rk3588" || "$BOARD" == "rk3399" ]]; then
    section "Installing Rockchip MPP (hardware decode)"

    if ! apt-cache show librockchip-mpp1 &>/dev/null; then
        info "Adding Radxa board-specific MPP repository..."
        apt-get install -y --no-install-recommends curl gnupg

        # Board-specific repos use "bookworm" suite regardless of host OS version.
        # rk3399-bookworm and rk3588-bookworm repos work on Bookworm and Trixie hosts.
        if [[ "$BOARD" == "rk3588" ]]; then
            RADXA_REPO_BASE="rk3588-bookworm"
        else
            RADXA_REPO_BASE="rk3399-bookworm"
        fi

        # Fetch Radxa GPG keyring from radxa-repo.github.io (primary CDN)
        KEYRING_DEST="/usr/share/keyrings/radxa-archive-keyring.gpg"
        KEYRING_FETCHED=0

        # Try multiple key URLs — the .gpg is already binary (no gpg --dearmor needed)
        for KEY_URL in \
            "https://radxa-repo.github.io/radxa-archive-keyring.gpg" \
            "https://radxa-repo.github.io/bookworm/radxa-archive-keyring.gpg" \
            "https://apt.radxa.com/bookworm-stable/public.key"; do
            # Determine if the key is armor (ASCII) or binary
            if curl -fsSL --connect-timeout 15 "$KEY_URL" -o /tmp/radxa.key.tmp 2>/dev/null; then
                if file /tmp/radxa.key.tmp | grep -q "PGP"; then
                    # Binary GPG keyring — copy directly
                    cp /tmp/radxa.key.tmp "$KEYRING_DEST"
                    KEYRING_FETCHED=1
                    info "Radxa keyring (binary) fetched from $KEY_URL"
                    break
                elif file /tmp/radxa.key.tmp | grep -qi "ASCII\|text"; then
                    # ASCII-armored key — dearmor
                    if gpg --batch --yes --dearmor -o "$KEYRING_DEST" < /tmp/radxa.key.tmp 2>/dev/null; then
                        KEYRING_FETCHED=1
                        info "Radxa keyring (ascii) fetched from $KEY_URL"
                        break
                    fi
                fi
            fi
        done
        rm -f /tmp/radxa.key.tmp

        if [[ $KEYRING_FETCHED -eq 1 ]]; then
            # radxa-repo.github.io uses the repo-base name as both path and suite
            echo "deb [signed-by=${KEYRING_DEST}] https://radxa-repo.github.io/${RADXA_REPO_BASE} ${RADXA_REPO_BASE} main" \
                > /etc/apt/sources.list.d/radxa.list
            apt-get update -qq || warn "Radxa repo update failed — hardware decode may be unavailable"
        else
            warn "Could not fetch Radxa keyring — skipping MPP install"
        fi
    fi

    apt-get install -y --no-install-recommends \
        librockchip-mpp1 librockchip-mpp-dev librockchip-vpu0 gstreamer1.0-rockchip1 \
        || warn "Rockchip MPP packages not available — hardware decode may be unavailable"
fi

###############################################################################
# gst-plugin-ndi  (NDI source element for GStreamer)
###############################################################################
section "Installing gst-plugin-ndi"

GST_PLUGIN_DIR="/lib/${ARCH}-linux-gnu/gstreamer-1.0"
NDI_PLUGIN="$GST_PLUGIN_DIR/libgstndi.so"

if [[ ! -f "$NDI_PLUGIN" ]]; then
    info "Downloading pre-built gst-plugin-ndi for ${ARCH}..."
    mkdir -p "$GST_PLUGIN_DIR"
    # Pre-built binary from teltek/gst-plugin-ndi — NDI SDK v6 compatible
    GST_NDI_URL="https://github.com/teltek/gst-plugin-ndi/releases/download/v0.13.0/libgstndi-aarch64.so"
    if curl -fsSL "$GST_NDI_URL" -o "$NDI_PLUGIN"; then
        chmod 644 "$NDI_PLUGIN"
        ok "gst-plugin-ndi installed: $NDI_PLUGIN"
    else
        warn "Could not download gst-plugin-ndi automatically."
        echo "  Manual install: https://github.com/teltek/gst-plugin-ndi/releases"
        echo "  Copy to: $NDI_PLUGIN"
    fi
else
    ok "gst-plugin-ndi already installed: $NDI_PLUGIN"
fi

# Clear GStreamer plugin cache so ndisrc element is found
rm -rf /root/.cache/gstreamer-1.0/ 2>/dev/null || true

###############################################################################
# CLONE / UPDATE REPO
###############################################################################
section "Setting up NDIMon-R application"

if [[ -d "$INSTALL_DIR/.git" ]]; then
    info "Updating existing installation in $INSTALL_DIR..."
    git -C "$INSTALL_DIR" pull origin master
else
    if [[ -d "$INSTALL_DIR" ]]; then
        warn "$INSTALL_DIR exists but is not a git repo — backing up and re-cloning"
        mv "$INSTALL_DIR" "${INSTALL_DIR}.bak.$(date +%s)"
    fi
    info "Cloning NDIMon-R from $REPO_URL..."
    git clone "$REPO_URL" "$INSTALL_DIR"
fi

###############################################################################
# PYTHON VIRTUALENV
###############################################################################
section "Setting up Python environment"

if [[ ! -x "$INSTALL_DIR/venv/bin/pip" ]]; then
    info "Creating Python virtualenv..."
    rm -rf "$INSTALL_DIR/venv"
    python3 -m venv "$INSTALL_DIR/venv"
fi

"$INSTALL_DIR/venv/bin/pip" install --upgrade pip -q
"$INSTALL_DIR/venv/bin/pip" install -q -r "$INSTALL_DIR/requirements.txt"
ok "Python packages installed"

###############################################################################
# DIRECTORIES & PERMISSIONS
###############################################################################
section "Creating directories"

mkdir -p \
    "$INSTALL_DIR/logs" \
    "$INSTALL_DIR/uploads" \
    "$INSTALL_DIR/recordings" \
    "$INSTALL_DIR/backups" \
    "$INSTALL_DIR/static/img"

chmod 755 "$INSTALL_DIR/update.sh" "$INSTALL_DIR/install.sh"

# NDI SDK config directory (app writes ndi-config.v1.json here at startup)
mkdir -p /root/.ndi

ok "Directories created"

###############################################################################
# LOGROTATE
###############################################################################
section "Configuring log rotation"

cat > /etc/logrotate.d/ndimon-r << EOF
${INSTALL_DIR}/logs/*.log {
    daily
    rotate 7
    compress
    delaycompress
    missingok
    notifempty
    copytruncate
}
EOF
ok "Logrotate configured (7 days)"

###############################################################################
# MIGRATE OLD SERVICE  (ndi-monitor → ndimon-r)
###############################################################################
if systemctl list-unit-files "${OLD_SERVICE_NAME}.service" &>/dev/null 2>&1; then
    section "Migrating from old service: ${OLD_SERVICE_NAME}"
    systemctl stop  "${OLD_SERVICE_NAME}.service" 2>/dev/null || true
    systemctl disable "${OLD_SERVICE_NAME}.service" 2>/dev/null || true
    rm -f "/etc/systemd/system/${OLD_SERVICE_NAME}.service"
    ok "Old service ${OLD_SERVICE_NAME} removed"
fi

###############################################################################
# SYSTEMD SERVICE
###############################################################################
section "Installing systemd service: ${SERVICE_NAME}"

# GStreamer plugin paths — include both legacy and standard locations for aarch64
if [[ "$ARCH" == "aarch64" ]]; then
    GST_PLUGIN_PATH_ENV="/lib/aarch64-linux-gnu/gstreamer-1.0:/usr/lib/aarch64-linux-gnu/gstreamer-1.0"
else
    GST_PLUGIN_PATH_ENV="/usr/lib/${ARCH}-linux-gnu/gstreamer-1.0"
fi

# NDI SDK library path — include both SDK install dir and /usr/local/lib
NDI_LIB_PATH_ENV="/usr/local/ndisdk/lib/aarch64-linux-gnu:/usr/local/lib"

cat > "/etc/systemd/system/${SERVICE_NAME}.service" << EOF
[Unit]
Description=NDIMon-R — Rockchip NDI Display Appliance
After=network-online.target avahi-daemon.service
Wants=network-online.target avahi-daemon.service

[Service]
Type=simple
User=root
WorkingDirectory=${INSTALL_DIR}
Environment=PYTHONUNBUFFERED=1
Environment=GST_PLUGIN_PATH=${GST_PLUGIN_PATH_ENV}
Environment=LD_LIBRARY_PATH=${NDI_LIB_PATH_ENV}
ExecStartPre=/bin/sh -c 'printf "\033[2J\033[H\033[?25l" > /dev/tty1 2>/dev/null; true'
ExecStart=${INSTALL_DIR}/venv/bin/gunicorn -w 1 -b 0.0.0.0:8080 --timeout 120 app:app
Restart=always
RestartSec=5
StandardOutput=append:${INSTALL_DIR}/logs/app.log
StandardError=append:${INSTALL_DIR}/logs/error.log
KillMode=mixed
TimeoutStopSec=15

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable "${SERVICE_NAME}"
ok "Service ${SERVICE_NAME} installed and enabled"

###############################################################################
# AVAHI (mDNS — NDI source discovery on LAN)
###############################################################################
section "Enabling Avahi mDNS"
systemctl enable avahi-daemon 2>/dev/null || true
systemctl start  avahi-daemon 2>/dev/null || true
ok "Avahi mDNS running"

###############################################################################
# DONE
###############################################################################
section "Installation complete"

IP=$(hostname -I | awk '{print $1}')

echo ""
echo -e "${GREEN}╔══════════════════════════════════════════════╗${NC}"
echo -e "${GREEN}║      NDIMon-R installed successfully!        ║${NC}"
echo -e "${GREEN}╚══════════════════════════════════════════════╝${NC}"
echo ""
echo -e "  Board:     ${CYAN}${BOARD_NAME} (${SoC})${NC}"
echo -e "  Web UI:    ${CYAN}http://${IP}:8080${NC}"
echo ""
echo -e "  Start:     ${YELLOW}systemctl start ${SERVICE_NAME}${NC}"
echo -e "  Status:    ${YELLOW}systemctl status ${SERVICE_NAME}${NC}"
echo -e "  Logs:      ${YELLOW}tail -f ${INSTALL_DIR}/logs/app.log${NC}"
echo -e "  Update:    ${YELLOW}${INSTALL_DIR}/update.sh${NC}"
echo ""

if [[ "$BOARD" == "rk3588" || "$BOARD" == "rk3399" ]]; then
    ok "Rockchip hardware decode enabled (${SoC})"
elif [[ "$BOARD" == "generic" ]]; then
    warn "Generic board — software decode only (no Rockchip MPP)"
fi

echo ""
read -rp "Start NDIMon-R now? [Y/n] " START
if [[ "${START,,}" != "n" ]]; then
    systemctl start "${SERVICE_NAME}"
    sleep 3
    if systemctl is-active "${SERVICE_NAME}" &>/dev/null; then
        ok "NDIMon-R is running at http://${IP}:8080"
    else
        warn "Service may have failed — check: journalctl -u ${SERVICE_NAME} -n 50"
    fi
fi
