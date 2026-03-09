#!/bin/bash
# NDIMon-R Installation Script
# Supports: Radxa Rock 5B (RK3588), Radxa Rock Pi 4B+ (RK3399), Raspberry Pi 5
# Tested on: Armbian Bookworm, Radxa Debian 12
# Run as root: sudo bash install.sh
set -e

###############################################################################
# CONFIG
###############################################################################
INSTALL_DIR="/opt/ndi-monitor"
REPO_URL="https://github.com/markfen88/NDIMon-R.git"
SERVICE_NAME="ndi-monitor"
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

if grep -qi "rk3588" /proc/cpuinfo /sys/firmware/devicetree/base/model 2>/dev/null; then
    BOARD="rk3588"
    SoC="RK3588"
elif grep -qi "rk3399" /proc/cpuinfo /sys/firmware/devicetree/base/model 2>/dev/null; then
    BOARD="rk3399"
    SoC="RK3399"
elif grep -qi "raspberry pi 5" /proc/cpuinfo /sys/firmware/devicetree/base/model 2>/dev/null; then
    BOARD="rpi5"
    SoC="BCM2712"
fi

ARCH=$(uname -m)
info "Board: ${BOARD} | SoC: ${SoC} | Arch: ${ARCH}"

###############################################################################
# NDI SDK CHECK — must be installed manually
###############################################################################
section "Checking NDI SDK"

NDI_LIB_PATH=""
for p in /usr/local/lib/libndi.so.6 /usr/lib/libndi.so.6; do
    [[ -f "$p" ]] && NDI_LIB_PATH="$p" && break
done

if [[ -z "$NDI_LIB_PATH" ]]; then
    echo ""
    warn "NDI SDK not found. You must install it manually before continuing."
    echo ""
    echo "  1. Go to: https://ndi.video/for-developers/ndi-sdk/"
    echo "  2. Download 'NDI SDK for Linux'"
    echo "  3. Run the installer:  bash Install_NDI_SDK_v6_Linux.sh"
    echo "  4. Copy the aarch64 library:"
    echo "     cp '/usr/local/ndisdk/NDI SDK for Linux/lib/aarch64-rpi4-linux-gnueabi/libndi.so.6.x.x' /usr/local/lib/"
    echo "     ln -sf /usr/local/lib/libndi.so.6.x.x /usr/local/lib/libndi.so.6"
    echo "     ln -sf /usr/local/lib/libndi.so.6 /usr/local/lib/libndi.so"
    echo "     ldconfig"
    echo ""
    echo "  Then re-run this script."
    exit 1
fi

ok "NDI SDK found: $NDI_LIB_PATH"

###############################################################################
# APT DEPENDENCIES
###############################################################################
section "Installing system packages"

apt-get update -qq

# Core GStreamer + KMS output
PKGS=(
    git python3 python3-pip python3-venv
    gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good
    gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-x
    libgstreamer1.0-0 libgstreamer-plugins-base1.0-0
    gstreamer1.0-alsa
    v4l-utils modetest kmod
    cec-utils avahi-utils avahi-daemon
    python3-dev build-essential
)

apt-get install -y --no-install-recommends "${PKGS[@]}" || warn "Some packages may have failed — continuing"

###############################################################################
# ROCKCHIP MPP (hardware video decode)
###############################################################################
if [[ "$BOARD" == "rk3588" || "$BOARD" == "rk3399" ]]; then
    section "Installing Rockchip MPP (hardware decode)"

    # Add Radxa repo if not present
    if ! apt-cache show librockchip-mpp1 &>/dev/null; then
        info "Adding Radxa package repository..."
        apt-get install -y curl gnupg
        curl -fsSL https://apt.radxa.com/bookworm/public.key | gpg --dearmor -o /usr/share/keyrings/radxa-archive-keyring.gpg
        if [[ "$BOARD" == "rk3588" ]]; then
            echo "deb [signed-by=/usr/share/keyrings/radxa-archive-keyring.gpg] https://apt.radxa.com/bookworm-stable/ bookworm main" \
                > /etc/apt/sources.list.d/radxa.list
        else
            echo "deb [signed-by=/usr/share/keyrings/radxa-archive-keyring.gpg] https://apt.radxa.com/bookworm-stable/ bookworm main" \
                > /etc/apt/sources.list.d/radxa.list
        fi
        apt-get update -qq
    fi

    apt-get install -y librockchip-mpp1 librockchip-mpp-dev librockchip-vpu0 gstreamer1.0-rockchip1 || \
        warn "Rockchip MPP packages not available — hardware decode disabled"
fi

###############################################################################
# gst-plugin-ndi
###############################################################################
section "Installing gst-plugin-ndi"

GST_PLUGIN_DIR="/lib/${ARCH}-linux-gnu/gstreamer-1.0"
[[ "$ARCH" == "aarch64" ]] || GST_PLUGIN_DIR="/usr/lib/${ARCH}-linux-gnu/gstreamer-1.0"
NDI_PLUGIN="$GST_PLUGIN_DIR/libgstndi.so"

if [[ ! -f "$NDI_PLUGIN" ]]; then
    info "Downloading pre-built gst-plugin-ndi for aarch64..."
    mkdir -p "$GST_PLUGIN_DIR"
    # Pre-built binary from teltek/gst-plugin-ndi (NDI SDK v6 compatible)
    # Source: https://github.com/teltek/gst-plugin-ndi
    GST_NDI_URL="https://github.com/teltek/gst-plugin-ndi/releases/download/v0.13.0/libgstndi-aarch64.so"
    if curl -fsSL "$GST_NDI_URL" -o "$NDI_PLUGIN" 2>/dev/null; then
        chmod 644 "$NDI_PLUGIN"
        ok "gst-plugin-ndi installed"
    else
        warn "Could not auto-download gst-plugin-ndi. Install manually:"
        echo "  See: https://github.com/teltek/gst-plugin-ndi/releases"
        echo "  Copy to: $NDI_PLUGIN"
    fi
else
    ok "gst-plugin-ndi already installed: $NDI_PLUGIN"
fi

###############################################################################
# NDI LIBRARY SYMLINKS
###############################################################################
section "Setting up NDI library symlinks"

NDI_VER=$(ls /usr/local/lib/libndi.so.*.* 2>/dev/null | head -1 | grep -oP '\d+\.\d+\.\d+')
if [[ -n "$NDI_VER" ]]; then
    ln -sf "/usr/local/lib/libndi.so.${NDI_VER}" /usr/local/lib/libndi.so.6 2>/dev/null || true
    ln -sf /usr/local/lib/libndi.so.6 /usr/local/lib/libndi.so 2>/dev/null || true
    ldconfig
    ok "NDI library symlinks set (v${NDI_VER})"
fi

###############################################################################
# CLONE / UPDATE REPO
###############################################################################
section "Setting up NDIMon-R application"

if [[ -d "$INSTALL_DIR/.git" ]]; then
    info "Updating existing installation..."
    git -C "$INSTALL_DIR" pull origin master
else
    info "Cloning NDIMon-R..."
    git clone "$REPO_URL" "$INSTALL_DIR"
fi

###############################################################################
# PYTHON VIRTUALENV
###############################################################################
section "Setting up Python environment"

if [[ ! -d "$INSTALL_DIR/venv" ]]; then
    python3 -m venv "$INSTALL_DIR/venv"
fi

"$INSTALL_DIR/venv/bin/pip" install --upgrade pip -q
"$INSTALL_DIR/venv/bin/pip" install -q \
    flask flask-login gunicorn pillow psutil pyserial requests python-dotenv

ok "Python packages installed"

###############################################################################
# DIRECTORIES & PERMISSIONS
###############################################################################
section "Creating directories"

mkdir -p "$INSTALL_DIR/logs" "$INSTALL_DIR/uploads" "$INSTALL_DIR/recordings"
chmod 755 "$INSTALL_DIR/update.sh" "$INSTALL_DIR/install.sh"

###############################################################################
# SYSTEMD SERVICE
###############################################################################
section "Installing systemd service"

GST_PLUGIN_PATH_ENV="/usr/lib/${ARCH}-linux-gnu/gstreamer-1.0"
[[ "$ARCH" == "aarch64" ]] && GST_PLUGIN_PATH_ENV="/lib/aarch64-linux-gnu/gstreamer-1.0:/usr/lib/aarch64-linux-gnu/gstreamer-1.0"

cat > /etc/systemd/system/${SERVICE_NAME}.service << EOF
[Unit]
Description=NDIMon-R NDI Monitor Appliance
After=network-online.target avahi-daemon.service
Wants=network-online.target avahi-daemon.service

[Service]
Type=simple
User=root
WorkingDirectory=${INSTALL_DIR}
Environment=PYTHONUNBUFFERED=1
Environment=GST_PLUGIN_PATH=${GST_PLUGIN_PATH_ENV}
Environment=LD_LIBRARY_PATH=/usr/local/ndisdk/lib/aarch64-linux-gnu:/usr/local/lib
ExecStartPre=/bin/sh -c 'printf "\033[2J\033[H\033[?25l" > /dev/tty1 2>/dev/null; true'
ExecStart=${INSTALL_DIR}/venv/bin/python3 app.py
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
systemctl enable ${SERVICE_NAME}
ok "Service installed and enabled"

###############################################################################
# AVAHI (mDNS for NDI discovery)
###############################################################################
section "Ensuring Avahi mDNS is running"
systemctl enable avahi-daemon 2>/dev/null || true
systemctl start  avahi-daemon 2>/dev/null || true

###############################################################################
# DONE
###############################################################################
section "Installation complete"

# Get IP
IP=$(hostname -I | awk '{print $1}')

echo ""
echo -e "${GREEN}╔══════════════════════════════════════════════╗${NC}"
echo -e "${GREEN}║         NDIMon-R installed successfully!     ║${NC}"
echo -e "${GREEN}╚══════════════════════════════════════════════╝${NC}"
echo ""
echo -e "  Web UI:    ${CYAN}http://${IP}:8080${NC}"
echo ""
echo -e "  Start:     ${YELLOW}systemctl start ${SERVICE_NAME}${NC}"
echo -e "  Status:    ${YELLOW}systemctl status ${SERVICE_NAME}${NC}"
echo -e "  Logs:      ${YELLOW}tail -f ${INSTALL_DIR}/logs/app.log${NC}"
echo -e "  Update:    ${YELLOW}${INSTALL_DIR}/update.sh${NC}"
echo ""

if [[ "$BOARD" != "generic" ]]; then
    ok "Hardware: ${SoC} — hardware decode enabled"
else
    warn "Generic board detected — software decode only (no Rockchip MPP)"
fi

echo ""
read -rp "Start NDIMon-R now? [Y/n] " START
if [[ "${START,,}" != "n" ]]; then
    systemctl start ${SERVICE_NAME}
    sleep 2
    systemctl is-active ${SERVICE_NAME} && ok "NDIMon-R is running at http://${IP}:8080" || \
        warn "Service may have failed — check: journalctl -u ${SERVICE_NAME} -n 30"
fi
