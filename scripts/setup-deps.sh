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

# Detect architecture and normalise into: IS_X86, MULTIARCH (Debian triplet),
# NDI_LIB_MATCH (substring used to pick the right dir in the NDI SDK tarball,
# which ships libraries for ALL architectures), and UBUNTU_MIRROR (FFmpeg pin).
ARCH=$(dpkg --print-architecture 2>/dev/null || uname -m)
case "$ARCH" in
    amd64|x86_64)  IS_X86=1; MULTIARCH="x86_64-linux-gnu"; NDI_LIB_MATCH="x86_64-linux-gnu"; FFMPEG_APT_ARCH="amd64"; UBUNTU_MIRROR="http://archive.ubuntu.com/ubuntu" ;;
    arm64|aarch64) IS_X86=0; MULTIARCH="aarch64-linux-gnu"; NDI_LIB_MATCH="aarch64";          FFMPEG_APT_ARCH="arm64"; UBUNTU_MIRROR="http://ports.ubuntu.com/ubuntu-ports" ;;
    armhf|armv7l)  IS_X86=0; MULTIARCH="arm-linux-gnueabihf"; NDI_LIB_MATCH="arm-";            FFMPEG_APT_ARCH="armhf"; UBUNTU_MIRROR="http://ports.ubuntu.com/ubuntu-ports" ;;
    *)             IS_X86=0; MULTIARCH="$(gcc -dumpmachine 2>/dev/null || echo unknown)"; NDI_LIB_MATCH="$ARCH"; FFMPEG_APT_ARCH="$ARCH"; UBUNTU_MIRROR="http://ports.ubuntu.com/ubuntu-ports" ;;
esac
info "Arch:  $ARCH  (x86=$IS_X86, multiarch=$MULTIARCH)"

export DEBIAN_FRONTEND=noninteractive

# --- 1. Basic build tools ---
info "Installing build tools..."
apt-get install -yq --no-install-recommends \
    build-essential cmake pkg-config git wget curl ca-certificates \
    libdrm-dev libasound2-dev \
    libavahi-client-dev libavahi-common-dev \
    nlohmann-json3-dev \
    libsystemd-dev \
    libavcodec-dev libavformat-dev libswscale-dev libavutil-dev \
    2>/dev/null || {
    # Fallback: some packages may differ by distro
    apt-get install -yq --no-install-recommends \
        build-essential cmake pkg-config git wget curl ca-certificates \
        libdrm-dev libasound2-dev \
        libavahi-client-dev libavahi-common-dev \
        libsystemd-dev \
        libavcodec-dev libavutil-dev libswscale-dev
    # nlohmann-json header-only, can be fetched by cmake if package not found
    apt-get install -yq --no-install-recommends nlohmann-json3-dev 2>/dev/null || true
}
ok "Build tools installed"

# --- 2. Rockchip MPP (only on Rockchip boards) ---
if [[ "$BOARD" == rk3588 || "$BOARD" == rk3399 ]]; then
    if /sbin/ldconfig -p 2>/dev/null | grep -q librockchip_mpp; then
        ok "Rockchip MPP already installed"
    else
        info "Installing Rockchip MPP..."

        # Install the Radxa archive keyring .deb
        # The keyring package installs yearly-rotated keys to /usr/share/keyrings/
        _keyring_url=$(curl -sL https://api.github.com/repos/radxa-pkg/radxa-archive-keyring/releases/latest 2>/dev/null \
            | python3 -c "import sys,json; assets=json.load(sys.stdin).get('assets',[]); \
              print(next((a['browser_download_url'] for a in assets if a['name'].endswith('_all.deb')), ''))" 2>/dev/null)
        RADXA_KEYRING_DEB="${_keyring_url:-https://github.com/radxa-pkg/radxa-archive-keyring/releases/download/0.2.2/radxa-archive-keyring_0.2.2_all.deb}"
        if ! dpkg -s radxa-archive-keyring &>/dev/null; then
            info "Fetching Radxa archive keyring..."
            _mpp_tmpdir=$(mktemp -d)
            if wget -q -O "$_mpp_tmpdir/radxa-keyring.deb" "$RADXA_KEYRING_DEB" 2>/dev/null; then
                dpkg -i "$_mpp_tmpdir/radxa-keyring.deb" 2>/dev/null && ok "Radxa keyring installed" || \
                    warn "Radxa keyring install failed"
            else
                warn "Could not download Radxa keyring"
            fi
            rm -rf "$_mpp_tmpdir"
        fi

        # Find a valid installed keyring file (yearly rotation: 2024, 2025, 2026 …)
        RADXA_KEY=$(ls /usr/share/keyrings/radxa-archive-keyring-*.gpg 2>/dev/null | sort -V | tail -1)

        if [[ -n "$RADXA_KEY" ]]; then
            # MPP packages live in rk3588-bookworm regardless of host OS.
            # Also add noble + noble-test repos (Radxa utilities, dependency resolution).
            SUITE_SOC="${BOARD}-bookworm"
            RADXA_LIST=/etc/apt/sources.list.d/radxa-rockchip.list
            write_radxa_sources() {
                local key="$1"
                {
                    echo "deb [signed-by=$key] https://radxa-repo.github.io/bookworm bookworm main"
                    echo "deb [signed-by=$key] https://radxa-repo.github.io/${SUITE_SOC} ${SUITE_SOC} main"
                    if [[ "$CODENAME" == "noble" ]]; then
                        echo "deb [signed-by=$key] https://radxa-repo.github.io/noble noble main"
                        echo "deb [signed-by=$key] https://radxa-repo.github.io/noble-test noble-test main"
                    fi
                } > "$RADXA_LIST"
            }
            write_radxa_sources "$RADXA_KEY"

            # Full update so apt can resolve dependencies across all sources
            info "Updating apt package index (Radxa repos)..."
            UPDATE_ERRORS=$(apt-get update -qq 2>&1 | grep -E 'Err:|W:.*GPG|E:.*not signed' || true)
            if [[ -n "$UPDATE_ERRORS" ]]; then
                # The latest keyring may not yet sign the repos (e.g. 2026 key, repos still
                # signed by 2025 key). Fall back to the known-good 2025 keyring.
                RADXA_KEY_2025="/usr/share/keyrings/radxa-archive-keyring-2025.gpg"
                if [[ ! -f "$RADXA_KEY_2025" ]]; then
                    info "Fetching Radxa 2025 keyring (fallback)..."
                    _mpp_tmpdir2=$(mktemp -d)
                    if wget -q -O "$_mpp_tmpdir2/radxa-keyring-2025.deb" \
                        "https://github.com/radxa-pkg/radxa-archive-keyring/releases/download/0.2.2/radxa-archive-keyring_0.2.2_all.deb" 2>/dev/null; then
                        dpkg -i "$_mpp_tmpdir2/radxa-keyring-2025.deb" 2>/dev/null || true
                    fi
                    rm -rf "$_mpp_tmpdir2"
                fi
                if [[ -f "$RADXA_KEY_2025" ]]; then
                    warn "Latest keyring has GPG errors — falling back to 2025 keyring"
                    RADXA_KEY="$RADXA_KEY_2025"
                    write_radxa_sources "$RADXA_KEY"
                    apt-get update -qq 2>&1 | grep -E 'Err:|W:' || true
                else
                    echo "$UPDATE_ERRORS"
                fi
            fi

            apt-get install -yq --no-install-recommends \
                librockchip-mpp1 librockchip-mpp-dev librockchip-vpu0 && ok "MPP installed" || \
            warn "MPP packages unavailable — software decode only"
        else
            warn "Radxa keyring not found — MPP installation skipped (software decode will be used)"
        fi
    fi
fi

# --- 2b. VAAPI hardware decode (x86 Intel/AMD) ---
# On Linux the NDI SDK has NO GPU decode path (FFmpeg software only), so HX
# H.264/H.265 hardware decode is done application-side via the VAAPIDecoder.
# VAAPI is the vendor-neutral API covering Intel (iHD/i965) and AMD (Mesa).
if [[ "$IS_X86" == "1" ]]; then
    info "Installing VAAPI runtime + drivers (Intel/AMD hardware decode)..."
    # libva runtime + vainfo; intel-media (modern iHD), i965 (older Intel),
    # mesa-va-drivers (AMD). Installing all three is harmless — libva loads the
    # one matching the GPU. dev headers are pulled for the build.
    apt-get install -yq --no-install-recommends \
        libva2 libva-drm2 libva-dev vainfo \
        intel-media-va-driver-non-free i965-va-driver mesa-va-drivers \
        2>/dev/null || \
    apt-get install -yq --no-install-recommends \
        libva2 libva-drm2 libva-dev vainfo mesa-va-drivers 2>/dev/null || \
        warn "Some VAAPI packages unavailable — hardware decode may fall back to software"
    ok "VAAPI runtime installed"
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

    # Install library — the NDI SDK tarball ships libraries for ALL targets
    # (x86_64-linux-gnu, aarch64-*, arm-*), so we MUST pick the dir matching this
    # host's architecture (NDI_LIB_MATCH) rather than always grabbing aarch64.
    local ndi_lib_dir=""
    local ndi_lib_base="$ndi_base/lib"
    if [[ -d "$ndi_lib_base" ]]; then
        ndi_lib_dir=$(find "$ndi_lib_base" -type d -name "*${NDI_LIB_MATCH}*" 2>/dev/null | head -1)
        # aarch64 fallbacks (Pi tarball names the dir aarch64-rpi4-linux-gnueabi)
        if [[ -z "$ndi_lib_dir" && "$IS_X86" != "1" ]]; then
            ndi_lib_dir=$(find "$ndi_lib_base" -type d -name "*aarch64*" 2>/dev/null | head -1)
            [[ -z "$ndi_lib_dir" ]] && ndi_lib_dir=$(find "$ndi_lib_base" -type d -name "*arm64*" 2>/dev/null | head -1)
        fi
    fi
    # Final fallback: locate the .so under a dir matching this arch only — do NOT
    # blindly grab the first libndi.so.* in the tree (that could be the wrong arch).
    if [[ -z "$ndi_lib_dir" ]]; then
        local so_parent; so_parent=$(find "$ndi_base" -path "*${NDI_LIB_MATCH}*" -name "libndi.so.*.*" 2>/dev/null | head -1)
        [[ -n "$so_parent" ]] && ndi_lib_dir=$(dirname "$so_parent")
    fi

    if [[ -z "$ndi_lib_dir" ]]; then
        warn "NDI library for $ARCH ($NDI_LIB_MATCH) not found in extracted SDK"
        return 1
    fi
    info "NDI library dir: $ndi_lib_dir"

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
    # Record the exact installed SDK version for the About page and for support
    # diagnostics. We pin to the v6 SDK line (see NDI_SDK_URL); HX passthrough
    # behaviour is validated against this major version at runtime.
    echo "${so_ver#libndi.so.}" > /etc/ndimon-ndi-version 2>/dev/null || true
    ok "NDI SDK installed: $so_ver"
}

if [[ -f /usr/local/lib/libndi.so ]]; then
    ok "NDI SDK already present"
else
    install_ndi_sdk || warn "NDI SDK install failed — build will fail without it"
fi

# --- 3b. FFmpeg 7 (libavcodec.so.61 + libavutil.so.59) for NDI HX decode ---
# NDI SDK 6.x dlopen()s libavcodec.so.61 + libavutil.so.59 at runtime to decode
# H.265 HX sources internally. Without these libraries the SDK renders a
# "Video Decoder not Found" error frame for any H.265 source.
#
# These packages coexist with the distro FFmpeg (different sonames) — no conflicts.
#
# Previously a compat symlink (.so.61 → .so.60) was suggested; DO NOT do this —
# the FFmpeg 6 and 7 ABIs differ in AVFrame/AVPacket layout and cause a hard SEGV.
# Remove only stale compat shims (those pointing to FFmpeg 6 targets, e.g. .so.60.*).
# Do NOT remove real Debian/Ubuntu package symlinks pointing to .so.61.* targets.
for _stale in /lib/${MULTIARCH}/libavcodec.so.61 /lib/${MULTIARCH}/libavutil.so.59 \
              /usr/lib/${MULTIARCH}/libavcodec.so.61 /usr/lib/${MULTIARCH}/libavutil.so.59; do
    if [[ -L "$_stale" ]]; then
        _target=$(readlink "$_stale")
        # Only remove if pointing to an FFmpeg 6 library (compat shim), not FFmpeg 7
        if [[ "$_target" != *".so.61."* && "$_target" != *".so.59."* ]]; then
            rm -f "$_stale"
            info "Removed stale FFmpeg compat symlink: $_stale -> $_target"
        fi
    fi
done

install_ffmpeg7() {
    # Use Ubuntu Plucky (25.04) arm64 packages — they install cleanly on
    # both Ubuntu Noble and Debian Bookworm (same libc/libstdc++ ABI).
    #
    # RECONSIDER: once Ubuntu 26.04 LTS ships FFmpeg 7 in the default
    # Noble successor pocket (or backports lands libavcodec61 in Noble
    # itself), drop the Plucky pin and use the distro repo directly.
    # Pinning to a non-LTS release ties our HX H.265 path to Plucky's
    # 9-month lifecycle.
    local _tmplist; _tmplist=$(mktemp /etc/apt/sources.list.d/_tmp_plucky_XXXXXX.list)
    echo "deb [arch=${FFMPEG_APT_ARCH} trusted=yes] ${UBUNTU_MIRROR} plucky main universe" \
        > "$_tmplist"
    apt-get update -o Dir::Etc::sourcelist="$_tmplist" \
        -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0 -qq 2>/dev/null || true
    apt-get install -yq --no-install-recommends libavcodec61 libavutil59 2>/dev/null
    local _rc=$?
    rm -f "$_tmplist"
    # Restore normal apt lists cache so subsequent apt-get calls work correctly
    apt-get update -qq 2>/dev/null || true
    return $_rc
}

if /sbin/ldconfig -p 2>/dev/null | grep -q 'libavcodec\.so\.61\b'; then
    ok "FFmpeg 7 already present (libavcodec.so.61)"
else
    info "Installing FFmpeg 7 (libavcodec61 + libavutil59) for NDI HX decode..."
    if install_ffmpeg7; then
        /sbin/ldconfig
        ok "FFmpeg 7 installed — NDI SDK can now decode H.265 HX sources"
    else
        warn "FFmpeg 7 install failed — NDI HX (H.265) sources will show 'Video Decoder not Found'"
        warn "Manual fix: add Ubuntu Plucky to apt sources and run:"
        warn "  apt-get install libavcodec61 libavutil59"
    fi
fi

# --- 4. Node.js (v20 LTS via NodeSource) + npm ---
# Check that node is v20+ AND comes from NodeSource (not the Ubuntu/Debian system package).
# Ubuntu 24.04 ships v18 from apt; NodeSource v20 must replace it.
_node_ok=0
if command -v node &>/dev/null && node --version 2>/dev/null | grep -q "^v2[0-9]"; then
    # Verify it's from NodeSource, not the distro package
    if dpkg -l nodejs 2>/dev/null | grep -q nodesource; then
        _node_ok=1
    fi
fi
if [[ $_node_ok -eq 1 ]] && command -v npm &>/dev/null; then
    ok "Node.js already installed: $(node --version) / npm $(npm --version)"
else
    info "Installing Node.js v20 LTS via NodeSource..."
    # Remove distro-packaged nodejs first to avoid conflicts
    apt-get remove -yq nodejs npm 2>/dev/null || true
    curl -fsSL https://deb.nodesource.com/setup_20.x | bash - 2>/dev/null || \
        curl -fsSL https://deb.nodesource.com/setup_20.x | DEBIAN_CODENAME="${CODENAME:-bookworm}" bash -
    apt-get install -yq nodejs
    ok "Node.js installed: $(node --version) / npm $(npm --version 2>/dev/null || echo '?')"
fi

# --- 5. DRM/VAAPI/V4L2 device group membership ---
# The service user needs access to /dev/dri (DRM display + VAAPI renderD128)
# and /dev/video* (V4L2). Required on every platform: ARM for MPP/V4L2, x86 for
# VAAPI. Pick the most likely service user: the sudo invoker, else a known SBC
# account, else skip (root installs don't need group membership).
SVCUSER="${SUDO_USER:-}"
if [[ -z "$SVCUSER" || "$SVCUSER" == "root" ]]; then
    for _u in radxa rock pi ubuntu; do
        if id "$_u" &>/dev/null; then SVCUSER="$_u"; break; fi
    done
fi
if [[ -n "$SVCUSER" && "$SVCUSER" != "root" ]]; then
    for grp in video render audio; do
        if getent group "$grp" &>/dev/null; then
            usermod -aG "$grp" "$SVCUSER" 2>/dev/null && \
                info "Added $SVCUSER to group $grp" || true
        fi
    done
    ok "Device groups (video/render/audio) configured for $SVCUSER"
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
/sbin/ldconfig -p | grep libndi          && echo "  ✓ NDI SDK"           || echo "  ✗ NDI SDK MISSING"
/sbin/ldconfig -p | grep 'libavcodec\.so\.61\b' && echo "  ✓ FFmpeg 7 (libavcodec61)" \
                                          || echo "  ✗ FFmpeg 7 (libavcodec61) MISSING — H.265 HX sources will fail"
/sbin/ldconfig -p | grep librockchip_mpp 2>/dev/null && echo "  ✓ MPP"   || echo "  - MPP (not found, software decode will be used)"
if [[ "$IS_X86" == "1" ]]; then
    # Probe VAAPI and record supported decode profiles for the About page.
    if command -v vainfo &>/dev/null && vainfo &>/tmp/_vainfo 2>&1; then
        cp -f /tmp/_vainfo /etc/ndimon-vaapi-info 2>/dev/null || true
        if grep -q "VAProfile.*\(H264\|HEVC\).*VAEntrypointVLD" /tmp/_vainfo; then
            echo "  ✓ VAAPI hardware decode ($(grep -oE 'Driver version[^\n]*' /tmp/_vainfo | head -1))"
        else
            echo "  - VAAPI present but no H.264/HEVC decode profile — software decode will be used"
        fi
        rm -f /tmp/_vainfo
    else
        echo "  - VAAPI not usable (no /dev/dri/renderD128 or driver) — software decode will be used"
    fi
fi
pkg-config --exists libdrm   && echo "  ✓ libdrm"          || echo "  ✗ libdrm MISSING"
pkg-config --exists libsystemd && echo "  ✓ libsystemd"    || echo "  ✗ libsystemd MISSING (sd_notify won't work)"
pkg-config --exists alsa     && echo "  ✓ alsa"            || echo "  ✗ alsa MISSING"
command -v cmake  &>/dev/null && echo "  ✓ cmake"           || echo "  ✗ cmake MISSING"
command -v node   &>/dev/null && echo "  ✓ node $(node --version)" || echo "  ✗ node MISSING"
echo ""
ok "setup-deps.sh complete"
