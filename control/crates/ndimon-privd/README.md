# ndimon-privd

The root privilege boundary of NDIMon-R: a socket-activated Rust daemon with a
closed, typed request set. Only `ndimon-control` reaches it. No request carries a
path, executable, shell fragment, arbitrary argv or TOML. Third-party
`PluginExec` takes a command the plugin's manifest declared, with **no arguments
and no stdin**; a command that needs arguments needs its own typed variant (the
Tessera operations have them). The wire types and the client live in
`ndimon-privd-proto`; this crate holds the handlers, the socket server and the
policy.

## Socket and wire

`ndimon-control` has its own listening socket, `/run/ndimon-privd/control.sock`,
in the root-owned, world-traversable `/run/ndimon-privd/`. The socket unit,
`ndimon-privd.socket`, passes the descriptor to the daemon (`Sockets=`):

| Unit | Path | Mode, owner:group | Caller | Group granted through |
|------|------|-------------------|--------|-----------------------|
| `ndimon-privd.socket` | `control.sock` | 0660 root:`ndimon-priv-control` | `ndimon-control` | its `SupplementaryGroups=` |

`listenfd` takes the inherited listener; the daemon never binds its own socket.
No account is persistently a member of the group (systemd's `User=` setup loads
every group of the account, so membership would reach every unit running as it).
The finder, which parses LAN traffic, has its own account, no group and no path
into `/run/ndimon-privd`. There is no second caller, and the daemon refuses a
descriptor bound to any other path.

**The accepting socket is the caller's identity.** The daemon identifies each
inherited descriptor by the path it is bound to (and cross-checks the
`FileDescriptorName=`), then applies that caller's closed request table
(`policy::authorize`) to every request on it; control's is the complete set. A
second caller would need its own socket, group and an explicit table. This needs
nothing from the kernel, so it holds on every supported kernel, including the
vendor 5.10/6.1 trees of the Rockchip boards. At startup the daemon refuses to
serve a socket layout with a foreign owner, any world access, or a socket
directory others can modify.

**Defence in depth where the kernel allows.** On Linux 6.5+ the daemon also takes
the socket's `SO_PEERPIDFD`, pins `/proc/<pid>`, reads the peer's cgroup and
rechecks liveness; the cgroup's exact system unit (`ndimon-control.service`) must
be the one the socket stands for, and a mismatch is refused. Without
`SO_PEERPIDFD` (`ENOPROTOOPT`, Linux before 6.5) this check is skipped and the
socket alone decides; there is no PID-based substitute, which could pin a
recycled PID. The daemon logs the active mode at every start (`peer_check`, with
the reason). `NDIMON_PRIVD_PEER_PIDFD=off` skips the check on any kernel, to
exercise the older-kernel path; no kernel version ever makes the daemon refuse to
serve. A root session passes the socket's file permissions and, without the pidfd
check, is served as the socket's caller; root already holds every privilege this
daemon grants.

One request and one reply per connection, a `u32` big-endian byte length followed
by JSON:

```json
{"v":1,"id":7,"req":{"set_hostname":{"name":"studio-1"}}}
{"v":1,"id":7,"ok":{"stdout":"","stderr":""}}
{"v":1,"id":7,"err":{"invalid":{"field":"name","reason":"invalid hostname"}}}
```

Unit variants are strings (`"reboot"`). Tags and fields are snake_case with
serde's external tag, as in the public contract. Errors are `invalid`,
`permission_denied`, `busy`, `failed`, `unsupported` or `internal`; a failed
program includes its exit code when available. Clients check the version and the
ID. Frames are 1..8 MiB; a legacy JSON or logo write is at most 4 MiB after
encoding or decoding respectively. Flow sets keep the 60 KB limit and PCM requests
at most 128 ranges.

The daemon serves at most 32 concurrent connections, bounds frame read and write
time to 10 seconds, and exits after 60 idle seconds with no active connection
(`NDIMON_PRIVD_IDLE_SECONDS`, 5..3600). The socket stays with systemd. Journald
receives structured tracing fields `peer_uid`, `request_id`, `request_kind`,
`outcome`; read-only network polls and release lookups are logged at debug. Request bodies, passwords
and confirmation tokens are never logged.

The binary has four more root-only entry points that share the handlers:
`--job ID` (a long-job worker), `--cpu apply|restore` (decoder start and stop
hooks) and `network-job boot|apply ID|recover ID` (see
[the network engine](../ndimon-netengine/README.md)); `--network-boot` is kept as
an alias of `network-job boot`.

Every network request and job builds the engine from
`/etc/ndimon-network-certified-backends.json` (`network::certification`): the
backends listed there may change, all others are read-only. The `ndimon-privd`
package ships the file as a conffile listing all four. A file that is missing,
unreadable (not a regular root-owned file others cannot write), malformed or empty
allows no backend; privd logs the reason once at warn level and still serves the
request, so the boot job, which the other services depend on, never fails on it.

## Request coverage

`Host::handle` is an exhaustive match without a wildcard: adding a variant needs a
handler and a payload-free audit label. Every external process has an absolute
executable, explicit argv, a cleared environment and a `/usr/bin/timeout`
deadline; output is capped at 512 KiB per stream, and no shell command text is
constructed. `systemctl` and `systemd-run` always mean `/usr/bin/systemctl` and
`/usr/bin/systemd-run`.

### Used by the control plane

| Request | Behavior | External programs |
|---|---|---|
| `RenderArtifacts` | Read `state.json`, verify its revision, render every native artifact set with the shared renderer and write only what changed (a first-time overwrite keeps `<file>.pre-control`); returns the manifest | none |
| `WriteLegacyConfig` | Atomic write of one allowlisted `/etc` JSON. Refused for every file a native domain owns, so it serves only the password record `ndimon-auth.json` (control sign-in and restore) | none |
| `ServiceControl` | Start, stop or restart the decoder, finder or control unit (control with `--no-block`, so its own reply is delivered) | `systemctl` |
| `RestartStack` | Queue a restart of decoder, finder and control | `systemctl --no-block` |
| `Reboot` | Reboot the host | `/sbin/reboot` |
| `SetHostname` | Apply one validated DNS label | `/usr/bin/hostnamectl` |
| `SetTimeSync` | Write or remove the timesyncd drop-in (and the chrony source when chrony is installed), enable and restart the clock service | `timedatectl`, `systemctl` |
| `SetRebootSchedule` | Write or remove the local-time timer (`Persistent=false`); duplicate weekdays are refused | `systemctl` |
| `InstallUpdate { version, force }` | Start the signed-release worker (below) | `systemd-run` |
| `CheckUpdate` | Look up the newest signed release this device could install (below); read-only, bounded, answers `{"latest_version": VERSION or null}` | HTTPS to `api.github.com` and the release assets |
| `NetworkNative { call }` | Status, observe, preview, stage, pending, confirm, revert, rollback and the four handoff calls of the network engine; carries typed policy, the trusted socket context and token-scoped pending reads | `systemd-run`, backend tools |
| `PluginInspect` | Snapshot and validate an archive and its manifest | `systemd-run`, root worker |
| `PluginInstall` | Validate, stage, run the install hook, grant the scoped groups and paths; restore the old code when the hook fails | `systemd-run`, root worker, `dpkg`, the plugin's hook, `systemctl` |
| `PluginUninstall` | Run the uninstall hook or force-remove; optionally purge state | `systemd-run`, root worker, the plugin's hook, `systemctl` |
| `PluginExec` | Check manifest and generated `ALLOWED`, root ownership and modes; run the declared command (for Tessera only `enable` and `disable`) | `systemd-run`, the installed executable |
| `PluginServiceControl` | Start, stop or restart a unit an installed manifest scopes to its plugin | `systemctl` |
| `PluginJobStatus`, `PluginJobCancel` | Read a durable result and progress, or stop one job unit | `systemctl` |
| `PluginRuntimeStart`, `PluginRuntimeStop` | Start or stop a manifest-v2 plugin process (below) | `systemd-run`, `systemctl` |
| `PluginResetState` | Require an empty accepted native registry, then recreate only `/etc/ndimon-plugins.json` | none |

### Implemented, with no caller in the shipped control plane

These variants stay in the closed set and are tested, but nothing in the packaged
stack sends them:

- `WriteLegacyLogo`, `RemoveLogo`: refused while the display domain is native
  (logos are rendered from the managed asset store).
- `RestartFinder`, `StopCore`, `StartCore`: `ServiceControl` covers them.
- `StartUpdate`: a wire tombstone that always answers `unsupported` (unsigned
  updates do not exist).
- `CpuTuning`: the decoder unit calls the `--cpu apply|restore` entry points
  instead. They journal the RK3588 global `ondemand` sampling interval and restore
  it without overwriting a later operator change; other boards and governors are
  skipped.
- `PluginRuntimeStatus`: reads the active state and UID of a plugin process unit.
- `TesseraWriteFlows`, `TesseraWriteAlsa`, `TesseraWritePcms`: refused with
  "domain is native; submit a control-plane command" once the Tessera domain is
  native and `state.json` exists (the renderer writes those files).
- `TesseraBundle`, `TesseraEnable`, `TesseraDisable`: the plugin install, uninstall
  and `PluginExec` paths call the same handlers directly (bundle checksum, lock
  and platform check, then the bundle's own `tessera-bundle install` or
  `uninstall` with fixed flags, run from a root-private unpacking under
  `/var/lib/ndimon-privd/bundles` (not `/run`, which is `noexec`) that is removed
  when the job ends; enable synchronizes the network binding and starts
  `tesserad`); they are not sent on their own.
- The legacy-shape `Network*` variants (`NetworkStatus`, `NetworkCheckAddress`,
  `NetworkApply`, `NetworkConfirm`, `NetworkRollback`, `NetworkVlan*`,
  `NetworkProfiles*`): they dispatch to the native engine, and immediate VLAN
  mutations are refused in favour of complete profile transactions.
  `NetworkProfilesSyncTessera` is also called inside the daemon by the Tessera
  enable path.

## Jobs and plugin safety

Plugin inspection, lifecycle, exec, flow validation and the Tessera bundle,
enable and disable operations start independent system-manager units named
`ndimon-plugin-<name>-<id>.service`, with a 600-second runtime limit
(`ProtectSystem=no`, so a trusted hook can write what it installs). A request
returns `ok.job`. `PluginJobStatus` returns `data.result` (an `ok` or `err`
object) and `data.progress` once available; `stdout` is `active` or `inactive`
for the control plane's recovery guard. Results and request snapshots are
root-private under `/run/ndimon/privd-jobs`. The worker stores the result before
removing the request. A killed worker has no success result: clients fail rather
than assuming it completed. Runtime state is cleaned on reboot and by the tmpfiles
policy. A disconnected client does not cancel the job. `Client::execute` waits for
the durable result; `Client::request` only acknowledges that a job started.

Archives use typed download and import IDs resolved below the two fixed upload
roots. The source is opened without following any directory or file symlink, its
uploader UID is checked, and a root-owned snapshot is copied **from that file
descriptor**. The registry parent policy (`check_state_parent`) applies to both
archive and runtime-state traversal. Expanded data is limited to 1 GiB and 20,000
entries, and the gzip stream, including tar metadata records, is held to a finite
budget; links, devices, traversal, control characters and duplicates are
refused. The operation lock is held through hook completion. A failed install
keeps a broken marker; a failed upgrade restores the previous code, and the
`.previous-*` recovery on a later operation survives an interrupted swap. Scoped
service, group and path checks apply before and after hooks. The groups
`ndimon-priv-control` and `ndimon-control` are reserved, and so are the retired
`ndimon-priv` and `ndimon-priv-api`, next to the denied system groups. Installed
plugin hooks are the plugin author's own executables and run as root; Tessera's
install and uninstall are done in Rust.

A manifest-v2 plugin process is a different path: `PluginRuntimeStart` re-reads the
installed manifest, pins a regular executable below the root-owned plugin
directory (no symlink, traversal or absolute path) and starts a transient
`ndimon-plugin-runtime-<name>.service` with `DynamicUser=yes`, no capabilities,
`NoNewPrivileges`, `ProtectSystem=strict`, private `/tmp` and devices,
`RestrictAddressFamilies=AF_UNIX`, only the manifest's writable paths and groups, and
`BindsTo=ndimon-control.service`. It receives one file, the plugin's own
`control.sock` below `/run/ndimon-control/plugins/<name>/`, as
`NDIMON_PLUGIN_SOCKET`; the privd directory is inaccessible. A process asks for
privilege only through the host, which sends `PluginExec` with a command its
manifest declares. See [plugins](../../../docs/plugins.md).

## Signed updates

`CheckUpdate` is the release lookup behind the web **Update now** button. It uses
the same key list, `release-source` and host allowlist as an install and no job: it
asks `releases/latest` of the repository, and only when that release is newer than
`/etc/ndimon-firmware-version` (`dpkg --compare-versions`) does it download and
verify `SHA256SUMS` and its signature (trusted key, product and version in the
trusted comment) and check that the list names the four packages for this
architecture. Anything unverified fails the lookup instead of being reported.
Connections are cut after 4 s, reads after 5 s and requests after 8 s, and no
request starts 20 s after the first. The control plane caches the answer.

`InstallUpdate` runs the worker as `ndimon-plugin-update-<id>.service` (30-minute
limit, exclusive update lock) using `ndimon-release`, and reports stages through
the job's progress file. It resolves the GitHub release for the exact tag
`vVERSION` of the configured `owner/repository` (default `markfen88/NDIMon-R`,
overridable by a root-owned `/etc/ndimon/release-source`), downloads only the
signed `SHA256SUMS` and `SHA256SUMS.minisig` and the `.deb` assets for the local
architecture (plus `all`), verifies the signature against the trusted keys, checks
every selected checksum, then checks each package's name, version and
architecture with `dpkg-deb`. A release must contain exactly one each of
`ndimon-control`, `ndimon-privd`, `ndimon-runtime` and `ndimon-ui`; others and
duplicates are refused. Downgrades need `force`, which never bypasses a
signature, hash or identity. Only after every package verifies does the worker
run `dpkg -i` on the staged files (fixed argv) and write
`/etc/ndimon-firmware-version`; a partial dpkg failure is reported, not rolled
back.

Downloads have 15-second connect, 30-second read and 120-second request limits and
at most four redirects; metadata is limited to 1 MiB, each package to 256 MiB, the
total to 1 GiB and 32 assets. The metadata request goes to `api.github.com`
without redirects; asset redirects may only reach HTTPS `github.com`,
`objects.githubusercontent.com` and `release-assets.githubusercontent.com`.
Packages are staged in a private directory below `/var/lib/ndimon-updates` and
removed on completion. `dpkg` runs with `--force-confdef --force-confold`: nobody can
answer a conffile question, and an operator's edit of one (the list of certified
network backends) wins. The compiled trust list is empty; root provisions
`/etc/ndimon/trusted-release-keys` (see [releases](../../../docs/releases.md)).
Without a key every verification fails and the control plane reports that updates
need a signed release; `NDIMON_PRIVD=0` in the control unit's environment also
makes the control plane refuse the request. (The packaged unit sets
`NDIMON_PRIVD=1`; nothing else reads that variable.)

## Writer and owner table

The writer opens each directory with `openat(O_DIRECTORY|O_NOFOLLOW)`, writes a
new file with `O_EXCL`, assigns owner and mode, fsyncs, renames within the pinned
directory, then fsyncs the directory. Linked or non-regular destinations are
refused.

| Artifact class | Owner:group | Mode |
|---|---|---|
| Rendered `/etc/ndimon-*.json`, `/etc/ndi-*.json`, splash logo files and the password record | root:ndimon | 0640 |
| Tessera TOML and ALSA configuration | root:root | 0644 |
| Network binding, unit drop-ins and slices | root:root | 0644 |
| Timesync and chrony configuration and the reboot timer units | root:root | 0644 |
| CPU snapshot, job request and result snapshots | root:root | 0600 |
| Plugin runtime state directory | ndimon:ndimon | 0700 |
| Installed plugin files and directories | root:root | 0644 / 0755 |

The renderer is the shared `ndimon-render` registry; the daemon renders the whole
set successfully before it touches a destination. Mixed files (the device
settings and the decoder files) are reread under `/etc/.ndimon-config.lock`, so an
edit that is not the actor's survives a render; see
[the state crate](../ndimon-state/README.md).

## Packaging, capabilities and rollback

The four native Debian packages declare accounts and directories in
`packaging/debian/{sysusers.d,tmpfiles.d}/ndimon.conf`. The shared runtime parent
is `0755 root:root`; the finder writes only its own output and SDK subdirectories
plus the fixed root-owned trigger. The native maintainer hook removes historical
persistent privileged-group memberships, the retired API socket, unit and group,
and superseded manual declarations before activating units, and preserves
unrelated groups and migration recovery copies. Normal operation never uses sudo
and there is no shell helper.

The daemon capability set is:

- `CAP_CHOWN`: artifact ownership and plugin runtime directories.
- `CAP_DAC_OVERRIDE`: read uploaded archives and replace root-owned artifacts.
- `CAP_FOWNER`: metadata, private snapshots and atomic target replacement.
- `CAP_SYS_BOOT`: `/sbin/reboot`'s direct fallback.
- `CAP_NET_RAW`: address checks use arping.
- `CAP_NET_ADMIN`: nftables ownership probes.

System-manager operations use root-authenticated D-Bus, not daemon-side mounts or
clock syscalls. Workers run in separate units without inheriting the daemon's
sandbox; trusted plugin hooks keep the root privileges their contract requires.
The daemon uses `ProtectSystem=strict` with bounded writable trees; CPU sampling
needs the cpufreq write exception. No unit uses `StateDirectory=` on a shared
tree. Kernel modules, kernel logs and control groups are protected, and namespace
and SUID/SGID creation is restricted.

Rolling back means reinstalling the previous release; see
[the README's rollback steps](../../../README.md#rollback).
