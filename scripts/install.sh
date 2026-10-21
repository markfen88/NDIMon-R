#!/bin/bash
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

# Plugin installation is shared by the real install and the side-effect-free
# transcript used by tests. Newline-delimited env preserves repeatable options.
plugin_test_root=''
if [[ "${NDIMON_INSTALL_COPY_TEST:-}" == 1 && $EUID -ne 0 ]]; then
    plugin_test_root=$(realpath -e -- "${NDIMON_INSTALL_TEST_ROOT:?test root required}")
    [[ "$plugin_test_root" != / ]] || exit 2
fi
as_ndimon() {
    if [[ -n "$plugin_test_root" ]]; then "$@"; else runuser -u ndimon -- "$@"; fi
}
install_plugins() {
    local entry name source bundle arch bundle_dir protocols allow_insecure
    local import_dir="$plugin_test_root/var/lib/ndimon/import"
    if [[ -n "${NDIMON_PLUGIN_FEEDS:-}" ]]; then
        if [[ "${NDIMON_INSTALL_DRY_RUN:-}" == 1 ]]; then
            echo 'write plugin_feeds'
        else
            node -e '
              const fs=require("fs");
              let settings={};
              try { settings=JSON.parse(fs.readFileSync("/etc/ndimon-device-settings.json","utf8")); }
              catch(e) { if(e.code!=="ENOENT") throw e; }
              settings.plugin_feeds=process.env.NDIMON_PLUGIN_FEEDS.split("\n").filter(Boolean);
              process.stdout.write(JSON.stringify(settings));
            ' | /usr/local/lib/ndimon/ndimon-write-config write ndimon-device-settings.json
        fi
    fi
    while IFS= read -r entry; do
        [[ -n "$entry" ]] || continue
        name=${entry%%=*}
        [[ "$name" =~ ^[a-z][a-z0-9-]{1,31}$ ]] || { echo 'invalid plugin name' >&2; return 2; }
        bundle="/var/lib/ndimon/import/plugin-$name.XXXXXXXX/$name.tar.gz"
        source=''
        [[ "$entry" != "$name" ]] && source=${entry#*=}
        if [[ "${NDIMON_INSTALL_DRY_RUN:-}" == 1 ]]; then
            if [[ "$entry" == "$name" ]]; then
                echo "resolve $name"
            elif [[ "$source" =~ ^https?:// ]]; then
                echo "download $source $bundle"
            else
                echo "copy $source $bundle"
            fi
            echo "plugin-install $name $bundle"
            continue
        fi
        bundle_dir=$(as_ndimon mktemp -d "$import_dir/plugin-$name.XXXXXXXX")
        bundle="$bundle_dir/$name.tar.gz"
        if [[ "$entry" == "$name" ]]; then
            arch=$(dpkg --print-architecture)
            as_ndimon node /opt/ndimon-r/api/plugin-feed-cli.js "$name" "$arch" "$bundle_dir" > /dev/null
        elif [[ "$source" =~ ^https?:// ]]; then
            allow_insecure=$(as_ndimon node -e '
              const fs=require("fs");
              let settings={};
              try { settings=JSON.parse(fs.readFileSync(process.argv[1],"utf8")); }
              catch(e) { if(e.code!=="ENOENT") throw e; }
              process.stdout.write(settings.allow_insecure_feeds === true ? "true" : "false");
            ' "$plugin_test_root/etc/ndimon-device-settings.json")
            protocols='=https'
            if [[ "$allow_insecure" == true ]]; then protocols='=http,https'; fi
            if [[ "$source" == http://* && "$allow_insecure" != true ]]; then
                echo 'HTTP plugin archives require allow_insecure_feeds:true' >&2
                return 2
            fi
            as_ndimon curl --proto "$protocols" --proto-redir "$protocols" -fsSL "$source" -o "$bundle"
        else
            [[ -f "$source" ]] || { echo 'plugin archive missing' >&2; return 2; }
            as_ndimon cp --no-dereference -- "$source" "$bundle"
        fi
        "$plugin_test_root/usr/local/sbin/ndimon-priv" plugin-install "$name" "$bundle"
        as_ndimon rm -r -- "$bundle_dir"
    done <<< "${NDIMON_WITH_PLUGINS:-}"
}
if [[ "${NDIMON_INSTALL_DRY_RUN:-}" == 1 ]]; then
    install_plugins
    exit
fi

if [[ -n "$plugin_test_root" ]]; then
    install_plugins
    exit
fi

if [ ! -f build/ndimon-r ]; then
    echo "[install] Build first: ./scripts/build.sh"
    exit 1
fi

# --- Root vs non-root ---
if [ "$(id -u)" = "0" ]; then
    # Running as root → install system-wide services
    SYSTEMD_DIR="/etc/systemd/system"
    SYSTEMCTL="systemctl"
    WANTED_BY="multi-user.target"
    SUDO=""
else
    # Non-root → user services in ~/.config/systemd/user
    SYSTEMD_DIR="$HOME/.config/systemd/user"
    SYSTEMCTL="systemctl --user"
    WANTED_BY="default.target"
    SUDO="sudo"
fi

echo "[install] Installing binaries..."
$SUDO cmake --install build

# Dedicated unprivileged user for the API and finder (decoder stays root for DRM).
if [ "$(id -u)" = "0" ]; then
    if ! id ndimon >/dev/null 2>&1; then
        useradd --system --home /var/lib/ndimon --shell /usr/sbin/nologin --comment "NDIMon-R" ndimon
        echo "[install] Created system user ndimon"
    fi
    mkdir -p /var/lib/ndimon/.ndi \
             /var/lib/ndimon/net/pending \
             /var/lib/ndimon/net/vlans \
             /var/lib/ndimon/import \
             /var/lib/ndimon/backups
    # Reassign only API/finder-owned leaves. Recursive ownership of the parent
    # would also expose the private rollback state and VLAN index on reinstall.
    chown root:ndimon /var/lib/ndimon
    chown -R ndimon:ndimon /var/lib/ndimon/.ndi \
             /var/lib/ndimon/import /var/lib/ndimon/backups
    chmod 3770 /var/lib/ndimon
    chmod 2770 /var/lib/ndimon/.ndi
    # The API stages network files here. pending/ holds the rollback snapshot
    # and is root-only so the confirm token is not readable by the API user.
    chown root:ndimon /var/lib/ndimon/net
    chmod 3770 /var/lib/ndimon/net
    chown root:root /var/lib/ndimon/net/pending /var/lib/ndimon/net/vlans
    chmod 00700 /var/lib/ndimon/net/pending /var/lib/ndimon/net/vlans
    chown ndimon:ndimon /var/lib/ndimon/import /var/lib/ndimon/backups
    chmod 0700 /var/lib/ndimon/import /var/lib/ndimon/backups
    install -D -o root -g root -m 0755 "$PROJECT_DIR/scripts/ndimon-write-config.py" /usr/local/lib/ndimon/ndimon-write-config
    install -D -o root -g root -m 0755 "$PROJECT_DIR/scripts/ndimon-cpu-sampling.py" /usr/local/lib/ndimon/ndimon-cpu-sampling
    # Finder state is writable without granting access to /etc.
    for file in ndimon-sources.json NDIFinderSrcList.json ndi_src_find_param; do
        if [ -f "/etc/$file" ] && [ ! -f "/var/lib/ndimon/$file" ]; then
            cp "/etc/$file" "/var/lib/ndimon/$file"
        fi
        [ ! -f "/var/lib/ndimon/$file" ] || chown ndimon:ndimon "/var/lib/ndimon/$file"
        rm -f "/etc/$file"
    done
    # Share one NDI config with the decoder (which is root but HOME is pinned here).
    if [ -f /root/.ndi/ndi-config.v1.json ] && [ ! -f /var/lib/ndimon/.ndi/ndi-config.v1.json ]; then
        cp /root/.ndi/ndi-config.v1.json /var/lib/ndimon/.ndi/
        chown ndimon:ndimon /var/lib/ndimon/.ndi/ndi-config.v1.json
        echo "[install] Migrated NDI config /root/.ndi → /var/lib/ndimon/.ndi"
    fi
    install -d -o root -g root -m 0755 /opt/ndimon-r/plugins
    install -d -o root -g root -m 0755 /var/lib/ndimon/plugins
    install -d -o ndimon -g ndimon -m 0700 /var/lib/ndimon/plugins/downloads
    install -m 0755 "$PROJECT_DIR/scripts/ndimon-priv.sh" /usr/local/sbin/ndimon-priv
    install -m 0440 "$PROJECT_DIR/packaging/sudoers.ndimon" /etc/sudoers.d/ndimon
    visudo -cf /etc/sudoers.d/ndimon >/dev/null || {
        echo "[install] ERROR: sudoers.ndimon failed visudo; not installing it"
        rm -f /etc/sudoers.d/ndimon
    }
fi

# Record firmware / build metadata for the About page.
$SUDO sh -c "echo '1.3.0' > /etc/ndimon-firmware-version" 2>/dev/null || true
$SUDO sh -c "date -u '+%Y-%m-%d' > /etc/ndimon-build-date" 2>/dev/null || true

# Record the source checkout dir + git commit so the web UI's "Check for
# updates" / "Update now" can git-pull and rebuild from the right place.
$SUDO sh -c "printf '%s' '$PROJECT_DIR' > /etc/ndimon-source-dir" 2>/dev/null || true
if command -v git >/dev/null 2>&1 && git -C "$PROJECT_DIR" rev-parse --short HEAD >/dev/null 2>&1; then
    $SUDO sh -c "git -C '$PROJECT_DIR' rev-parse --short HEAD > /etc/ndimon-build-commit" 2>/dev/null || true
fi

echo "[install] Installing config files (only if not present)..."
for f in config/*.json; do
    [ "$(basename "$f")" != ndimon-sources.json ] || continue
    dest="/etc/$(basename $f)"
    if [ ! -f "$dest" ]; then
        $SUDO cp "$f" "$dest"
        echo "  Created $dest"
    else
        echo "  Skipped $dest (already exists)"
    fi
done

# Before the SDK reads it, the saved transport "TCP" was inert (the SDK ran its
# RUDP default). A TCP that was never chosen in the UI (no Rxpm_user_set marker)
# is the old template value: move it to RUDP before any service restarts, so the
# decoder does not start on real single-TCP (measured ~18% frame drops).
if [ -f /etc/ndimon-rx-settings.json ] && command -v python3 >/dev/null 2>&1; then
    $SUDO python3 -I - <<'PYEOF' || true
import json, os
p = '/etc/ndimon-rx-settings.json'
try:
    d = json.load(open(p))
except Exception:
    d = None
if isinstance(d, dict) and d.get('Rxpm') == 'TCP' and d.get('Rxpm_user_set') is not True:
    d['Rxpm'] = 'RUDP'
    tmp = p + '.tmp'
    with open(tmp, 'w') as f:
        json.dump(d, f)
        f.flush()
        os.fsync(f.fileno())
    os.chmod(tmp, os.stat(p).st_mode & 0o7777)
    os.replace(tmp, p)
    print('  Migrated untouched transport default TCP -> RUDP')
PYEOF
fi

# Create param file if missing
[ -f /var/lib/ndimon/ndi_src_find_param ] || $SUDO touch /var/lib/ndimon/ndi_src_find_param

if [ "$(id -u)" = "0" ]; then
    echo "[install] Setting config ownership for ndimon user..."
    chown root:ndimon /etc/ndimon-*.json /etc/ndi-config.json /etc/ndi-group.json \
        /etc/ndimon-auth.json /etc/ndimon-presets.json /etc/ndi-splash-logo.* 2>/dev/null || true
    chmod 640 /etc/ndimon-*.json /etc/ndi-config.json /etc/ndi-group.json /etc/ndi-splash-logo.* 2>/dev/null || true
    chmod 640 /etc/ndimon-auth.json 2>/dev/null || true
    touch /var/lib/ndimon/ndi_src_find_param
    chown ndimon:ndimon /var/lib/ndimon/ndi_src_find_param
fi

# --- RGA device symlink ---
# On newer kernels (6.x) the Rockchip RGA is registered as /dev/video* instead
# of /dev/rga. librga still opens /dev/rga at runtime, so we create a symlink
# and a persistent udev rule so it survives reboots.
if [ "$(id -u)" = "0" ] && [ ! -e /dev/rga ]; then
    RGA_DEV=$(grep -rl "rockchip-rga" /sys/class/video4linux/*/name 2>/dev/null \
              | sed 's|/sys/class/video4linux/\(video[0-9]*\)/name|\1|' | head -1)
    if [ -n "$RGA_DEV" ]; then
        ln -sf "/dev/$RGA_DEV" /dev/rga
        echo "[install] Created /dev/rga -> /dev/$RGA_DEV"
        # Persist via udev so symlink survives reboot
        echo 'KERNEL=="video*", ATTRS{name}=="rockchip-rga", SYMLINK+="rga"' \
            > /etc/udev/rules.d/99-rockchip-rga.rules
        udevadm control --reload-rules 2>/dev/null || true
        echo "[install] Installed udev rule for /dev/rga"
    fi
fi

# --- Install C++ binaries ---
if [ -f "$PROJECT_DIR/build/ndimon-r" ]; then
    echo "[install] Installing ndimon-r binary to /usr/local/bin..."
    $SUDO cp "$PROJECT_DIR/build/ndimon-r" /usr/local/bin/ndimon-r.new
    $SUDO mv /usr/local/bin/ndimon-r.new /usr/local/bin/ndimon-r
fi
if [ -f "$PROJECT_DIR/build/ndimon-finder" ]; then
    echo "[install] Installing ndimon-finder binary to /usr/local/bin..."
    $SUDO cp "$PROJECT_DIR/build/ndimon-finder" /usr/local/bin/ndimon-finder.new
    $SUDO mv /usr/local/bin/ndimon-finder.new /usr/local/bin/ndimon-finder
fi

# --- Install API to stable location ---
# The API is installed to /opt/ndimon-r/api so its path is independent of
# where the source was cloned (avoids WorkingDirectory pointing at a temp dir).
API_INSTALL_DIR="/opt/ndimon-r"
echo "[install] Installing Node.js API to $API_INSTALL_DIR/api..."
$SUDO mkdir -p "$API_INSTALL_DIR"
$SUDO cp -r "$PROJECT_DIR/api" "$API_INSTALL_DIR/"
(cd "$API_INSTALL_DIR/api" && $SUDO npm install --production) || \
    echo "  WARNING: npm install failed (API may not work)"

echo "[install] Granting Node.js permission to bind port 80..."
NODE_BIN=$(which node 2>/dev/null || echo "")
if [ -n "$NODE_BIN" ]; then
    $SUDO setcap cap_net_bind_service=+ep "$NODE_BIN" && \
        echo "  setcap applied to $NODE_BIN" || \
        echo "  WARNING: setcap failed. Run API as root or use PORT=8080 env var."
else
    echo "  WARNING: node not found; cannot apply setcap"
fi

# Root owns the network helper's runtime banner directory, independent of API lifecycle.
$SUDO install -D -m 0644 "$PROJECT_DIR/systemd/ndimon.tmpfiles.conf" /usr/lib/tmpfiles.d/ndimon.conf
$SUDO systemd-tmpfiles --create ndimon.conf

echo "[install] Installing systemd services..."
mkdir -p "$SYSTEMD_DIR"
ACTUAL_NODE=$(which node 2>/dev/null || echo "/usr/bin/node")
for svc in "$PROJECT_DIR/systemd/"*.service; do
    name=$(basename "$svc")
    # Replace placeholders: install dir → /opt/ndimon-r, node binary, WantedBy target
    $SUDO sed -e "s|NDIMON_INSTALL_DIR|$API_INSTALL_DIR|g" \
              -e "s|ExecStart=.*/node server\.js|ExecStart=$ACTUAL_NODE server.js|g" \
              -e "s|WantedBy=default\.target|WantedBy=$WANTED_BY|g" \
              "$svc" > "$SYSTEMD_DIR/$name"
    echo "  Installed $name → $SYSTEMD_DIR/$name"
done

if [ "$(id -u)" != "0" ]; then
    # User units already run as the logged-in user; drop system User=/Group=.
    for name in ndimon-api.service ndimon-finder.service ndimon-r.service; do
        [ -f "$SYSTEMD_DIR/$name" ] || continue
        sed -i '/^User=/d;/^Group=/d;/^AmbientCapabilities=/d;/^StateDirectory=/d;/^Environment=HOME=\/var\/lib\/ndimon/d;/^Environment=NDI_CONFIG_DIR=/d' "$SYSTEMD_DIR/$name"
    done
fi

if [ "$(id -u)" != "0" ]; then
    echo "[install] Enabling systemd linger so services persist after logout..."
    loginctl enable-linger "$(whoami)"
fi

echo "[install] Reloading systemd..."
$SYSTEMCTL daemon-reload

echo "[install] Enabling and starting services..."
# Install watchdog script
echo "[install] Installing watchdog script..."
$SUDO mkdir -p "$API_INSTALL_DIR/scripts"
$SUDO cp "$PROJECT_DIR/scripts/ndimon-watchdog.sh" "$API_INSTALL_DIR/scripts/"
$SUDO chmod +x "$API_INSTALL_DIR/scripts/ndimon-watchdog.sh"
$SUDO cp "$PROJECT_DIR/scripts/ndimon-update-run.sh" "$API_INSTALL_DIR/scripts/"
$SUDO chmod +x "$API_INSTALL_DIR/scripts/ndimon-update-run.sh"
$SUDO mkdir -p /usr/local/lib/ndimon
$SUDO cp "$PROJECT_DIR/scripts/ndimon-net.py" /usr/local/lib/ndimon/ndimon-net
$SUDO chmod 0755 /usr/local/lib/ndimon/ndimon-net
$SUDO install -o root -g root -m 0644 "$PROJECT_DIR/scripts/ndimon_network_profiles.py" /usr/local/lib/ndimon/ndimon_network_profiles.py
$SUDO install -d -o root -g root -m 0700 /var/lib/ndimon-network

$SYSTEMCTL enable ndimon-r ndimon-finder ndimon-api ndimon-watchdog ndimon-net-boot
$SYSTEMCTL restart ndimon-r ndimon-finder ndimon-api ndimon-watchdog

install_plugins
if [[ -n "${NDIMON_WITH_PLUGINS:-}" ]]; then
    $SYSTEMCTL restart ndimon-api
fi

IP=$(hostname -I | awk '{print $1}')
echo ""
echo "[install] Done!"
echo "  Web UI: http://${IP}/"
echo "  API:    http://${IP}/v1/NDIFinder/List"
echo "  Modes:  http://${IP}/v1/VideoOutput/modes"
echo ""
echo "  (If port 80 is unavailable, set PORT=8080 in the ndimon-api service env)"
echo ""
echo "  Logs:"
if [ "$(id -u)" = "0" ]; then
    echo "    journalctl -u ndimon-r -f"
    echo "    journalctl -u ndimon-finder -f"
    echo "    journalctl -u ndimon-api -f"
else
    echo "    journalctl --user -u ndimon-r -f"
    echo "    journalctl --user -u ndimon-finder -f"
    echo "    journalctl --user -u ndimon-api -f"
fi
