#!/bin/bash
# setup-deps.sh — Install all dependencies for NDIMon-R
# Supports: Ubuntu 24.04 Noble (Armbian), Debian Bookworm/Trixie (Radxa/RPi), Raspberry Pi OS
set -e

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
info()  { echo -e "${BLUE}[deps]${NC}  $*"; }
ok()    { echo -e "${GREEN}[deps]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[deps]${NC}  $*"; }
die()   { echo -e "${RED}[deps]${NC}  $*"; exit 1; }

# Detect OS
CODENAME=$(. /etc/os-release 2>/dev/null && echo "${VERSION_CODENAME:-}")
ID=$(. /etc/os-release 2>/dev/null && echo "${ID:-}")
info "OS: $ID $CODENAME"

# Detect board / SoC
BOARD="unknown"
if grep -qi "rockchip,rk3588" /proc/device-tree/compatible 2>/dev/null; then
    BOARD="rk3588"
elif grep -qi "rockchip,rk3399" /proc/device-tree/compatible 2>/dev/null; then
    BOARD="rk3399"
elif grep -qi "raspberrypi\|brcm,bcm2" /proc/device-tree/compatible 2>/dev/null; then
    BOARD="rpi"
fi
info "Board: $BOARD"

# Detect architecture
ARCH=$(dpkg --print-architecture 2>/dev/null || uname -m)
info "Arch:  $ARCH"

export DEBIAN_FRONTEND=noninteractive

# --- 1. Basic build tools ---
info "Installing build tools..."
apt-get install -yq --no-install-recommends \
    build-essential cmake pkg-config git wget curl ca-certificates \
    libdrm-dev libasound2-dev \
    libavahi-client-dev libavahi-common-dev \
    nlohmann-json3-dev \
    libavcodec-dev libavformat-dev libswscale-dev libavutil-dev \
    2>/dev/null || {
    # Fallback: some packages may differ by distro
    apt-get install -yq --no-install-recommends \
        build-essential cmake pkg-config git wget curl ca-certificates \
        libdrm-dev libasound2-dev \
        libavahi-client-dev libavahi-common-dev \
        libavcodec-dev libavutil-dev libswscale-dev
    # nlohmann-json header-only, can be fetched by cmake if package not found
    apt-get install -yq --no-install-recommends nlohmann-json3-dev 2>/dev/null || true
}
ok "Build tools installed"

# --- 2. Rockchip MPP (only on Rockchip boards) ---
if [[ "$BOARD" == rk3588 || "$BOARD" == rk3399 ]]; then
    if ldconfig -p 2>/dev/null | grep -q librockchip_mpp; then
        ok "Rockchip MPP already installed"
    else
        info "Installing Rockchip MPP..."

        # Install the Radxa archive keyring .deb (replaces the old raw .gpg approach)
        # The keyring package installs yearly-rotated keys to /usr/share/keyrings/
        # Note: GitHub /releases/latest/download/ requires the exact filename; fetch it via the API
        local _keyring_url
        _keyring_url=$(curl -sL https://api.github.com/repos/radxa-pkg/radxa-archive-keyring/releases/latest 2>/dev/null \
            | python3 -c "import sys,json; assets=json.load(sys.stdin).get('assets',[]); \
              print(next((a['browser_download_url'] for a in assets if a['name'].endswith('_all.deb')), ''))" 2>/dev/null)
        RADXA_KEYRING_DEB="${_keyring_url:-https://github.com/radxa-pkg/radxa-archive-keyring/releases/download/0.2.2/radxa-archive-keyring_0.2.2_all.deb}"
        if ! dpkg -s radxa-archive-keyring &>/dev/null; then
            info "Fetching Radxa archive keyring..."
            local _tmpdir; _tmpdir=$(mktemp -d)
            if wget -q -O "$_tmpdir/radxa-keyring.deb" "$RADXA_KEYRING_DEB" 2>/dev/null; then
                dpkg -i "$_tmpdir/radxa-keyring.deb" 2>/dev/null && ok "Radxa keyring installed" || \
                    warn "Radxa keyring install failed"
            else
                warn "Could not download Radxa keyring"
            fi
            rm -rf "$_tmpdir"
        fi

        # Find a valid installed keyring file (yearly rotation: 2024, 2025, 2026 …)
        RADXA_KEY=$(ls /usr/share/keyrings/radxa-archive-keyring-*.gpg 2>/dev/null | sort -V | tail -1)

        if [[ -n "$RADXA_KEY" ]]; then
            # MPP packages live in rk3588-bookworm / rk3399-bookworm regardless of host OS.
            # There is no rk3588-noble repo; the bookworm libs are ABI-compatible with Noble.
            SUITE_SOC="${BOARD}-bookworm"
            RADXA_LIST=/etc/apt/sources.list.d/radxa-rockchip.list
            {
                echo "deb [signed-by=$RADXA_KEY] https://radxa-repo.github.io/bookworm bookworm main"
                echo "deb [signed-by=$RADXA_KEY] https://radxa-repo.github.io/${SUITE_SOC} ${SUITE_SOC} main"
            } > "$RADXA_LIST"

            apt-get update -o Dir::Etc::sourcelist="$RADXA_LIST" \
                -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0 -qq 2>/dev/null || true

            apt-get install -yq --no-install-recommends \
                librockchip-mpp1 librockchip-mpp-dev librockchip-vpu0 \
                librga2 librga-dev 2>/dev/null || \
            apt-get install -yq --no-install-recommends \
                librockchip-mpp1 librockchip-mpp-dev librockchip-vpu0 2>/dev/null || \
            warn "MPP packages unavailable — software decode only"
        else
            warn "Radxa keyring not found — MPP installation skipped (software decode will be used)"
        fi

        # Create librga.pc stub if librga installed but no pkg-config file
        if ldconfig -p 2>/dev/null | grep -q "librga\." && \
           ! pkg-config --exists librga 2>/dev/null; then
            local _rga_so; _rga_so=$(find /usr/lib -name "librga.so*" 2>/dev/null | head -1)
            RGA_LIBDIR=$(dirname "$_rga_so" 2>/dev/null)
            if [[ -n "$RGA_LIBDIR" ]]; then
                cat > /usr/lib/aarch64-linux-gnu/pkgconfig/librga.pc <<RGAEOF
Name: librga
Description: Rockchip RGA 2D accelerator
Version: 2.2.0
Libs: -L${RGA_LIBDIR} -lrga
Cflags: -I/usr/include/rga
RGAEOF
                ok "librga.pc stub created"
            fi
        fi
    fi
fi

# --- 3. NDI SDK v6 ---
install_ndi_sdk() {
    info "Installing NDI SDK v6..."
    local NDI_SDK_URL="https://downloads.ndi.tv/SDK/NDI_SDK_Linux/Install_NDI_SDK_v6_Linux.tar.gz"
    local tmpdir; tmpdir=$(mktemp -d)
    trap "rm -rf $tmpdir" EXIT

    info "Downloading NDI SDK (~60MB)..."
    if ! wget -q --show-progress -O "$tmpdir/ndi.tar.gz" "$NDI_SDK_URL"; then
        warn "NDI SDK download failed"
        return 1
    fi

    tar xzf "$tmpdir/ndi.tar.gz" -C "$tmpdir"
    local installer; installer=$(find "$tmpdir" -name "Install_NDI_SDK*.sh" | head -1)
    [[ -z "$installer" ]] && { warn "NDI installer not found in tarball"; return 1; }

    # Extract embedded archive non-interactively (skips license prompt)
    # By running this installer you accept the NDI SDK License Agreement:
    # https://www.ndi.tv/license
    local archive_line
    archive_line=$(awk '/^__NDI_ARCHIVE_BEGIN__/ { print NR+1; exit 0; }' "$installer")
    [[ -z "$archive_line" ]] && { warn "Cannot find NDI archive marker"; return 1; }

    info "Extracting NDI SDK (archive starts at line $archive_line)..."
    mkdir -p /usr/local/ndisdk
    tail -n+"$archive_line" "$installer" | tar xzf - -C /usr/local/ndisdk

    local ndi_base="/usr/local/ndisdk/NDI SDK for Linux"
    [[ ! -d "$ndi_base" ]] && { warn "NDI extraction failed"; return 1; }

    # Install headers (use -exec dirname to avoid xargs+spaces issue)
    local ndi_inc_file; ndi_inc_file=$(find "$ndi_base" -name "Processing.NDI.Lib.h" | head -1)
    if [[ -n "$ndi_inc_file" ]]; then
        local ndi_inc_dir; ndi_inc_dir=$(dirname "$ndi_inc_file")
        cp -f "$ndi_inc_dir"/Processing.NDI.*.h /usr/local/include/
    fi

    # Install library — search under lib/ to avoid picking up bin/ subdirs
    # Fall back to any dir containing libndi.so if lib/ subdirs not found
    local ndi_lib_dir=""
    local ndi_lib_base="$ndi_base/lib"
    if [[ -d "$ndi_lib_base" ]]; then
        ndi_lib_dir=$(find "$ndi_lib_base" -type d -name "aarch64-rpi4-linux-gnueabi" 2>/dev/null | head -1)
        [[ -z "$ndi_lib_dir" ]] && ndi_lib_dir=$(find "$ndi_lib_base" -type d -name "*aarch64*" 2>/dev/null | head -1)
        [[ -z "$ndi_lib_dir" ]] && ndi_lib_dir=$(find "$ndi_lib_base" -type d -name "*arm64*"   2>/dev/null | head -1)
    fi
    # Final fallback: find the .so file itself and use its parent dir
    if [[ -z "$ndi_lib_dir" ]]; then
        local so_parent; so_parent=$(find "$ndi_base" -name "libndi.so.*.*" 2>/dev/null | head -1)
        [[ -n "$so_parent" ]] && ndi_lib_dir=$(dirname "$so_parent")
    fi

    if [[ -z "$ndi_lib_dir" ]]; then
        warn "NDI aarch64 library not found in extracted SDK"
        return 1
    fi

    local so_full; so_full=$(find "$ndi_lib_dir" -name "libndi.so.*.*" | head -1)
    [[ -z "$so_full" ]] && { warn "libndi.so.*.* not found in $ndi_lib_dir"; return 1; }
    local so_ver; so_ver=$(basename "$so_full")
    # Extract major version number (e.g. libndi.so.6.3.1 → 6)
    local major_num; major_num=$(echo "$so_ver" | grep -oE '[0-9]+' | head -1)
    [[ -z "$major_num" ]] && { warn "Cannot parse NDI SO version from $so_ver"; return 1; }
    local so_major="libndi.so.${major_num}"

    cp -f "$so_full" /usr/local/lib/"$so_ver"
    ln -sf "$so_ver" /usr/local/lib/"$so_major"
    ln -sf "$so_major" /usr/local/lib/libndi.so
    ldconfig
    ok "NDI SDK installed: $so_ver"
}

if [[ -f /usr/local/lib/libndi.so ]]; then
    ok "NDI SDK already present"
else
    install_ndi_sdk || warn "NDI SDK install failed — build will fail without it"
fi

# --- 3b. NDI HX codec compat symlinks (Ubuntu Noble only) ---
# NDI SDK 6.3.1 dlopen()s libavcodec.so.61 + libavutil.so.59 (FFmpeg 7.x) for HX decode.
# Noble ships FFmpeg 6.1 (libavcodec.so.60 / libavutil.so.58). Create compat symlinks so
# HX streams work without upgrading FFmpeg. The NDI SDK only uses stable API symbols
# (avcodec_send_packet, avcodec_receive_frame, etc.) that are ABI-compatible across 6→7.
if [[ "$CODENAME" == "noble" ]]; then
    FFMPEG_LIB_DIR="/lib/aarch64-linux-gnu"
    if [[ ! -e "$FFMPEG_LIB_DIR/libavcodec.so.61" ]]; then
        avcodec_src=$(find "$FFMPEG_LIB_DIR" -name 'libavcodec.so.60.*' 2>/dev/null | sort -V | tail -1)
        if [[ -n "$avcodec_src" ]]; then
            ln -sf "$avcodec_src" "$FFMPEG_LIB_DIR/libavcodec.so.61"
            info "Created symlink: libavcodec.so.61 → $(basename "$avcodec_src")"
        else
            warn "libavcodec.so.60.* not found — NDI HX streams may not work"
        fi
    fi
    if [[ ! -e "$FFMPEG_LIB_DIR/libavutil.so.59" ]]; then
        avutil_src=$(find "$FFMPEG_LIB_DIR" -name 'libavutil.so.58.*' 2>/dev/null | sort -V | tail -1)
        if [[ -n "$avutil_src" ]]; then
            ln -sf "$avutil_src" "$FFMPEG_LIB_DIR/libavutil.so.59"
            info "Created symlink: libavutil.so.59 → $(basename "$avutil_src")"
        else
            warn "libavutil.so.58.* not found — NDI HX streams may not work"
        fi
    fi
    ldconfig
    ok "NDI HX codec symlinks verified (Noble FFmpeg 6→7 compat)"
fi

# --- 4. Node.js (v20 LTS via NodeSource) ---
if command -v node &>/dev/null && node --version | grep -q "^v2[0-9]"; then
    NODE_VER=$(node --version)
    ok "Node.js already installed: $NODE_VER"
else
    info "Installing Node.js v20 LTS..."
    # Debian trixie ships Node 18, so use NodeSource for v20 on all distros
    curl -fsSL https://deb.nodesource.com/setup_20.x | bash - 2>/dev/null || \
        curl -fsSL https://deb.nodesource.com/setup_20.x | DEBIAN_CODENAME="${CODENAME:-bookworm}" bash -
    apt-get install -yq nodejs
    ok "Node.js installed: $(node --version)"
fi

# --- 5. RPi-specific: DRM/V4L2 group membership ---
if [[ "$BOARD" == "rpi" ]]; then
    # The service user needs access to /dev/dri and /dev/video* devices
    SVCUSER=""
    if id radxa &>/dev/null; then SVCUSER="radxa"; fi
    if [[ -n "$SVCUSER" ]]; then
        for grp in video render audio; do
            if getent group "$grp" &>/dev/null; then
                usermod -aG "$grp" "$SVCUSER" 2>/dev/null && \
                    info "Added $SVCUSER to group $grp" || true
            fi
        done
        ok "RPi device groups configured for $SVCUSER"
    fi
fi

# Grant node access to port 80
NODE_BIN=$(which node 2>/dev/null || true)
if [[ -n "$NODE_BIN" ]]; then
    setcap cap_net_bind_service=+ep "$NODE_BIN" 2>/dev/null && \
        ok "setcap applied to $NODE_BIN" || \
        warn "setcap failed — API may need PORT env override"
fi

# --- 6. Verify ---
echo ""
info "Dependency check:"
ldconfig -p | grep libndi    && echo "  ✓ NDI SDK"        || echo "  ✗ NDI SDK MISSING"
ldconfig -p | grep librockchip_mpp 2>/dev/null && echo "  ✓ MPP" || echo "  - MPP (not found, software decode will be used)"
ldconfig -p | grep librga    2>/dev/null && echo "  ✓ librga"    || echo "  - librga (not found, ok)"
pkg-config --exists libdrm   && echo "  ✓ libdrm"         || echo "  ✗ libdrm MISSING"
pkg-config --exists alsa     && echo "  ✓ alsa"           || echo "  ✗ alsa MISSING"
command -v cmake  &>/dev/null && echo "  ✓ cmake"          || echo "  ✗ cmake MISSING"
command -v node   &>/dev/null && echo "  ✓ node $(node --version)"  || echo "  ✗ node MISSING"
echo ""
ok "setup-deps.sh complete"
