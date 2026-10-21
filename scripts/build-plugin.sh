#!/bin/bash
# Explicit local artifact only: no unpublished Debian/arm64 feed fallback.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $# = 3 && $1 = tessera && $2 = --ubuntu-experimental-bundle ]] || { echo 'usage: scripts/build-plugin.sh tessera --ubuntu-experimental-bundle /absolute/tessera-bundle_*.tar.gz' >&2; exit 2; }
bundle=$(realpath -e -- "$3")
[[ $bundle = /*/tessera-bundle_*_arm64_ubuntu-noble-experimental.tar.gz ]] || { echo 'experimental Ubuntu noble arm64 bundle required' >&2; exit 2; }
python3 - "$bundle" <<'PYVERIFY'
import hashlib,json,sys,tarfile
from pathlib import Path
bundle=Path(sys.argv[1]);lock=json.loads(Path('plugins/tessera/bundle-lock.json').read_text())
if hashlib.sha256(bundle.read_bytes()).hexdigest()!=lock['sha256']: raise SystemExit('bundle SHA256 differs from reviewed lock')
with tarfile.open(bundle) as tar:
 members=[m for m in tar.getmembers() if m.name.endswith('/manifest.json')]
 if len(members)!=1: raise SystemExit('bundle manifest missing')
 manifest=json.load(tar.extractfile(members[0]))
for key in ('name','version','arch','commit'):
 if manifest.get(key)!=lock[key]: raise SystemExit('bundle '+key+' differs from reviewed lock')
if any(manifest.get('os',{}).get(k)!=v for k,v in lock['os'].items()): raise SystemExit('bundle OS differs from reviewed lock')
PYVERIFY
version=$(node -p 'require("./plugins/tessera/plugin.json").version')
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/tessera/bundle" dist/plugins
cp -a plugins/tessera/. "$stage/tessera/"
cp plugins/tessera/bundle-lock.json "$stage/tessera/bundle-info.json"
find "$stage/tessera" -type d -name __pycache__ -prune -exec rm -rf {} +
cp -- "$bundle" "$stage/tessera/bundle/"
(cd "$stage/tessera/bundle" && sha256sum "$(basename "$bundle")" > SHA256SUMS)
archive="tessera-plugin_${version}_arm64_ubuntu-noble-experimental.tar.gz"
tar --sort=name --owner=0 --group=0 --numeric-owner -czf "dist/plugins/$archive" -C "$stage/tessera" .
(cd dist/plugins && sha256sum "$archive" > "$archive.sha256")
printf '%s\n' "$PWD/dist/plugins/$archive"
