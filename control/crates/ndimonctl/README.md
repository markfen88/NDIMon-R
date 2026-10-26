# ndimonctl

Operator command-line tool, installed with `ndimon-control` as `/usr/bin/ndimonctl`.
It has two jobs: a quick status report and an opt-in diagnostic collector for
tracking down receive drops. It does not change configuration; changes go through the
web UI or the HTTP API (`POST /api/v1/commands`), so nothing bypasses the state
actor.

```text
ndimonctl status
ndimonctl diag collect --out DIR --secs N --peer IP --nic NIC [--proc UNIT]
ndimonctl diag analyze DIR [CHANNEL]
ndimonctl --version
```

`--version` prints the crate's version, not the release's. The release version is in
`/etc/ndimon-firmware-version`.

## status

Prints `systemctl status` for `ndimon-r`, `ndimon-finder`, `ndimon-control` and
`ndimon-privd`, the decoder health from the loopback `/api/health` (the one
unauthenticated route, reachable from the device itself only), and the finder's
source list from `/var/lib/ndimon/ndimon-sources.json`.

## diag

`collect` samples once a second for `--secs` seconds. Run it as root: it opens a raw
packet capture of the sender (`--peer`) on the receiving interface (`--nic`). It
writes two CSV files into `--out`: `sys.csv` (per-CPU load, frequencies, temperature,
softirq, UDP and TCP counters, NIC counters, pressure-stall figures and the CPU use
of the process named by `--proc`, default `ndimon-r`, including its busiest thread)
and `pkt.csv` (the sender's packet and byte counts per second and its longest
inter-packet gap). `analyze` joins them with the decoder's per-channel telemetry,
which must exist as `tele/t-ch<CHANNEL>.csv` under the same directory (`CHANNEL`
defaults to 2), and prints a report that lines up SDK drops, timestamp gaps and frame
age with CPU, softirq and packet bursts. The decoder writes telemetry when it runs
with `NDIMON_TELEMETRY_CSV=<DIR>/tele/t`, one `-chN.csv` file per output.
