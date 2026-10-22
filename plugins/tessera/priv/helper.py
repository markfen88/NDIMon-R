#!/usr/bin/python3 -I
"""Fixed Tessera operations. Never accepts a path, shell command, or TOML."""
import contextlib
import ipaddress
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

PLUGIN = Path(__file__).resolve().parent.parent
FLOW = Path('/etc/tessera/config.d/61-ndimon-flows.toml')
ALSA = Path('/etc/alsa/conf.d/60-ndimon-tessera.conf')
RANGES = Path('/etc/alsa/conf.d/61-ndimon-tessera-ranges.conf')
CONFIG = Path('/etc/tessera')
RUNTIME = Path('/run')

def fail(message):
    raise ValueError(message)

def run(args, **kwargs):
    return subprocess.run(args, check=True, timeout=300, **kwargs)

def safe_parent(path, create=True):
    for parent in reversed((path.parent, *path.parent.parents)):
        if parent.exists() and (parent.is_symlink() or parent.stat().st_uid != 0 or parent.stat().st_mode & 0o022):
            fail('unsafe root configuration parent: '+str(parent))
    if create: path.parent.mkdir(parents=True, exist_ok=True)
    if path.is_symlink() or path.exists() and (not path.is_file() or path.stat().st_uid != 0):
        fail('unsafe configuration target')

def atomic(path, text):
    safe_parent(path)
    fd, temp = tempfile.mkstemp(prefix='.'+path.name, dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as stream:
            stream.write(text); stream.flush(); os.fsync(stream.fileno())
        os.chmod(temp, 0o644); os.replace(temp, path)
        directory = os.open(path.parent, os.O_DIRECTORY)
        try: os.fsync(directory)
        finally: os.close(directory)
    finally:
        with contextlib.suppress(FileNotFoundError): os.unlink(temp)

def render_flows(flows):
    if not isinstance(flows,list) or len(json.dumps(flows).encode())>60000: fail('flows exceed 60KB plugin write limit')
    ids=set(); rx=0; lines=['# NDIMon-R Tessera plugin; static flows, all device channels']
    for f in flows:
        if not isinstance(f,dict) or set(f)-{'id','direction','peer','port','frames_per_packet','any_source'}: fail('unknown flow fields')
        import re
        if not isinstance(f.get('id'),str) or not re.fullmatch('[A-Za-z0-9_-]{1,32}',f['id']) or f['id'] in ids: fail('unique flow id required')
        ids.add(f['id'])
        if f.get('direction') not in ('rx','tx'): fail('invalid direction')
        if type(f.get('port')) is not int or not 1024<=f['port']<=65535: fail('invalid port')
        if 'any_source' in f and type(f['any_source']) is not bool: fail('any_source must be boolean')
        if f['direction']=='rx': rx+=1
        if rx>1: fail('only one RX flow; all channels are carried')
        if f.get('any_source'):
            if f['direction']!='rx' or f.get('peer'): fail('any_source excludes peer and is RX only')
        else:
            if not isinstance(f.get('peer'),str): fail('peer must be a string IPv4 address')
            peer=ipaddress.IPv4Address(f.get('peer',''))
            if peer.is_multicast or peer.is_loopback or peer.is_unspecified or int(peer)>>24 in (0,255) or peer.is_reserved: fail('peer must be unicast IPv4')
        if 'frames_per_packet' in f and (f['direction']!='tx' or type(f['frames_per_packet']) is not int or not 1<=f['frames_per_packet']<=1024): fail('invalid frames_per_packet')
        lines += ['[[flow]]','id = '+json.dumps(f['id']),'direction = '+json.dumps(f['direction']),'device = "main"','channels = []']
        lines += ['any_source = true'] if f.get('any_source') else ['peer = '+json.dumps(f['peer'])]
        lines += ['port = '+str(f['port'])]
        if 'frames_per_packet' in f: lines += ['frames_per_packet = '+str(f['frames_per_packet'])]
    return '\n'.join(lines)+'\n'

def write_flows(flows):
    text=render_flows(flows); safe_parent(FLOW)
    with tempfile.TemporaryDirectory(prefix='ndimon-tessera-config-',dir=RUNTIME) as tmp:
        stage=Path(tmp)
        for source in CONFIG.rglob('*'):
            if source.is_symlink(): fail('symlink in Tessera configuration')
        shutil.copytree(CONFIG,stage,dirs_exist_ok=True)
        (stage/'config.d').mkdir(exist_ok=True)
        (stage/'config.d'/FLOW.name).write_text(text)
        run(['/usr/bin/tesseractl','config','check','--config-dir',str(stage)])
    atomic(FLOW,text)

def write_alsa(rx,tx):
    if type(rx) is not int or type(tx) is not int or not 1<=rx<=16 or not 1<=tx<=16: fail('PCM channels must be 1..16')
    atomic(ALSA, '# NDIMon-R Tessera plugin\n'+f'pcm.ndimon_net_in {{ type tessera device "main" first_channel 1 channels {rx} client "ndimon-r" }}\n'+f'pcm.ndimon_net_out {{ type tessera device "main" first_channel 1 channels {tx} client "ndimon-r" }}\n')

def write_pcms(request):
    if not isinstance(request,list) or len(request)>128: fail('PCM request must contain at most 128 ranges')
    entries=set()
    for item in request:
        if not isinstance(item,dict) or set(item)!={'direction','first_channel','channels'}: fail('invalid PCM range fields')
        direction,first,count=item['direction'],item['first_channel'],item['channels']
        if direction not in ('rx','tx') or type(first) is not int or type(count) is not int or not 1<=first<=65535 or not 1<=count<=16 or first+count>65536: fail('invalid PCM range')
        entries.add((direction,first,count))
    text='# NDIMon-R Tessera endpoint ranges; generated from the audio graph\n'
    for direction,first,count in sorted(entries):
        text+=f'pcm.ndimon_tessera_{direction}_{first}_{count} {{ type tessera device "main" first_channel {first} channels {count} client "ndimon-r" }}\n'
    atomic(RANGES,text)

def bundle(action, option=None):
    archives=list((PLUGIN/'bundle').glob('tessera-bundle_*.tar.gz'))
    if len(archives)!=1: fail('exactly one verified local bundle is required')
    archive=archives[0]
    import hashlib
    lock=json.loads((PLUGIN/'bundle-lock.json').read_text())
    expected=(PLUGIN/'bundle'/'SHA256SUMS').read_text().split()
    if len(expected)!=2 or expected[1].lstrip('*')!=archive.name or expected[0]!=lock['sha256'] or hashlib.sha256(archive.read_bytes()).hexdigest()!=expected[0]: fail('bundle checksum mismatch')
    with tempfile.TemporaryDirectory(prefix='ndimon-tessera-bundle-') as tmp:
        with tarfile.open(archive) as tar:
            for member in tar.getmembers():
                if member.issym() or member.islnk() or not (member.isfile() or member.isdir()): fail('unsafe bundle member')
            tar.extractall(tmp,filter='data')
        roots=list(Path(tmp).rglob('install.sh'))
        if len(roots)!=1: fail('invalid bundle layout')
        directory=roots[0].parent
        manifest=json.loads((directory/'manifest.json').read_text())
        if any(manifest.get(k)!=lock[k] for k in ('name','version','arch','commit')) or any(manifest.get('os',{}).get(k)!=v for k,v in lock['os'].items()): fail('bundle manifest differs from reviewed lock')
        if action=='install':
            args=['/bin/bash',str(directory/'install.sh'),'--profile','ndimon','--admin-user','ndimon','--yes']
            platform=lock.get('os',{})
            if platform.get('id')=='ubuntu' and platform.get('version_id')=='24.04' and lock['arch']=='arm64':
                args+=['--allow-ubuntu-24.04']
            elif not (platform.get('id')=='debian' and platform.get('version_id')=='13' and lock['arch'] in ('amd64','arm64')):
                fail('unsupported runtime bundle platform')
            if option=='restart': args+=['--restart']
        else: args=['/bin/bash',str(directory/'uninstall.sh'),'--yes']+(['--purge'] if option=='purge' else [])
        run(args)

def main(argv):
    if os.geteuid()!=0: fail('requires root')
    if not argv: fail('operation required')
    action,*args=argv
    if action=='write-flows' and not args:
        raw=sys.stdin.buffer.read(65537)
        if len(raw)>65536: fail('flow request exceeds 64KiB')
        write_flows(json.loads(raw))
    elif action=='write-pcms' and not args:
        raw=sys.stdin.buffer.read(65537)
        if len(raw)>65536: fail('PCM request exceeds 64KiB')
        write_pcms(json.loads(raw))
    elif action=='write-alsa' and len(args)==2: write_alsa(*map(int,args))
    elif action=='service' and len(args)==1 and args[0] in ('start','stop','restart','reload'):
        run(['/usr/bin/systemctl',args[0],'tesserad.service'])
    elif action=='enable' and not args:
        run(['/usr/local/sbin/ndimon-priv','net-profiles-sync-tessera'])
        run(['/usr/bin/systemctl','start','tesserad.service'])
    elif action=='disable' and not args:
        # Framework disable removes only this plugin's audio; legacy NDI remains.
        run(['/usr/bin/node','/opt/ndimon-r/api/audio-settings-cli.js','select-v1'])
        run(['/usr/bin/node','/opt/ndimon-r/api/audio-settings-cli.js','remove','net-in','net-out'])
        run(['/usr/bin/systemctl','stop','tesserad.service'])
    elif action in ('bundle-install','bundle-uninstall') and (not args or args in (['restart'],['purge'])):
        if args and (action=='bundle-install' and args!=['restart'] or action=='bundle-uninstall' and args!=['purge']): fail('invalid bundle option')
        bundle('install' if action=='bundle-install' else 'uninstall',args[0] if args else None)
    elif action=='install' and not args:
        run(['/usr/local/sbin/ndimon-priv','net-profiles-sync-tessera'])
        bundle('install');write_alsa(2,2)
        run(['/usr/bin/node','/opt/ndimon-r/api/audio-settings-cli.js','register-tessera'])
    elif action=='uninstall' and (not args or args==['purge']):
        run(['/usr/bin/node','/opt/ndimon-r/api/audio-settings-cli.js','select-v1'])
        run(['/usr/bin/node','/opt/ndimon-r/api/audio-settings-cli.js','remove','net-in','net-out'])
        bundle('uninstall',args[0] if args else None)
        for path in (FLOW,ALSA,RANGES,Path('/etc/tessera/config.d/62-ndimon-network.toml'),Path('/etc/systemd/system/tesserad.service.d/70-ndimon-network.conf')):
            if path.parent.exists():
                safe_parent(path,create=False);path.unlink(missing_ok=True)
        run(['/usr/bin/systemctl','daemon-reload'])
    else: fail('invalid operation or arguments')

if __name__=='__main__':
    try: main(sys.argv[1:])
    except (ValueError,OSError,subprocess.SubprocessError,tarfile.TarError) as error:
        print(str(error),file=sys.stderr);sys.exit(2)
