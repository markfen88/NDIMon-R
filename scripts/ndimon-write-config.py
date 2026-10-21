#!/usr/bin/env python3
"""Bounded atomic writes for the unprivileged appliance API; no arbitrary paths."""
import grp
import json
import os
import re
import stat
import sys
import tempfile

MAX_BYTES = 4 * 1024 * 1024
JSON_NAMES = frozenset({
    'ndimon-audio-settings.json', 'ndimon-about-settings.json', 'ndimon-device-settings.json',
    'ndimon-rx-settings.json', 'ndimon-find-settings.json',
    'ndimon-splash-settings.json', 'ndimon-osd-settings.json',
    'ndimon-plugins.json', 'ndimon-presets.json', 'ndimon-auth.json', 'ndi-config.json', 'ndi-group.json',
})
LOGO_NAMES = frozenset('ndi-splash-logo.' + ext for ext in ('png', 'jpg', 'jpeg'))


def validate_name(name, removing=False):
    allowed = name in LOGO_NAMES if removing else (
        name in JSON_NAMES or name in LOGO_NAMES or
        re.fullmatch(r'ndimon-dec[1-8]-settings\.json', name) is not None)
    if not allowed:
        raise ValueError('unsupported config filename')


def validate_payload(name, data):
    if len(data) > MAX_BYTES:
        raise ValueError('config exceeds 4 MiB')
    if name in LOGO_NAMES:
        if not (data.startswith(b'\x89PNG\r\n\x1a\n') or data.startswith(b'\xff\xd8\xff')):
            raise ValueError('logo must be PNG or JPEG')
    else:
        json.loads(data)


def regular_target(dirfd, name):
    try:
        info = os.stat(name, dir_fd=dirfd, follow_symlinks=False)
    except FileNotFoundError:
        return
    if not stat.S_ISREG(info.st_mode):
        raise ValueError('config destination must be a regular file')


def apply(name, data=None, *, directory='/etc', uid=0, gid=None):
    removing = data is None
    validate_name(name, removing)
    if not removing:
        validate_payload(name, data)
    if gid is None:
        gid = grp.getgrnam('ndimon').gr_gid
    dirfd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
    temporary = None
    try:
        regular_target(dirfd, name)
        if removing:
            try:
                os.unlink(name, dir_fd=dirfd)
            except FileNotFoundError:
                pass
        else:
            fd, temporary = tempfile.mkstemp(prefix='.ndimon-config-', dir=directory)
            with os.fdopen(fd, 'wb') as f:
                os.fchown(f.fileno(), uid, gid)
                os.fchmod(f.fileno(), 0o640)
                f.write(data)
                f.flush()
                os.fsync(f.fileno())
            os.replace(temporary, name, dst_dir_fd=dirfd)
            temporary = None
        os.fsync(dirfd)
    finally:
        if temporary is not None:
            os.unlink(temporary)
        os.close(dirfd)


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in ('write', 'remove'):
        raise ValueError('usage: ndimon-write-config write|remove <basename>')
    action, name = sys.argv[1:]
    validate_name(name, action == 'remove')
    data = sys.stdin.buffer.read(MAX_BYTES + 1) if action == 'write' else None
    apply(name, data)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, KeyError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
