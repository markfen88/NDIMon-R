#!/usr/bin/python3 -I
"""Temporarily shorten RK3588's global ondemand response time while decoding."""
import fcntl
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import time

TARGET_US = 10000


class SamplingPolicy:
    def __init__(self, cpu=Path('/sys/devices/system/cpu/cpufreq'),
                 compatible=Path('/proc/device-tree/compatible'), runtime=Path('/run/ndimon')):
        self.cpu, self.compatible, self.runtime = cpu, compatible, runtime
        self.rate = cpu / 'ondemand/sampling_rate'
        self.state = runtime / 'cpu-sampling.json'

    def save(self, value):
        fd, name = tempfile.mkstemp(prefix='.cpu-sampling-', dir=self.runtime)
        try:
            with os.fdopen(fd, 'w') as stream:
                json.dump(value, stream)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(name, self.state)
        finally:
            if os.path.exists(name):
                os.unlink(name)

    def governors_match(self):
        policies = list(self.cpu.glob('policy*'))
        return bool(policies) and all((p / 'scaling_governor').read_text().strip() == 'ondemand'
                                     for p in policies)

    def read_rate(self):
        value = int(self.rate.read_text())
        if not 0 < value <= 60000000:
            raise ValueError('sampling interval outside supported range')
        return value

    def write_rate(self, value):
        self.rate.write_text(str(value) + '\n')
        deadline = time.monotonic() + .5
        while self.read_rate() != value and time.monotonic() < deadline:
            time.sleep(.01)
        if self.read_rate() != value:
            raise RuntimeError('kernel did not accept sampling interval')

    def restore(self):
        if not self.state.exists():
            return
        fd = os.open(self.state, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd) as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
                raise RuntimeError('unsafe sampling snapshot')
            saved = json.load(stream)
        if (set(saved) != {'original', 'applied', 'pending'} or
                any(type(saved[k]) is not int or not 0 < saved[k] <= 60000000
                    for k in ('original', 'applied')) or type(saved['pending']) is not bool):
            raise ValueError('invalid sampling snapshot')
        if self.rate.exists():
            current = self.read_rate()
            if saved['pending'] or current == saved['applied']:
                self.write_rate(saved['original'])
                print('[cpu-sampling] restored original sampling interval', file=sys.stderr)
            else:
                print('[cpu-sampling] preserved operator sampling change', file=sys.stderr)
        self.state.unlink()

    def apply(self):
        self.restore()
        if os.environ.get('NDIMON_CPU_SAMPLING') == 'off':
            print('[cpu-sampling] disabled by operator', file=sys.stderr)
            return
        if not self.compatible.exists():
            print('[cpu-sampling] not an RK3588 device', file=sys.stderr)
            return
        if b'rockchip,rk3588' not in self.compatible.read_bytes().split(b'\0'):
            print('[cpu-sampling] not an RK3588 device', file=sys.stderr)
            return
        if not self.rate.exists() or not self.governors_match():
            print('[cpu-sampling] global ondemand tuning unavailable', file=sys.stderr)
            return
        original = self.read_rate()
        if original <= TARGET_US:
            print('[cpu-sampling] operator interval already fast enough', file=sys.stderr)
            return
        saved = {'original': original, 'applied': TARGET_US, 'pending': True}
        self.save(saved)  # Record the old value before touching global kernel state.
        try:
            self.write_rate(TARGET_US)
            saved['pending'] = False
            self.save(saved)
        except Exception:
            self.restore()
            raise
        print('[cpu-sampling] ondemand sampling %d -> %d us (system-wide while running)' %
              (original, TARGET_US), file=sys.stderr)

    def run(self, action):
        self.runtime.mkdir(mode=0o755, parents=True, exist_ok=True)
        info = self.runtime.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise RuntimeError('unsafe runtime directory')
        fd = os.open(self.runtime / 'cpu-sampling.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            getattr(self, action)()


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in ('apply', 'restore'):
        return 2
    if os.geteuid() != 0:
        return 0
    try:
        SamplingPolicy().run(sys.argv[1])
    except (OSError, ValueError, RuntimeError, TypeError) as error:
        # CPU tuning must never stop the decoder from starting or stopping.
        print('[cpu-sampling] skipped: ' + str(error), file=sys.stderr)
    return 0


if __name__ == '__main__':
    sys.exit(main())
