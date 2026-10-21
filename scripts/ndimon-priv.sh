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
    write-config)
        [[ $# -eq 1 ]] || exit 2
        exec /usr/bin/python3 -I /usr/local/lib/ndimon/ndimon-write-config write "$1"
        ;;
    remove-logo)
        [[ $# -eq 1 ]] || exit 2
        exec /usr/bin/python3 -I /usr/local/lib/ndimon/ndimon-write-config remove "$1"
        ;;
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
    start-update)
        if systemctl is-active --quiet ndimon-update.service; then
            echo "update already running" >&2
            exit 3
        fi
        systemctl reset-failed ndimon-update.service >/dev/null 2>&1 || true
        exec systemctl start --no-block ndimon-update.service
        ;;
    stop-core)
        systemctl stop ndimon-r.service ndimon-finder.service
        ;;
    start-core)
        systemctl start ndimon-finder.service ndimon-r.service
        ;;
    net-profiles-status|net-profiles-preview|net-profiles-stage|net-profiles-rollback|net-profiles-boot|net-profiles-sync-tessera)
        [ "$#" -eq 0 ] || { echo "invalid network arguments" >&2; exit 2; }
        exec /usr/local/lib/ndimon/ndimon-net "${cmd#net-}"
        ;;
    net-profiles-confirm)
        [ "$#" -eq 2 ] || { echo "invalid network arguments" >&2; exit 2; }
        exec /usr/local/lib/ndimon/ndimon-net profiles-confirm "$1" "$2"
        ;;
    net-profiles-revert)
        [ "$#" -eq 1 ] || exit 2
        exec /usr/local/lib/ndimon/ndimon-net profiles-revert "$1"
        ;;
    net-vlan-list)
        [ "$#" -eq 0 ] || { echo "invalid VLAN arguments" >&2; exit 2; }
        exec /usr/local/lib/ndimon/ndimon-net vlan-list
        ;;
    net-vlan-apply)
        [ "$#" -eq 2 ] && [ "$1" = "--requester-ip" ] || { echo "invalid VLAN arguments" >&2; exit 2; }
        exec /usr/local/lib/ndimon/ndimon-net vlan-apply --requester-ip "$2"
        ;;
    net-vlan-remove)
        [ "$#" -eq 3 ] && [ "$2" = "--requester-ip" ] || { echo "invalid VLAN arguments" >&2; exit 2; }
        exec /usr/local/lib/ndimon/ndimon-net vlan-remove "$1" --requester-ip "$3"
        ;;
    net-status)
        exec /usr/local/lib/ndimon/ndimon-net status
        ;;
    net-check-address)
        exec /usr/local/lib/ndimon/ndimon-net check-address "${1:-}"
        ;;
    net-apply)
        case "${1:-}" in
            *..*) echo "bad network stage path" >&2; exit 2 ;;
        esac
        if [[ ! "${1:-}" =~ ^/var/lib/ndimon/net/stage-[a-z0-9]+\.json$ ]]; then
            echo "bad network stage path" >&2
            exit 2
        fi
        [ -f "$1" ] || { echo "staged network file missing" >&2; exit 2; }
        exec /usr/local/lib/ndimon/ndimon-net apply "$1"
        ;;
    net-confirm)
        exec /usr/local/lib/ndimon/ndimon-net confirm "${1:-}"
        ;;
    net-rollback)
        exec /usr/local/lib/ndimon/ndimon-net rollback
        ;;
    plugin-inspect|plugin-install|plugin-uninstall|plugin-exec|plugin-job-status|plugin-job-cancel|service-ctl|restart-api)
        # A test prefix is never accepted by a privileged invocation.
        plugin_root=/
        if [[ $EUID -ne 0 && -n "${NDIMON_PRIV_ROOT:-}" ]]; then
            plugin_root=$(realpath -e -- "$NDIMON_PRIV_ROOT")
            [[ "$plugin_root" != / ]] || exit 2
        elif [[ $EUID -ne 0 ]]; then
            echo 'root required' >&2; exit 3
        fi
        if [[ "$plugin_root" == / && ( "$cmd" == plugin-install || "$cmd" == plugin-uninstall || "$cmd" == plugin-exec ) && "${NDIMON_PLUGIN_WORKER:-}" != 1 ]]; then
            # sudo inherits ProtectSystem=strict from the API. A system-manager
            # transient unit provides an independent mount namespace/cgroup.
            [[ "${1:-}" =~ ^[a-z][a-z0-9-]{1,31}$ ]] || exit 2
            if [[ "$cmd" == plugin-exec ]]; then
                [[ "${2:-}" =~ ^[a-z][a-z0-9-]{1,31}$ ]] || exit 2
            fi
            plugin_unit="ndimon-plugin-$1-$$.service"
            # Only install/uninstall participate in the management job protocol.
            # plugin-exec stdout belongs exclusively to its command's response.
            [[ "$cmd" == plugin-exec ]] || printf 'NDIMON_PLUGIN_JOB=%s\n' "$plugin_unit"
            exec systemd-run --quiet --collect --wait --pipe --unit="$plugin_unit" \
                --property=RuntimeMaxSec=600 --property=ProtectSystem=no \
                --setenv=NDIMON_PLUGIN_WORKER=1 -- /usr/local/sbin/ndimon-priv "$cmd" "$@"
        fi
        # The Python program arrives on stdin below. Preserve the caller's
        # payload separately, then restore fd0 before execing a plugin command.
        exec 3<&0
        exec python3 -I - "$plugin_root" "$cmd" "$@" <<'PY'
import fcntl, json, os, pathlib, re, shutil, stat, subprocess, sys, tarfile, tempfile
root = pathlib.Path(sys.argv[1]); cmd = sys.argv[2]; args = sys.argv[3:]
if cmd == 'plugin-exec': os.dup2(3, 0)
os.close(3)
real = root == pathlib.Path('/')
def at(p): return root / p.lstrip('/')
def fail(message, code=2):
    print(message, file=sys.stderr); sys.exit(code)
def valid(s): return bool(re.fullmatch(r'[a-z][a-z0-9-]{1,31}', s))
def systemctl(*argv):
    if real: subprocess.run(['systemctl', *argv], check=True)
def regular(p): return p.is_file() and not p.is_symlink()
def permitted_service(name, unit):
    denied = {'ssh.service','sshd.service','ndimon-r.service','ndimon-api.service','ndimon-finder.service','ndimon-watchdog.service'}
    return unit not in denied and (name == 'tessera' and unit == 'tesserad.service' or name == 'bluetooth' and unit == 'bluealsa.service' or unit == name+'.service' or unit.startswith((name+'-',name+'@','ndimon-'+name+'.','ndimon-'+name+'-')))
def permitted_group(name, group):
    if group in {'tessera','tessera-admin'} and name != 'tessera': return False
    if group in {'root','sudo','wheel','adm','shadow','disk','docker','lxd','kmem','systemd-journal','ndimon'}: return False
    if group in {'audio','video','render','bluetooth','tessera','tessera-admin'}: return True
    result = subprocess.run(['getent','group',group],capture_output=True,text=True)
    if result.returncode == 2: return True
    try: return result.returncode == 0 and int(result.stdout.split(':')[2]) >= 1000
    except (ValueError,IndexError): return False
base = at('/opt/ndimon-r/plugins')
if cmd in ('plugin-job-status','plugin-job-cancel'):
    if len(args) != 1 or not re.fullmatch(r'ndimon-plugin-[a-z][a-z0-9-]{1,31}-[0-9]+\.service', args[0]): fail('invalid plugin job unit')
    if cmd == 'plugin-job-cancel':
        systemctl('stop', '--', args[0]); sys.exit(0)
    if not real:
        print('inactive'); sys.exit(0)
    result = subprocess.run(['systemctl','show','--property=ActiveState','--value','--',args[0]],capture_output=True,text=True)
    if result.returncode: fail('cannot query plugin job',3)
    active = result.stdout.strip()
    if active not in ('inactive','failed','active','activating','deactivating','reloading','refreshing'): fail('unknown job state',3)
    print('inactive' if active in ('inactive','failed') else 'active'); sys.exit(0)
if cmd == 'restart-api':
    if args: fail('unexpected arguments')
    systemctl('--no-block', 'restart', 'ndimon-api.service'); sys.exit(0)
if cmd == 'service-ctl':
    if len(args) != 2 or args[0] not in ('start','stop','restart') or not re.fullmatch(r'[A-Za-z0-9_@.-]+\.service',args[1]): fail('invalid service')
    allowed = False
    for p in base.glob('*/plugin.json'):
        if not valid(p.parent.name) or p.parent.is_symlink() or (p.parent/'.broken').exists(): continue
        try: allowed |= permitted_service(p.parent.name,args[1]) and args[1] in json.loads(p.read_text()).get('services',[])
        except (ValueError,OSError): pass
    if not allowed: fail('service not allowed',3)
    systemctl(args[0], '--', args[1]); sys.exit(0)
if not args or not valid(args[0]): fail('invalid plugin name')
name = args[0]; dest = base / name
state = at('/var/lib/ndimon/plugins') / name
drop = at('/etc/systemd/system/ndimon-api.service.d') / ('plugin-'+name+'.conf')
env = dict(os.environ, NDIMON_PLUGIN_DIR=str(dest), NDIMON_PLUGIN_STATE=str(state))
base.mkdir(parents=True, exist_ok=True)
lock = open(base / '.operation-lock','a')
fcntl.flock(lock, fcntl.LOCK_EX)
previous = base/('.previous-'+name)
if previous.exists() and not dest.exists(): previous.rename(dest)
if cmd == 'plugin-exec':
    if len(args) < 2 or not valid(args[1]): fail('invalid command')
    command = args[1]; target = dest / 'priv' / command
    allow = dest / 'priv/ALLOWED'
    if not regular(allow) or command not in allow.read_text().splitlines(): fail('command not allowed',3)
    if not regular(target) or target.resolve().parent != (dest / 'priv').resolve(): fail('unsafe command',3)
    if real:
        for entry in (allow, target):
            for component in (entry, *entry.parents):
                info = component.lstat()
                if stat.S_ISLNK(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
                    fail('plugin command path must be root-owned and not writable by others',3)
    os.set_inheritable(lock.fileno(), True)
    os.execve(str(target), [str(target), *args[2:]], env)
def script(p, argv=()):
    if not regular(p): fail('missing script: '+str(p))
    return subprocess.run(['/bin/bash',str(p),*argv],env=env,cwd=dest).returncode
if cmd == 'plugin-uninstall':
    if len(args) > 2 or (len(args)==2 and args[1] not in ('purge','force')): fail('invalid uninstall option')
    if not dest.exists():
        drop.unlink(missing_ok=True)
        if previous.is_symlink(): previous.unlink()
        else: shutil.rmtree(previous,ignore_errors=True)
        systemctl('daemon-reload'); sys.exit(0)
    try:
        manifest = json.loads((dest/'plugin.json').read_text())
        if not isinstance(manifest,dict): fail('invalid installed manifest')
    except (OSError,ValueError): fail('installed plugin.json missing or invalid')
    option = args[1:]
    if option != ['force'] and script(dest/manifest.get('uninstall','uninstall.sh'),option):
        (dest/'.broken').write_text('uninstall failed\n'); fail('uninstall failed; files kept',4)
    drop.unlink(missing_ok=True)
    shutil.rmtree(dest)
    if option == ['purge']: shutil.rmtree(state,ignore_errors=True)
    systemctl('daemon-reload'); sys.exit(0)
if len(args) < 2: fail('bundle required')
expected_version = None; force = False; options = args[2:]
while options:
    option = options.pop(0)
    if option == '--expect-version' and options: expected_version = options.pop(0)
    elif option == '--force': force = True
    else: fail('invalid install option')
bundle = pathlib.Path(args[1])
allowed = [at('/var/lib/ndimon/plugins/downloads'), at('/var/lib/ndimon/import')]
if not bundle.is_absolute() or '..' in bundle.parts or not any(p == bundle.parent or p in bundle.parents for p in allowed): fail('bundle outside allowed directories')
def check_state_parent(fd, directory):
    if directory == at('/var/lib/ndimon/plugins'):
        info = os.fstat(fd)
        if info.st_uid != (0 if real else os.getuid()) or info.st_mode & 0o022: raise OSError('unsafe plugins parent')
def open_directory(directory):
    fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        walked = pathlib.Path('/')
        for component in directory.parts[1:]:
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd); fd = child
            walked /= component
            check_state_parent(fd,walked)
        return fd
    except BaseException:
        os.close(fd); raise
try:
    directory_fd = open_directory(bundle.parent)
    try:
        bundle_fd = os.open(bundle.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory_fd)
    finally: os.close(directory_fd)
    info = os.fstat(bundle_fd)
    if real:
        import pwd
        expected_uid = pwd.getpwnam('ndimon').pw_uid
    else: expected_uid = os.getuid()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != expected_uid:
        os.close(bundle_fd); fail('bundle must be a regular file owned by ndimon')
except OSError as e: fail('unsafe bundle path: '+str(e))
# Copy only from the pinned descriptor. No source pathname is reopened between
# validation and copying, even if the uploader replaces the directory entry.
with os.fdopen(bundle_fd, 'rb') as source, tempfile.TemporaryDirectory(prefix='.archive-',dir=base) as temp:
    snapshot = pathlib.Path(temp)/'bundle.tar.gz'
    with open(snapshot, 'xb') as output:
        os.chmod(snapshot, 0o600)
        total = 0
        while True:
            chunk = source.read(min(1024*1024,200*1024*1024-total+1))
            if not chunk: break
            total += len(chunk)
            if total > 200*1024*1024: fail('bundle exceeds 200 MiB')
            output.write(chunk)
    stage = base/('.staging-'+name)
    if stage.exists(): shutil.rmtree(stage)
    try:
        with tarfile.open(snapshot,'r:gz') as archive:
            members = []; expanded = 0
            while True:
                member = archive.next()
                if member is None: break
                expanded += member.size
                if len(members) >= 20000 or expanded > 1024*1024*1024: fail('archive too large')
                members.append(member)
            seen = set()
            for m in members:
                p = pathlib.PurePosixPath(m.name)
                if any(ord(c)<32 or ord(c)==127 for c in m.name) or p.is_absolute() or '..' in p.parts or not (m.isfile() or m.isdir()): fail('unsafe archive entry')
                key = str(p)
                if key in seen: fail('duplicate archive entry')
                seen.add(key)
            entry = next((m for m in members if str(pathlib.PurePosixPath(m.name))=='plugin.json'),None)
            if not entry or not entry.isfile(): fail('plugin.json missing')
            manifest = json.load(archive.extractfile(entry))
            if not isinstance(manifest,dict): fail('manifest must be an object')
            if manifest.get('name') != name: fail('plugin name mismatch')
            known = {'name','version','title','icon','min_ndimon','routes','ui','backup','priv_commands','services','supplementary_groups','readwrite_paths','config_files','install','uninstall'}
            if set(manifest)-known: fail('unknown manifest field')
            for field in ('version','min_ndimon'):
                if not isinstance(manifest.get(field),str) or not re.fullmatch(r'(?:[0-9]+:)?[0-9][A-Za-z0-9.+:~-]*',manifest[field]): fail('invalid version')
            icon = manifest.get('icon','bi-puzzle')
            if not isinstance(icon,str) or not re.fullmatch(r'bi-[a-z0-9-]+',icon): fail('invalid icon')
            if not isinstance(manifest.get('title'),str) or not 1<=len(manifest['title'])<=40: fail('invalid title')
            for field, default in [('routes','routes.js'),('ui','ui.html'),('backup','backup.js'),('install','install.sh'),('uninstall','uninstall.sh')]:
                value = manifest.get(field,default)
                if field=='backup' and value is None: continue
                if not isinstance(value,str) or not re.fullmatch(r'[A-Za-z0-9_-]+\.[A-Za-z0-9]+',value): fail('invalid filename')
            for field, pattern in [('priv_commands',r'[a-z][a-z0-9-]{1,31}'),('supplementary_groups',r'[a-z][a-z0-9-]{1,31}'),('services',r'[A-Za-z0-9_@.-]+\.service'),('readwrite_paths',r'/[A-Za-z0-9_./-]+'),('config_files',r'/[A-Za-z0-9_./-]+')]:
                values = manifest.get(field,[])
                if not isinstance(values,list) or any(not isinstance(v,str) or not re.fullmatch(pattern,v) or '..' in v.split('/') for v in values): fail('invalid '+field)
                if len(values) != len(set(values)): fail('duplicate '+field)
            if any(not permitted_group(name,g) for g in manifest.get('supplementary_groups',[])): fail('privileged group')
            if any(not permitted_service(name,u) for u in manifest.get('services',[])): fail('unscoped service')
            if any(not any(p == base+name or p.startswith(base+name+'/') or name == 'tessera' and p.startswith(base+'tessera') for base in ('/run/','/var/lib/','/etc/')) for p in manifest.get('readwrite_paths',[])): fail('unscoped writable path')
            if expected_version is not None and manifest['version'] != expected_version: fail('bundle version mismatch')
            if cmd == 'plugin-inspect':
                print(json.dumps(manifest)); sys.exit(0)
            try: host_version = at('/etc/ndimon-firmware-version').read_text().strip()
            except OSError: fail('host_too_old: firmware version unavailable')
            if not re.fullmatch(r'(?:[0-9]+:)?[0-9][A-Za-z0-9.+:~-]*',host_version): fail('host_too_old: invalid firmware version')
            if subprocess.run(['dpkg','--compare-versions',host_version,'ge',manifest['min_ndimon']]).returncode != 0: fail('host_too_old')
            if dest.exists() and not force:
                try: installed_version = json.loads((dest/'plugin.json').read_text())['version']
                except (OSError,ValueError,KeyError): fail('invalid installed manifest')
                if subprocess.run(['dpkg','--compare-versions',manifest['version'],'lt',installed_version]).returncode == 0: fail('downgrade requires --force')
            stage.mkdir(mode=0o700)
            for m in members:
                target = stage/pathlib.PurePosixPath(m.name)
                if m.isdir(): target.mkdir(parents=True,exist_ok=True)
                else:
                    target.parent.mkdir(parents=True,exist_ok=True)
                    with archive.extractfile(m) as source, open(target,'wb') as output: shutil.copyfileobj(source,output)
                    target.chmod(0o755 if m.mode & 0o111 else 0o644)
            for directory, _, _ in os.walk(stage): pathlib.Path(directory).chmod(0o755)
            (stage/'priv').mkdir(exist_ok=True)
            (stage/'priv/ALLOWED').write_text(''.join(c+'\n' for c in manifest.get('priv_commands',[])))
            if not regular(stage/manifest.get('install','install.sh')) or not regular(stage/manifest.get('uninstall','uninstall.sh')): fail('install/uninstall scripts required')
    except (OSError,ValueError,tarfile.TarError) as e:
        shutil.rmtree(stage,ignore_errors=True); fail(str(e))
# Walk from an open root directory with O_NOFOLLOW at every component.
# The API owns state directories and could otherwise substitute a symlink
# before root chmod/chown, including in an ancestor directory.
fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
try:
    walked = pathlib.Path('/')
    for component in state.parts[1:]:
        try: os.mkdir(component, mode=0o700, dir_fd=fd)
        except FileExistsError: pass
        child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
        os.close(fd); fd = child
        walked /= component; check_state_parent(fd,walked)
    os.fchmod(fd, 0o700)
    if real:
        import pwd
        user = pwd.getpwnam('ndimon'); os.fchown(fd, user.pw_uid, user.pw_gid)
except OSError:
    shutil.rmtree(stage, ignore_errors=True); fail('unsafe plugin state directory')
finally:
    os.close(fd)
previous = base/('.previous-'+name)
if previous.exists(): shutil.rmtree(previous)
old_drop = drop.read_bytes() if drop.exists() else None
if dest.exists(): dest.rename(previous)
with open(stage/'.broken','w') as marker:
    marker.write('installation in progress\n'); marker.flush(); os.fsync(marker.fileno())
stage.rename(dest)
drop.parent.mkdir(parents=True,exist_ok=True)
install_failed = script(dest/manifest.get('install','install.sh')) != 0
if not install_failed:
    for group in manifest.get('supplementary_groups',[]):
        resolved = subprocess.run(['getent','group',group],capture_output=True,text=True)
        try: exists = resolved.returncode == 0 and int(resolved.stdout.split(':')[2]) >= 0
        except (ValueError,IndexError): exists = False
        if not exists or not permitted_group(name,group):
            print('supplementary group missing or unsafe after install: '+group,file=sys.stderr)
            install_failed = True; break
if install_failed:
    (dest/'.broken').write_text('install failed\n')
    if previous.exists():
        shutil.rmtree(dest); previous.rename(dest); (dest/'.broken').write_text('update failed; previous version restored\n')
    if old_drop is None: drop.unlink(missing_ok=True)
    else:
        tmp = drop.with_suffix('.tmp'); tmp.write_bytes(old_drop); tmp.replace(drop)
    systemctl('daemon-reload'); fail('install failed',4)
content = '[Service]\n'
if manifest.get('supplementary_groups'): content += 'SupplementaryGroups='+' '.join(manifest['supplementary_groups'])+'\n'
if manifest.get('readwrite_paths'): content += 'ReadWritePaths='+' '.join(manifest['readwrite_paths'])+'\n'
tmp = drop.with_suffix('.tmp'); tmp.write_text(content); tmp.replace(drop)
systemctl('daemon-reload')
(dest/'.broken').unlink(missing_ok=True)
shutil.rmtree(previous,ignore_errors=True)
PY
        ;;
    *)
        echo "usage: ndimon-priv reboot|hostname <name>|restart-finder|restart-stack|restart-service <svc>|set-ntp [host]|set-reboot-schedule on|off [HH:MM] [daily|Sun,Mon,...]|start-update|stop-core|start-core|net-status|net-check-address <ip>|net-apply <file>|net-confirm <token>|net-rollback" >&2
        exit 2
        ;;
esac
