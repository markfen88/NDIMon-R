"""Versioned, device-side network transactions. Only ndimon-net invokes this module.

All persistent state is below a root-owned directory outside API-writable trees.
OS adapters are kept here so a transaction snapshots exactly the files it writes.
"""
import copy
import fcntl
import ipaddress
import json
import os
import re
import secrets
import stat
import subprocess
import time
from contextlib import contextmanager

ROOT = '/var/lib/ndimon-network'
RUNTIME = '/etc/ndimon-network-interfaces.json'
TESSERA = '/etc/tessera/config.d/62-ndimon-network.toml'
CONFIRM_SECONDS = 60
APPLY_SECONDS = 90
MAX_BYTES = 65536
SLICES = {'ndi':'ndimonndi.slice','tessera':'ndimontessera.slice'}
MEDIA_UNITS = ('ndimon-watchdog.service','ndimon-finder.service','ndimon-r.service','tesserad.service')


class Profiles:
    def __init__(self, net):
        self.n = net

    def error(self, message):
        raise self.n.NetError(message)

    @contextmanager
    def locked(self):
        # /var/lib is root-owned; never traverse the API-writable /var/lib/ndimon.
        try:
            os.mkdir(ROOT, 0o700)
        except FileExistsError:
            pass
        fd = os.open(ROOT, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        info = os.fstat(fd)
        if info.st_uid != 0 or stat.S_IMODE(info.st_mode) != 0o700:
            os.close(fd)
            self.error('unsafe network transaction directory')
        lock = os.open('lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600, dir_fd=fd)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX)
            yield
        finally:
            os.close(lock)
            os.close(fd)

    def path(self, name):
        return ROOT + '/' + name + '.json'

    def read(self, name):
        path = self.path(name)
        try:
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        except FileNotFoundError:
            return None
        with os.fdopen(fd) as stream:
            info = os.fstat(stream.fileno())
            if info.st_uid != 0 or not stat.S_ISREG(info.st_mode) or info.st_size > 2 * 1024 * 1024:
                self.error('unsafe network state file')
            return json.load(stream)

    def write(self, name, doc):
        self.n.atomic_write(self.path(name), json.dumps(doc), 0o600)

    def remove(self, name):
        try:
            os.unlink(self.path(name))
        except FileNotFoundError:
            pass

    def current(self):
        saved = self.read('committed')
        if saved:
            return saved
        status = self.n.status_document()
        if not status.get('applicable') or status.get('vlan_error'):
            self.error(status.get('vlan_error') or status.get('reason') or 'network configuration unavailable')
        base = self.import_base(status)
        if base['mode'] not in ('static', 'dhcp'):
            self.error('cannot import unknown base network configuration')
        networks = [dict(id='untagged', name='Untagged', vlan_id=None, **self.n.validate_request(base))]
        for row in status.get('vlans', []):
            if not row.get('managed'):
                continue
            settings = self.n.validate_request({**row, 'gateway': '', 'dns': []})
            networks.append(dict(id='vlan-' + str(row['id']), name='VLAN ' + str(row['id']), vlan_id=row['id'], **settings))
        return dict(version=1, revision=0, interface=status['interface'], backend=status['backend'],
                    networks=networks, management='untagged', ndi='untagged', tessera='untagged')

    def import_base(self, status):
        """Read effective configuration, including files not created by NDIMon."""
        base = {key: status.get(key) for key in ('mode','address','prefix','gateway','dns')}
        backend, iface = status['backend'], status['interface']
        if backend == 'netplan':
            import yaml
            parent = self.n.vlan_netplan_parent(iface)
            # Ask Netplan for its merged Ethernet map; do not read Wi-Fi secrets.
            entries = yaml.safe_load(self.n.run(['netplan','get','network.ethernets']).stdout) or {}
            entry = entries.get(parent)
            if not isinstance(entry, dict):
                self.error('cannot import the effective Netplan Ethernet configuration')
            addresses = entry.get('addresses') or []
            dhcp = entry.get('dhcp4') is True
            routes = entry.get('routes') or []
            if any(route.get('to') not in ('default','0.0.0.0/0') for route in routes):
                self.error('custom static routes cannot be represented by network profiles')
            gateways = [str(route['via']) for route in routes if route.get('via')]
            if entry.get('gateway4'): gateways.append(str(entry['gateway4']))
            dns = (entry.get('nameservers') or {}).get('addresses') or []
        elif backend == 'networkd':
            result = self.n.run(['networkctl','status','--no-pager',iface])
            match = re.search(r'Network File:\s+(\S+\.network)',result.stdout)
            if not match:
                self.error('cannot import the effective networkd configuration')
            path = match.group(1)
            texts = [self.n.read_text(path), *[self.n.read_text(p) for p in self.n.vlan_networkd_dropins(path)]]
            addresses, gateways, dns, dhcp, section, route = [], [], [], False, '', {}
            def finish_route():
                if route.get('Gateway'):
                    if route.get('Destination','0.0.0.0/0') not in ('0.0.0.0/0','default'):
                        self.error('custom static routes cannot be represented by network profiles')
                    gateways.append(route['Gateway'])
            for text in texts:
                for raw in text.splitlines():
                    line = raw.strip()
                    if not line or line.startswith(('#',';')): continue
                    if line.startswith('['):
                        finish_route(); route={}; section=line
                        continue
                    key, separator, value = line.partition('=')
                    if not separator: continue
                    key, value = key.strip(), value.strip()
                    if section == '[Route]': route[key]=value
                    elif section == '[Network]' or section == '[Address]' and key == 'Address':
                        if key == 'DHCP': dhcp=value.lower() in ('yes','true','ipv4')
                        elif key in ('Address','Gateway','DNS'):
                            values={'Address':addresses,'Gateway':gateways,'DNS':dns}[key]
                            if not value: values.clear()
                            else: values.extend(value.split())
                finish_route(); route={}
        else:
            return base
        if dhcp and (addresses or gateways or dns):
            self.error('mixed DHCP and static settings cannot be represented by network profiles')
        if dhcp:
            return dict(mode='dhcp',address='',prefix=0,gateway='',dns=[])
        if len(addresses) != 1 or len(set(gateways)) > 1 or not isinstance(addresses[0],str):
            self.error('network profiles require one static IPv4 address and one default gateway')
        address, prefix = self.n.split_cidr(addresses[0])
        return dict(mode='static',address=address,prefix=prefix,gateway=gateways[0] if gateways else '',dns=dns)

    def validate(self, doc, previous):
        if not isinstance(doc, dict) or set(doc) - {'version', 'revision', 'interface', 'backend', 'networks', 'management', 'ndi', 'tessera'}:
            self.error('invalid network document')
        if type(doc.get('version')) is not int or doc['version'] != 1 or type(doc.get('revision')) is not int:
            self.error('invalid network version or revision')
        if doc.get('interface') != previous['interface'] or doc.get('backend') != previous['backend']:
            self.error('physical port and network backend cannot be changed here')
        iface = doc['interface']
        if not re.fullmatch(r'[A-Za-z0-9_-]{1,15}', iface):
            self.error('invalid physical interface')
        rows = doc.get('networks')
        if not isinstance(rows, list) or not 1 <= len(rows) <= 9:
            self.error('configure between 1 and 9 networks')
        ids, tags, subnets, clean = set(), set(), [], []
        for role in ('management', 'ndi', 'tessera'):
            if doc.get(role) not in [r.get('id') for r in rows if isinstance(r, dict)]:
                self.error(f'{role} is assigned to a removed network; select a replacement first')
        for row in rows:
            if not isinstance(row, dict) or set(row) - {'id', 'name', 'vlan_id', 'mode', 'address', 'prefix', 'gateway', 'dns'}:
                self.error('invalid network profile')
            key, name, tag = row.get('id'), row.get('name'), row.get('vlan_id')
            if not isinstance(key, str) or not re.fullmatch(r'[a-z][a-z0-9-]{0,31}', key) or key in ids:
                self.error('network identifiers must be unique')
            if not isinstance(name, str) or not name.strip() or len(name) > 64 or any(ord(c) < 32 for c in name):
                self.error('invalid network name')
            if tag is not None and (type(tag) is not int or not 1 <= tag <= 4094):
                self.error('VLAN ID must be 1..4094 or untagged')
            if tag in tags or len(self.interface(doc, row)) > 15:
                self.error('duplicate VLAN or interface name too long')
            if row.get('mode') == 'static' and type(row.get('prefix')) is not int:
                self.error('prefix must be an integer')
            settings = self.n.validate_request(row)
            if key != doc.get('management') and (settings['gateway'] or settings['dns']):
                self.error('gateway and DNS belong to the management network only')
            if settings['mode'] == 'static':
                subnet = ipaddress.ip_network(f"{settings['address']}/{settings['prefix']}", strict=False)
                if any(subnet.overlaps(other) for other in subnets):
                    self.error('network subnets overlap')
                subnets.append(subnet)
            ids.add(key)
            tags.add(tag)
            clean.append(dict(id=key, name=name.strip(), vlan_id=tag, **settings))
        if len(tags - {None}) > 8:
            self.error('configure at most 8 tagged VLANs')
        for role in ('management', 'ndi', 'tessera'):
            if doc.get(role) not in ids:
                self.error(f'{role} is assigned to a removed network; select a replacement first')
        return dict(version=1, revision=doc['revision'], interface=iface, backend=doc['backend'],
                    networks=clean, **{r: doc[r] for r in ('management', 'ndi', 'tessera')})

    def interface(self, doc, row):
        return doc['interface'] + ('.' + str(row['vlan_id']) if row['vlan_id'] is not None else '')

    def role(self, doc, role):
        return next(row for row in doc['networks'] if row['id'] == doc[role])

    def status(self):
        doc = self.current()
        rows = []
        for row in doc['networks']:
            iface = self.interface(doc, row)
            address, prefix = self.n.live_address(iface)
            rows.append(dict(**row, interface=iface, live_address=address, live_prefix=prefix,
                             ready=bool(address), used_by=[r for r in ('management', 'ndi', 'tessera') if doc[r] == row['id']]))
        pending = self.read('pending')
        if pending and self.is_committed(pending):
            self.prune_nm(pending)
            self.remove('pending')
            self.n.clear_banner()
            pending = None
        return dict(ok=True, settings=doc, networks=rows, pending=self.public_pending(pending))

    def public_pending(self, state):
        if not state:
            return None
        return dict(token=state['token'], phase=state['phase'], address=state.get('address', ''),
                    profiles=True,
                    deadline=int(time.time() + max(0, state['expires'] - time.monotonic())),
                    remaining_seconds=max(0, int(state['expires'] - time.monotonic())),
                    confirm_seconds=CONFIRM_SECONDS)

    def preview(self, requested):
        old = self.current()
        doc = self.validate(requested, old)
        if doc['revision'] != old['revision']:
            self.error('revision conflict; reload network settings')
        old_rows = {r['id']: r for r in old['networks']}
        new_rows = {r['id']: r for r in doc['networks']}
        affected = [r for r in ('management', 'ndi', 'tessera')
                    if old[r] != doc[r] or old_rows[old[r]] != new_rows[doc[r]]]
        removed = [r['name'] for r in old['networks'] if r['id'] not in new_rows]
        return dict(ok=True, settings=doc, unchanged=doc == old, affected=affected, removed=removed,
                    requires_confirmation=True, confirm_seconds=CONFIRM_SECONDS,
                    message='Network changes may interrupt services. Confirm through the new management connection within 60 seconds after apply, or all network changes revert.')

    def stage(self, requested):
        # Start the boot prerequisite before taking our lock. Once active it
        # stays active; service restarts must not replay boot recovery against a
        # transaction that is waiting for the user's confirmation.
        if not self.n.systemctl_active('ndimon-net-boot.service'):
            self.n.run(['systemctl','start','ndimon-net-boot.service'],timeout=90)
        with self.locked():
            if self.read('pending') or os.path.exists(self.n.STATE):
                self.error('a network change is already pending')
            plan = self.preview(requested)
            if plan['unchanged']:
                return dict(ok=True, unchanged=True)
            old = self.current()
            owned = {self.interface(old, r) for r in old['networks']}
            for row in plan['settings']['networks']:
                iface = self.interface(plan['settings'], row)
                if row['vlan_id'] is not None and iface not in owned:
                    present = self.n.run(['ip', '-j', 'link', 'show', 'dev', iface], check=False)
                    if present.returncode == 0 and json.loads(present.stdout or '[]'):
                        self.error('interface already exists outside managed networks: ' + iface)
            # Preflight before scheduling a change; nft is needed for management isolation.
            self.n.run(['nft', 'list', 'tables'])
            previous_filter = self.n.run(['nft','list','table','inet','ndimon_management'],check=False)
            state = dict(version=1, token=secrets.token_hex(32), phase='scheduled',
                         expires=time.monotonic() + APPLY_SECONDS + CONFIRM_SECONDS,
                         boot=self.n.read_text('/proc/sys/kernel/random/boot_id').strip(),
                         previous=self.current(), requested=plan['settings'], files={}, nm={}, address='',
                         running=[unit for unit in MEDIA_UNITS if self.n.systemctl_active(unit)],
                         nft_present=previous_filter.returncode==0,
                         nft_previous=previous_filter.stdout if previous_filter.returncode==0 else '')
            self.write('pending', state)
            try:
                self.n.schedule('ndimon-profiles-rollback', APPLY_SECONDS + CONFIRM_SECONDS,
                                [self.n.HELPER, 'profiles-rollback', state['token']])
                self.n.schedule('ndimon-profiles-apply', 2, [self.n.HELPER, 'profiles-apply', state['token']])
            except Exception:
                self.remove('pending')
                raise
            return dict(ok=True, **self.public_pending(state))

    def snapshot_write(self, state, path, text, mode=0o644):
        if path not in state['files']:
            previous = self.n.vlan_file_snapshot(path)
            state['files'][path] = list(previous) if previous else None
            self.write('pending', state)  # write-ahead journal, including partial applies
        if text is None:
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass
        else:
            self.n.atomic_write(path, text, mode)

    def networkd(self, state):
        doc = state['requested']
        base = doc['interface']
        tagged = [r for r in doc['networks'] if r['vlan_id'] is not None]
        untagged = next((r for r in doc['networks'] if r['vlan_id'] is None), None)
        for row in doc['networks']:
            iface = self.interface(doc, row)
            if row['vlan_id'] is not None:
                self.snapshot_write(state, f'/etc/systemd/network/00-ndimon-{iface}.netdev',
                                    f"[NetDev]\nName={iface}\nKind=vlan\n\n[VLAN]\nId={row['vlan_id']}\n")
            self.snapshot_write(state, f'/etc/systemd/network/00-ndimon-{iface}.network',
                                self.networkd_text(doc, row, tagged if row is untagged else []))
        if not untagged:
            self.snapshot_write(state, f'/etc/systemd/network/00-ndimon-{base}.network',
                                f'[Match]\nName={base}\n\n[Network]\nDHCP=no\nLinkLocalAddressing=no\nIPv6AcceptRA=no\n' +
                                ''.join('VLAN=' + self.interface(doc, r) + '\n' for r in tagged))
        # Old primary override must not shadow or add old VLAN attachments.
        old = f'/etc/systemd/network/05-ndimon-{base}.network'
        if os.path.exists(old):
            self.snapshot_write(state, old, None)
        for path in self.n.glob.glob('/etc/systemd/network/*.network.d/*.conf'):
            if self.n.vlan_networkd_owned_dropin(path, base):
                self.snapshot_write(state, path, None)
        self.n.run(['networkctl', 'reload'])
        self.n.run(['networkctl', 'reconfigure', base,
                    *[self.interface(doc, row) for row in tagged]], timeout=30)

    def networkd_text(self, doc, row, tagged=()):
        iface = self.interface(doc, row)
        management = row['id'] == doc['management']
        text = f'[Match]\nName={iface}\n\n[Link]\nRequiredForOnline=no\n\n[Network]\nLinkLocalAddressing=no\nIPv6AcceptRA=no\n'
        text += ''.join('VLAN=' + self.interface(doc, r) + '\n' for r in tagged)
        if row['mode'] == 'dhcp':
            text += f'DHCP=ipv4\n\n[DHCPv4]\nUseDNS={"yes" if management else "no"}\nUseRoutes={"yes" if management else "no"}\n'
        else:
            text += f"DHCP=no\nAddress={row['address']}/{row['prefix']}\n"
            if management:
                text += ''.join('DNS=' + server + '\n' for server in row['dns'])
                if row['gateway']:
                    text += 'Gateway=' + row['gateway'] + '\n'
        return text

    def netplan(self, state):
        import yaml
        doc = state['requested']
        parent = self.n.vlan_netplan_parent(doc['interface'])
        # Netplan merges sequence values across files. Clear the adopted port's
        # old IP lists at their source, so a static address/gateway cannot remain
        # active alongside the new profile. Other ports and keys are preserved.
        for path in self.n.netplan_files():
            if path in ('/etc/netplan/90-ndimon.yaml','/etc/netplan/91-ndimon-vlans.yaml'):
                continue
            original = yaml.safe_load(self.n.read_text(path)) or {}
            entry = original.get('network', {}).get('ethernets', {}).get(parent)
            if isinstance(entry, dict) and any(key in entry for key in ('addresses','routes','gateway4','nameservers')):
                for key in ('addresses','routes','gateway4','nameservers'):
                    entry.pop(key, None)
                self.snapshot_write(state, path, yaml.safe_dump(original, sort_keys=False), 0o600)
        ethernet = dict(dhcp4=False, dhcp6=False, addresses=[], routes=[],
                        nameservers=dict(addresses=[]), optional=True,
                        **{'accept-ra':False, 'link-local':[]})
        vlans = {}
        for row in doc['networks']:
            management = row['id'] == doc['management']
            entry = dict(dhcp4=row['mode'] == 'dhcp', dhcp6=False,
                         addresses=[], routes=[], nameservers=dict(addresses=[]), optional=True,
                         **{'accept-ra':False, 'link-local':[]})
            entry['dhcp4-overrides'] = {'use-routes': management, 'use-dns': management}
            if row['mode'] == 'static':
                entry['addresses'] = [f"{row['address']}/{row['prefix']}"]
                entry['nameservers'] = dict(addresses=row['dns'])
                if row['gateway']:
                    entry['routes'] = [dict(to='default', via=row['gateway'])]
            if row['vlan_id'] is None:
                ethernet = entry
            else:
                vlans[self.interface(doc, row)] = dict(id=row['vlan_id'], link=parent, **entry)
        self.snapshot_write(state, '/etc/netplan/90-ndimon.yaml', yaml.safe_dump(
            dict(network=dict(version=2, ethernets={parent: ethernet})), sort_keys=False), 0o600)
        self.snapshot_write(state, '/etc/netplan/91-ndimon-vlans.yaml', yaml.safe_dump(
            dict(network=dict(version=2, vlans=vlans)), sort_keys=False), 0o600)
        self.n.run(['netplan', 'generate'], timeout=30)
        self.n.run(['netplan', 'apply'], timeout=40)

    NM_FIELDS = ('ipv4.method', 'ipv4.addresses', 'ipv4.gateway', 'ipv4.dns', 'ipv4.never-default',
                 'ipv4.ignore-auto-dns', 'ipv4.ignore-auto-routes', 'ipv4.may-fail', 'ipv6.method',
                 'connection.autoconnect','connection.wait-device-timeout')

    def networkmanager(self, state):
        doc = state['requested']
        base_profile = self.n.nm_profile(doc['interface'])
        for row in doc['networks'] + ([None] if all(r['vlan_id'] is not None for r in doc['networks']) else []):
            iface = self.interface(doc, row) if row else doc['interface']
            name = base_profile if iface == doc['interface'] else 'ndimon-vlan-' + iface
            if name not in state['nm']:
                exists = self.n.run(['nmcli', '-g', 'connection.uuid', 'connection', 'show', name], check=False).returncode == 0
                if not exists and (row is None or row['vlan_id'] is None):
                    self.error('base NetworkManager profile is missing')
                previous = {self.interface(state['previous'], r) for r in state['previous']['networks']}
                if exists and iface != doc['interface'] and iface not in previous:
                    self.error('NetworkManager connection name already exists outside managed networks')
                state['nm'][name] = ({key: self.n.nm_field(name, key) for key in self.NM_FIELDS} if exists else None)
                self.write('pending', state)
                if not exists:
                    self.n.run(['nmcli', 'connection', 'add', 'type', 'vlan', 'con-name', name,
                                'ifname', iface, 'dev', doc['interface'], 'id', str(row['vlan_id'])])
            management = bool(row and row['id'] == doc['management'])
            args = ['nmcli', 'connection', 'modify', name,
                    'ipv4.method', ('auto' if row['mode'] == 'dhcp' else 'manual') if row else 'disabled',
                    'ipv4.addresses', f"{row['address']}/{row['prefix']}" if row and row['mode'] == 'static' else '',
                    'ipv4.gateway', row['gateway'] if row else '', 'ipv4.dns', ' '.join(row['dns']) if row else '',
                    'ipv4.never-default', 'no' if management else 'yes',
                    'ipv4.ignore-auto-dns', 'no' if management else 'yes',
                    'ipv4.ignore-auto-routes', 'no' if management else 'yes',
                    'ipv6.method', 'disabled', 'connection.autoconnect', 'yes',
                    'ipv4.may-fail', 'yes', 'connection.wait-device-timeout', '0']
            self.n.run(args)
            self.n.run(['nmcli', 'connection', 'up', name], timeout=40)

    def ifupdown(self, state):
        doc = state['requested']
        base = doc['interface']
        self.n.ifupdown_block(base)
        primary, tagged, guarded = f'auto {base}\niface {base} inet manual\n', '', []
        for row in doc['networks']:
            iface = self.interface(doc, row)
            text = f"auto {iface}\niface {iface} inet {row['mode']}\n"
            if row['vlan_id'] is not None:
                text += f'    vlan-raw-device {base}\n'
            if row['mode'] == 'static':
                text += f"    address {row['address']}/{row['prefix']}\n"
                if row['gateway']:
                    text += '    gateway ' + row['gateway'] + '\n'
                if row['dns']:
                    text += '    dns-nameservers ' + ' '.join(row['dns']) + '\n'
            elif row['id'] != doc['management']:
                guarded.append(iface)
            if row['vlan_id'] is None:
                primary = text
            else:
                tagged += text + '\n'
        self.snapshot_write(state, f'/etc/network/interfaces.d/ndimon-{base}', primary)
        self.snapshot_write(state, '/etc/network/interfaces.d/ndimon-vlans', tagged)
        hook = '# Managed by NDIMon network profiles\n'
        if guarded:
            hook += 'case "$interface" in\n' + '|'.join(guarded) + ')\n'
            hook += 'unset new_routers new_static_routes new_classless_static_routes new_rfc3442_classless_static_routes new_domain_name_servers new_domain_name new_domain_search\n;;\nesac\n'
        self.snapshot_write(state, '/etc/dhcp/dhclient-enter-hooks.d/ndimon-vlans', hook)
        for row in state['previous']['networks']:
            self.n.run(['ifdown', self.interface(state['previous'], row)], check=False, timeout=30)
        self.n.run(['ifup', base], timeout=40)
        for row in doc['networks']:
            if row['vlan_id'] is not None:
                self.n.run(['ifup', self.interface(doc, row)], timeout=40)

    def stdin_run(self, args, text, check=True):
        proc = subprocess.run(args, input=text, text=True, capture_output=True, timeout=20)
        if check and proc.returncode:
            self.error(proc.stderr.strip() or 'network command failed')
        return proc

    def firewall(self, state, restore=False, journal=True):
        if not restore and journal and 'nft_previous' not in state:
            old = self.n.run(['nft', '-j', 'list', 'table', 'inet', 'ndimon_management'], check=False)
            state['nft_present'] = old.returncode == 0
            state['nft_previous'] = self.n.run(['nft', 'list', 'table', 'inet', 'ndimon_management'], check=False).stdout if old.returncode == 0 else ''
            self.write('pending', state)
        if not restore:
            self.prepare_slices(state if journal else None)
        exists = self.n.run(['nft', 'list', 'table', 'inet', 'ndimon_management'], check=False).returncode == 0
        script = 'delete table inet ndimon_management\n' if exists else ''
        if restore:
            script += state.get('nft_previous', '')
        else:
            iface = self.interface(state['requested'], self.role(state['requested'], 'management'))
            names = ', '.join(json.dumps(self.interface(state['requested'], row)) for row in state['requested']['networks'])
            script += 'table inet ndimon_management {\n chain input {\n type filter hook input priority -5; policy accept;\n'
            script += f' iifname != {{ "lo", "{iface}" }} tcp dport 80 reject;\n }}\n'
            script += ' chain forward {\n type filter hook forward priority -5; policy accept;\n'
            script += f' iifname {{ {names} }} reject;\n oifname {{ {names} }} reject;\n }}\n}}\n'
            # nft's socket cgroup matching does not depend on systemd having
            # BPF_FRAMEWORK support (Ubuntu 24.04's systemd lacks it). Stable
            # parent slices retain the same IDs when individual services restart.
            script = script[:-2]
            for chain, direction in (('media_output','oifname'),('media_input','iifname')):
                hook='output' if direction=='oifname' else 'input'
                script += f' chain {chain} {{\n type filter hook {hook} priority 0; policy accept;\n'
                for role,slice_name in SLICES.items():
                    selected=self.interface(state['requested'],self.role(state['requested'],role))
                    script += f' socket cgroupv2 level 1 "{slice_name}" {direction} != {{ "lo", "{selected}" }} counter reject;\n'
                script += ' }\n'
            script += '}\n'
        if script:
            self.stdin_run(['nft', '-f', '-'], script)

    def prepare_slices(self, state=None):
        if state is not None:
            for name in SLICES.values():
                self.snapshot_write(state,'/etc/systemd/system/'+name,
                                    '[Unit]\nDescription=NDIMon network traffic boundary\n[Slice]\n')
            self.n.run(['systemctl','daemon-reload'])
        for name in SLICES.values():
            self.n.run(['systemctl','start',name])

    def remove_old(self, state):
        old, doc = state['previous'], state['requested']
        keep = {self.interface(doc, r) for r in doc['networks']}
        for row in old['networks']:
            iface = self.interface(old, row)
            if row['vlan_id'] is None or iface in keep:
                continue
            if doc['backend'] == 'networkmanager':
                name = 'ndimon-vlan-' + iface
                if name not in state['nm']:
                    state['nm'][name] = {key: self.n.nm_field(name, key) for key in self.NM_FIELDS}
                    self.write('pending', state)
                # Keep the connection until confirmation, preventing destructive rollback.
                self.n.run(['nmcli', 'connection', 'modify', name, 'connection.autoconnect', 'no'])
                self.n.run(['nmcli', 'connection', 'down', name], check=False)
            elif doc['backend'] == 'networkd':
                for suffix in ('.netdev', '.network'):
                    self.snapshot_write(state, f'/etc/systemd/network/00-ndimon-{iface}' + suffix, None)
            self.n.run(['ip', 'link', 'delete', iface], check=False)

    def publish(self, doc, write=None):
        # Interface names are validated; actual addresses are resolved by each consumer.
        runtime = {r: self.interface(doc, self.role(doc, r)) for r in ('management', 'ndi', 'tessera')}
        (write or self.n.atomic_write)(RUNTIME, json.dumps(runtime), 0o644)

    def services(self, state, rollback=False):
        doc = state['previous'] if rollback else state['requested']
        if not rollback:
            self.publish(doc, lambda path,text,mode: self.snapshot_write(state,path,text,mode))
            # Classify each service into the stable kernel filter boundary.
            for unit, role in (('ndimon-r','ndi'),('ndimon-finder','ndi')):
                text = '[Unit]\nRequires=ndimon-net-boot.service\nAfter=ndimon-net-boot.service\n[Service]\nSlice='+SLICES[role]+'\n'
                self.snapshot_write(state, f'/etc/systemd/system/{unit}.service.d/70-ndimon-network.conf', text)
            if os.path.isdir('/etc/tessera/config.d'):
                self.configure_tessera(doc, lambda path, text: self.snapshot_write(state, path, text))
        # During rollback the journal has already restored the original service
        # configuration, including the absence of drop-ins on first adoption.
        self.n.run(['systemctl', 'daemon-reload'])
        # Only restore services that were running before apply. In particular,
        # do not enable a plugin that the user had explicitly stopped.
        for unit in ('tesserad.service','ndimon-finder.service','ndimon-r.service','ndimon-watchdog.service'):
            if unit in state.get('running',[]) or self.n.systemctl_active(unit):
                self.n.run(['systemctl', 'restart', '--no-block', unit])

    def configure_tessera(self, doc, write):
        iface = self.interface(doc, self.role(doc, 'tessera'))
        write(TESSERA, '[network]\ninterfaces = [' + json.dumps(iface) + ']\nlocked = ["primary"]\n')
        write('/etc/systemd/system/tesserad.service.d/70-ndimon-network.conf',
              '[Unit]\nRequires=ndimon-net-boot.service\nAfter=ndimon-net-boot.service\n[Service]\nSlice='+SLICES['tessera']+'\n')

    def sync_tessera(self):
        with self.locked():
            if self.read('pending'):
                self.error('a network change is pending; finish it before installing or enabling Tessera')
            doc = self.read('committed')
            if doc:
                self.configure_tessera(doc, lambda path, text: self.n.atomic_write(path, text, 0o644))
                self.n.run(['systemctl','daemon-reload'])

    def wait_addresses(self, doc, roles=('management','ndi','tessera')):
        end = time.monotonic() + 45
        while True:
            addresses = {row['id']: self.n.live_address(self.interface(doc,row)) for row in doc['networks']}
            static_ready = all(addresses[row['id']] == (row['address'],row['prefix'])
                               for row in doc['networks'] if row['mode']=='static')
            if static_ready and all(addresses[doc[role]][0] for role in roles):
                return addresses
            if time.monotonic() >= end:
                self.error('a configured network did not obtain an IPv4 address')
            time.sleep(0.25)

    def apply(self, expected_token=None):
        with self.locked():
            state = self.read('pending')
            if not state or state['phase'] != 'scheduled':
                return
            if expected_token and not secrets.compare_digest(state['token'], expected_token):
                return
            state['phase'] = 'applying'
            self.write('pending', state)
            try:
                doc = state['requested']
                for unit in state.get('running',[]):
                    self.n.run(['systemctl','stop',unit],timeout=40)
                if any(r['vlan_id'] is not None for r in doc['networks']):
                    self.n.run(['modprobe', '8021q'])
                    self.snapshot_write(state, '/etc/modules-load.d/ndimon-8021q.conf', '8021q\n')
                getattr(self, doc['backend'])(state)
                self.remove_old(state)
                # All selected interfaces must have an address. DHCP/static collisions
                # are checked against actual assignments, not only configured subnets.
                addresses = self.wait_addresses(doc)
                subnets = []
                for address, prefix in addresses.values():
                    if not address:
                        continue
                    subnet = ipaddress.ip_network(f'{address}/{prefix}', strict=False)
                    if any(subnet.overlaps(other) for other in subnets):
                        self.error('live network subnets overlap')
                    subnets.append(subnet)
                for row in doc['networks']:
                    if row['mode'] == 'static':
                        probe = self.n.run(['arping', '-D', '-I', self.interface(doc, row), '-c', '2', '-w', '3', row['address']], check=False, timeout=10)
                        if probe.returncode:
                            self.error('duplicate IPv4 address on ' + row['name'])
                self.firewall(state)
                self.n.write_banner(dict(mode='static', address=addresses[doc['management']][0]))
                self.services(state)
                state['address'] = addresses[doc['management']][0]
                state['phase'] = 'applied'
                state['expires'] = time.monotonic() + CONFIRM_SECONDS
                self.write('pending', state)
                self.n.schedule('ndimon-profiles-rollback', CONFIRM_SECONDS, [self.n.HELPER, 'profiles-rollback', state['token']])
            except BaseException:
                self.restore(state)
                raise

    def confirm(self, token, address):
        with self.locked():
            state = self.read('pending')
            if not state or state['phase'] != 'applied' or not secrets.compare_digest(state['token'], token):
                self.error('no matching applied network change')
            if state['boot'] != self.n.read_text('/proc/sys/kernel/random/boot_id').strip() or time.monotonic() >= state['expires']:
                self.restore(state)
                self.error('network confirmation expired; previous configuration restored')
            iface = self.interface(state['requested'], self.role(state['requested'], 'management'))
            actual, _ = self.n.live_address(iface)
            if not address or address != actual or address != state['address']:
                self.error('confirm using the new management address')
            doc = copy.deepcopy(state['requested'])
            doc['revision'] += 1
            # Mark committed before removing the journal, so boot recovery can
            # distinguish a completed commit from a failed pending apply.
            self.write('committed', doc)
            state['phase'] = 'confirmed'
            self.write('pending', state)
            self.remove('pending')
            self.n.clear_banner()
            self.n.run(['systemctl', 'stop', 'ndimon-profiles-rollback.timer'], check=False)
            self.prune_nm(state)
            return dict(ok=True, revision=doc['revision'])

    def prune_nm(self, state):
        if state['requested']['backend'] == 'networkmanager':
            wanted = {self.interface(state['requested'], r) for r in state['requested']['networks']}
            for row in state['previous']['networks']:
                iface = self.interface(state['previous'], row)
                if row['vlan_id'] is not None and iface not in wanted:
                    self.n.run(['nmcli', 'connection', 'delete', 'ndimon-vlan-' + iface], check=False)

    def is_committed(self, state):
        expected = copy.deepcopy(state['requested'])
        expected['revision'] += 1
        return self.read('committed') == expected

    def restore(self, state):
        state['phase'] = 'reverting'
        self.write('pending', state)
        for unit in state.get('running', []):
            self.n.run(['systemctl', 'stop', unit], timeout=40)
        if state['previous']['backend'] == 'ifupdown':
            for row in state['requested']['networks']:
                self.n.run(['ifdown', self.interface(state['requested'], row)], check=False, timeout=30)
        for path, previous in state['files'].items():
            if previous is None:
                try:
                    os.unlink(path)
                except FileNotFoundError:
                    pass
            else:
                self.n.atomic_write(path, previous[0], previous[1])
        doc = state['previous']
        if doc['backend'] == 'networkmanager':
            for name, values in state['nm'].items():
                if values is None:
                    self.n.run(['nmcli', 'connection', 'delete', name], check=False)
                else:
                    args = ['nmcli', 'connection', 'modify', name]
                    for key, value in values.items():
                        args.extend([key, value])
                    self.n.run(args)
                    self.n.run(['nmcli', 'connection', 'up', name], timeout=40)
        elif doc['backend'] == 'netplan':
            self.n.run(['netplan', 'generate'])
            self.n.run(['netplan', 'apply'], timeout=40)
        elif doc['backend'] == 'networkd':
            self.n.run(['networkctl', 'reload'])
            self.n.run(['networkctl', 'reconfigure', doc['interface'],
                        *[self.interface(doc, row) for row in doc['networks'] if row['vlan_id'] is not None]], timeout=30)
        else:
            self.n.run(['ifdown', doc['interface']], check=False, timeout=30)
            self.n.run(['ifup', doc['interface']], timeout=40)
            for row in doc['networks']:
                if row['vlan_id'] is not None:
                    self.n.run(['ifup', self.interface(doc, row)], timeout=40)
        previous_interfaces = {self.interface(doc, r) for r in doc['networks']}
        for row in state['requested']['networks']:
            iface = self.interface(state['requested'], row)
            if row['vlan_id'] is not None and iface not in previous_interfaces:
                self.n.run(['ip', 'link', 'delete', iface], check=False)
        # networkd/Netplan acknowledge reconfiguration before every address is
        # restored. Keep the journal and services stopped until recovery is ready.
        self.wait_addresses(doc, roles=('management',))
        if 'nft_previous' in state:
            if state.get('nft_present'):
                self.n.run(['systemctl','daemon-reload'])
                self.prepare_slices()
            self.firewall(state, restore=True)
        self.services(state, rollback=True)
        self.remove('pending')
        self.n.clear_banner()

    def rollback(self, expected_token=None):
        with self.locked():
            state = self.read('pending')
            if expected_token and (not state or not secrets.compare_digest(state['token'], expected_token)):
                return
            if state and self.is_committed(state):
                self.publish(self.read('committed'))
                self.prune_nm(state)
                self.remove('pending')
                self.n.clear_banner()
            elif state and state['phase'] != 'confirmed':
                self.restore(state)
            elif state:
                self.remove('pending')

    def revert_token(self, token):
        with self.locked():
            state = self.read('pending')
            if not state or not secrets.compare_digest(state['token'], token) or self.is_committed(state):
                self.error('no matching pending network change')
            self.restore(state)
            return dict(ok=True)

    def boot(self):
        self.rollback()
        with self.locked():
            doc = self.read('committed')
            if doc:
                self.publish(doc)
                # nftables tables are not persistent across boot. Restore this
                # before the web service starts, independent of nftables.service.
                state = dict(requested=doc, files={}, nm={})
                self.firewall(state, journal=False)


def dispatch(net, command, args):
    profiles = Profiles(net)
    if command in ('profiles-stage', 'profiles-preview'):
        raw = net.sys.stdin.read(MAX_BYTES + 1)
        if len(raw) > MAX_BYTES:
            profiles.error('network document exceeds 64 KiB')
        doc = json.loads(raw)
        return profiles.stage(doc) if command == 'profiles-stage' else profiles.preview(doc)
    if command == 'profiles-status':
        with profiles.locked():
            return profiles.status()
    if command == 'profiles-confirm' and len(args) == 2:
        return profiles.confirm(*args)
    if command == 'profiles-revert' and len(args) == 1:
        return profiles.revert_token(args[0])
    if command == 'profiles-apply' and len(args) <= 1:
        profiles.apply(args[0] if args else None)
    elif command == 'profiles-sync-tessera' and not args:
        profiles.sync_tessera()
    elif command == 'profiles-rollback' and len(args) <= 1:
        profiles.rollback(args[0] if args else None)
    elif command == 'profiles-boot' and not args:
        profiles.boot()
    else:
        profiles.error('invalid network profile command')
    return dict(ok=True)
