#!/usr/bin/env python3
"""Build a host plugin archive from source and an explicitly pinned runtime."""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import tarfile

ROOT = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--plugin', default='tessera')
parser.add_argument('--bundle', type=Path)
parser.add_argument('--output', type=Path, default=ROOT / 'dist')
parser.add_argument('--release-tag')
args = parser.parse_args()
if not re.fullmatch(r'[a-z][a-z0-9-]{1,31}', args.plugin):
    parser.error('invalid plugin name')
source = ROOT / 'plugins' / args.plugin
manifest = json.loads((source / 'plugin.json').read_text())
if manifest['name'] != args.plugin or not re.fullmatch(r'[0-9][A-Za-z0-9.+:~\-]*', manifest['version']):
    parser.error('invalid plugin manifest identity')

payload = {}
for path in sorted(source.rglob('*')):
    if any(part in {'__pycache__', 'node_modules', 'tests', '.git'} for part in path.relative_to(source).parts):
        continue
    if path.is_symlink():
        parser.error('plugin source contains a symbolic link')
    if path.is_file():
        payload[path.relative_to(source).as_posix()] = (path.read_bytes(), 0o755 if path.stat().st_mode & 0o111 else 0o644)

# These files are required by the host even when lifecycle hooks do no work.
for field, default in (('routes', 'routes.js'), ('ui', 'ui.html'),
                       ('install', 'install.sh'), ('uninstall', 'uninstall.sh'),
                       ('backup', 'backup.js')):
    filename = manifest.get(field, default)
    if field == 'backup' and filename is None:
        continue
    if not isinstance(filename, str) or not re.fullmatch(r'[A-Za-z0-9_-]+\.[A-Za-z0-9]+', filename) or filename not in payload:
        parser.error('missing or invalid host entry file: ' + field)
for command in manifest.get('priv_commands', []):
    if not isinstance(command, str) or not re.fullmatch(r'[a-z][a-z0-9-]{1,31}', command):
        parser.error('invalid privileged command')
    entry = payload.get('priv/' + command)
    if not entry or not entry[1] & 0o111:
        parser.error('privileged command must exist and be executable: ' + command)

arch = 'any'
platform = None
if args.plugin == 'tessera':
    if not args.bundle:
        parser.error('Tessera requires --bundle with a pinned native runtime archive')
    archive = args.bundle.resolve(strict=True)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    locks = json.loads((ROOT / 'bundles.lock.json').read_text())
    matches = [entry for entry in locks['bundles'] if entry['sha256'] == digest and entry['asset'] == archive.name]
    if len(matches) != 1:
        parser.error('runtime is not in bundles.lock.json; review and pin it first')
    lock = matches[0]
    with tarfile.open(archive) as tf:
        candidates = [m for m in tf.getmembers() if m.isfile() and m.name.endswith('/manifest.json')]
        contracts = [m for m in tf.getmembers() if m.isfile() and m.name.endswith('/integration-contract.json')]
        if len(candidates) != 1 or len(contracts) != 1:
            parser.error('runtime manifest/integration contract missing or ambiguous')
        runtime = json.load(tf.extractfile(candidates[0]))
        contract = json.load(tf.extractfile(contracts[0]))
    for key in ('name', 'version', 'arch', 'commit', 'os'):
        if runtime[key] != lock[key]:
            parser.error('runtime manifest differs from lock: ' + key)
    compatibility = json.loads((ROOT / 'compatibility.json').read_text())
    if manifest['version'] != compatibility['tessera']['plugin_version'] or runtime['version'] != compatibility['tessera']['runtime_package']:
        parser.error('plugin/runtime version changed: update compatibility before building')
    if contract.get('control_wire_version') != compatibility['tessera']['control_wire_version']:
        parser.error('control API changed: update plugin compatibility before building')
    if manifest['min_ndimon'] != compatibility['tessera']['min_ndimon']:
        parser.error('host minimum differs from compatibility record')
    platform = lock['os']
    arch = lock['arch']
    embedded = {k: lock[k] for k in ('name', 'version', 'arch', 'commit', 'sha256', 'os')}
    payload['bundle-lock.json'] = (json.dumps(embedded, indent=2).encode() + b'\n', 0o644)
    payload['bundle-info.json'] = payload['bundle-lock.json']
    payload['compatibility.json'] = (json.dumps(compatibility['tessera'], indent=2).encode() + b'\n', 0o644)
    payload['bundle/' + archive.name] = (archive.read_bytes(), 0o644)
    payload['bundle/SHA256SUMS'] = (f'{digest}  {archive.name}\n'.encode(), 0o644)
elif args.bundle:
    parser.error('--bundle is only supported for Tessera')

epoch = int(os.environ.get('SOURCE_DATE_EPOCH', '0'))
args.output.mkdir(parents=True, exist_ok=True)
# Transport filenames use hyphens: GitHub rewrites tildes on upload.
# The manifest/catalog retain the exact Debian-ordered version.
asset_version = manifest['version'].replace('~', '-')
name = f"{args.plugin}-plugin_{asset_version}_{arch}.tar.gz"
dest = args.output / name
with dest.open('wb') as stream, gzip.GzipFile(filename='', fileobj=stream, mode='wb', mtime=epoch) as zipped:
    with tarfile.open(fileobj=zipped, mode='w') as tf:
        for name_in_archive, (data, mode) in sorted(payload.items()):
            info = tarfile.TarInfo(name_in_archive)
            info.size = len(data)
            info.mode = mode
            info.uid = info.gid = 0
            info.uname = info.gname = 'root'
            info.mtime = epoch
            tf.addfile(info, io.BytesIO(data))
sha = hashlib.sha256(dest.read_bytes()).hexdigest()
release_tag = args.release_tag or f"{args.plugin}-v{manifest['version']}"
from urllib.parse import quote
entry = {'name': args.plugin, 'version': manifest['version'], 'arch': arch,
         'asset_url': f"https://github.com/markfen88/NDIMon-Plugins/releases/download/{quote(release_tag, safe='')}/{quote(dest.name, safe='')}",
         'sha256': sha, 'size': dest.stat().st_size}
if platform:
    entry['platform'] = platform
(args.output / (dest.name + '.json')).write_text(json.dumps(entry, indent=2) + '\n')
archives = sorted(args.output.glob('*-plugin_*.tar.gz'))
(args.output / 'SHA256SUMS').write_text(''.join(f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n' for p in archives))
print(dest)
