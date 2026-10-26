# NDIMon-R changelog

## Unreleased

### NDI Discovery Server monitoring and control

- **A Discovery Server controller can route the outputs, and the route sticks.**
  When a controller (for example NDI Tools Discovery 6.2+) picks a source for an
  output, the SDK switches the receiver and NDIMon-R now saves that source as the
  output's source, the same way a pick on the Outputs page or the API does. Status,
  the web UI and reconnects follow it, it survives a sender dropout, a decoder
  restart and a reboot, and the output shows "set by Discovery Server". A
  controller disconnect clears the source. Whichever change comes last wins.
  1.4.0 had made routes observe-only, and its reconnect logic put the previous
  source back.
- **New NDI page switches**, *Allow Receiver Advertising* (on) and *Allow
  Receiver Control*. Control is off on new installs, as NDI recommends, and stays
  on after an upgrade (1.3.x behaviour). NDI control has no authentication.
- **Monitoring is what the SDK reports.** Each output registers as its own
  receiver input (named after the output alias) and the Discovery Server shows
  connection state, source, video and audio format, transport and tally.
  NDIMon-R no longer sends `<ndi_routing>` or `<ndi_device_status>` XML upstream;
  those went to the sender, never to the Discovery Server.
- **Changing the Discovery Server address restarts the decoder**, after a
  confirmation, like a transport change. The SDK reads the address only when the
  decoder starts, so routes by name could not be resolved after a change until
  the next restart.
- Fixes: the receiver now registers once a Discovery Server that was unreachable
  at start answers (it used to stay absent until a restart); a lost link no longer
  disconnects the receiver, which threw a route away; after a route the old
  source address is no longer reused on the next reconnect.

## 1.4.0

1.4.0 is the control-plane release. The Node.js REST API, the shell and Python
helpers, the installer scripts, the standalone watchdog and the sudo rule of 1.3.x
are replaced by a Rust control plane, and the whole configuration becomes one
versioned document. The decoder and the finder stay C++. This release promotes the
application from `1.3.0` across the C++ project metadata and the packages: the
packages write `1.4.0-1` to `/etc/ndimon-firmware-version`. A 1.3.x appliance
upgrades in place; read the upgrade notes below and
[Upgrading from 1.3.x](README.md#upgrading-from-13x) first. For current operation,
use the [README](README.md) and the [documentation index](docs/README.md).

### Highlights

- **One control plane.** `ndimon-control` (Rust) serves the web UI and the API on
  port 80 and owns every setting as one desired-state document with one revision.
  Every change from the UI, a script, a plugin or a restore is a typed command that
  is validated as a whole, planned (what restarts, what is interrupted), confirmed
  when disruptive, applied and saved; a failed apply rolls back. Open pages update
  themselves from one event stream, and a stale save is refused with a conflict
  instead of overwriting.
- **A typed privilege boundary.** `ndimon-privd` is the only root helper. It takes
  a closed set of typed requests from a single caller, runs long jobs as
  independent systemd units and replaces the sudo rule, `ndimon-priv`, the config
  writer, the CPU tuner and the network helpers.
- **A native network engine** for NetworkManager, Netplan, systemd-networkd and
  ifupdown, with journaled, recoverable changes, DNS search domains, a confirm
  policy, a readable page when the host's configuration cannot be adopted, and an
  explicit ownership handoff with a standalone Tessera install.
- **Out-of-process plugins.** Plugins (manifest v2) run as sandboxed processes with
  their state in the device document; the control plane runs no third-party code.
- **Signed updates.** Updates install only from releases signed with a key the
  device trusts. With a key, the device looks up the newest signed release itself
  and **Update now** installs it.
- **Native backup and restore**, a **TypeScript web UI** on the native `/api/v1`
  store, and a **supervisor** inside the control plane in place of the watchdog
  service.
- **Four Debian packages** built by `cargo xtask build-deb`, installed on the
  appliance by `cargo xtask install`. The repository holds no Python and no shell,
  and nothing on the device needs Node.js.

### Breaking changes and upgrade notes

1. **Back up first.** Export a backup, resolve any pending network change and keep
   console access. Install the four 1.4.0 packages, or run `cargo xtask install`
   from a 1.4.0 checkout. The 1.3.x web **Update now** button cannot perform this
   upgrade. The first start imports the 1.3.x settings (originals kept as
   `<file>.pre-control`), the password, the effective network configuration (private
   journals also kept as `*.pre-p8`) and Tessera's effective configuration, and then
   retires `ndimon-api`, the watchdog and update units, `install.sh`, `scripts/`,
   the sudoers rule and the Python engine. The old services are stopped while this
   runs, so what cannot be imported costs its setting group, not the device: it
   is left at its defaults, reported, and the original stays as
   `<file>.pre-control`. See [rollback](README.md#rollback).
2. **JavaScript plugins (manifest v1) no longer run.** They stay installed with
   their files and data, are reported as "needs an update to the out-of-process
   format", cannot be enabled, and their backup section reads "restorable after
   plugin support is available". Install a manifest v2 release of the same name or
   remove them. See [the plugin guide](docs/plugins.md).
3. **Tessera needs 0.3.0 or later.** Install the matching plugin archive after
   upgrading the core. This release pins the Tessera 0.3.0 runtime (`0.3.0-1`, source
   commit `4b747c1`) for Debian 13 amd64 and arm64 and for the experimental Ubuntu
   24.04 arm64; the 0.3.0 plugin archive carries one of them. The 0.2.x bundles
   cannot be installed any more (they lack the `tessera-bundle` installer), and a
   0.2.x runtime that is already installed runs without flow locks: NDIMon-R reports
   an ownership conflict.
4. **Node.js is no longer used.** The upgrade does not remove the `nodejs`/`npm`
   packages; remove them if nothing else needs them.
5. **Other host names need to be added.** The web UI answers only to the device's
   IP addresses, `localhost`, its hostname, `hostname.local` and
   `hostname.<search domain>`. Any other name (a DNS alias, a proxy name) gets HTTP
   421 `misdirected_host`. Add it under **Settings → Security → Additional host
   names** (or `device.web_allowed_hosts`) after opening the UI by IP address.
6. **Network changes work on all four backends, as in 1.3.x.** The engine changes
   the network only for backends listed in the root-owned
   `/etc/ndimon-network-certified-backends.json`. All four (`networkd`, `netplan`,
   `networkmanager`, `ifupdown`) passed real-tool acceptance, so the `ndimon-privd`
   package installs the file listing them, on a fresh install and on the upgrade of
   a 1.3.x appliance. It is a conffile: edit it to withdraw a backend you have not
   validated on your hardware (the page then shows the live configuration
   read-only, previews work, and every change and Tessera handoff is refused with
   `backend_not_certified`); your edit survives upgrades. A missing, unreadable or
   malformed file allows no backend and the `ndimon-privd` journal says why.
7. **Saving an output's audio selects engine v2** in the same change. While v1 is
   running the device shows that the decoder restarts and asks first. In 1.3.x
   saving routes left the engine alone, which left network audio silent after a
   save (field incident); routes and engine are now one change. **Return to audio
   engine v1…** still restores the legacy path and keeps the saved selections.
8. **Updates are signed-only, and no key ships.** The checkout updater is gone.
   Without a key in `/etc/ndimon/trusted-release-keys` the device refuses to
   install updates and the page says "manual updates only"; update by hand. With a
   key the device looks up the newest release in the release source it installs
   from, and offers it only when it is newer and its checksum list is signed by a
   trusted key and complete for this architecture: **Update now** installs the
   `latest_version` the version query then reports (checked at most hourly; a
   failed lookup says why and offers nothing). You can still start an update with
   an explicit version (`POST /v1/System/update` or the `install_update`
   command). See [releases](docs/releases.md).
9. **Sessions and scripts.** Sessions end at the upgrade. The session cookie is
   `HttpOnly; SameSite=Strict` with an eight-hour idle timeout (it was `Lax` for
   seven days). Every change made with the cookie must send `X-CSRF-Token` (from
   `POST /api/v1/session` or the login reply); `Authorization: Bearer` with the
   token from `POST /api/login` needs none. Password hashes stay valid and are
   replaced by Argon2id at the next login.
10. **Generated files are not editable.** The `/etc/ndimon-*.json` family, Tessera's
    `config.d` files and the ALSA PCM files are rendered from the document by
    `ndimon-privd`; a hand edit is reported as drift and replaced at the next write.
    Use the UI or the API.
11. **Service layout.** `ndimon-finder` runs as its own `ndimon-finder` user;
    `ndimon-control` as `ndimon-control`; there is no `ndimon-api`, `ndimon-watchdog`
    or `ndimon-update` unit. Port 80 comes from systemd socket activation
    (`ndimon-control.socket`). The decoder and finder keep separate SDK homes under
    `/run/ndimon/ndi/`.

### Security changes

- **Host allowlist.** Every protected request must name the device in its `Host`
  header (interface addresses, hostname, `hostname.local`, loopback, resolver
  search domains, or the configured additional names), against DNS rebinding. No
  DNS answer or forwarding header is trusted. `Origin` must match `Host`, and
  cookie-authenticated changes need a CSRF token.
- **Login limiter.** Wrong passwords (and wrong current passwords) are counted per
  source (IPv4 address or IPv6 /64); from the fifth failure a source gets 429 with
  a `Retry-After` that doubles from 1 s to 30 s. A global failure count only adds
  a short delay, so one noisy client can no longer lock out everybody. Sessions
  are random 256-bit tokens, held in memory (at most 64).
- **Privileged daemon.** No sudo and no shell helper remain. `ndimon-privd` takes
  typed requests only, with no path, argv or shell text from a caller; it
  identifies its caller by the socket that accepted the connection on every kernel
  (and by cgroup on Linux 6.5+), keeps a minimal capability set and logs every
  request kind and outcome. The finder has its own account, no privileged group
  and no access to the state or privd. The privileged runtime parent is
  `root:root 0755`.
- **Token endpoints.** The unauthenticated network confirmation routes read their
  body with a deadline and a size limit and cannot stall each other.
- **Plugins are separate processes** in sandboxed transient units (dynamic user,
  no capabilities, scoped paths and groups, no access to privd or the control
  sockets). Archive extraction is bounded in size, entry count and metadata, which
  closes a compressed-metadata allocation attack.
- **Updates are signed-only.** A checksum list must carry a valid minisign signature
  from a trusted key whose comment names the product and version, every package
  must match it, and packages are verified before any is installed.
- Node.js and its dependency tree (five npm advisories) are no longer on the device.

### Notable fixes

- **Ownership of `/var/lib/ndimon`** is declared once, by `tmpfiles.d`. The finder
  unit no longer declares `StateDirectory=ndimon`, which re-owned the shared tree
  on every start (VLANs could not be read, plugin installs failed).
- **Static VLAN got DHCP.** Network files that sort before the distribution's own
  (for example Armbian's `Name=e*`) are written for every profile network, with the
  route metric the backend would have used.
- **Keep changes** is never disabled, can be pressed from any address that holds the
  token (default policy), and only a wrong token counts toward the rate limit.
- **Audio.** The master wait is two periods, rounded up (HDMI audio no longer
  garbled), and Tessera sinks target three periods of fill (no dropout every few
  seconds).
- **One flow writer.** Tessera flows have a single writer; controller flow writes are
  locked (Tessera 0.3.0 or later) and reported as drift when they happen.
- **Live pages.** Open pages reflect changes made elsewhere; stale saves get a
  conflict and offer a reload.
- **Restarts.** Restarting `ndimon-net-boot` no longer leaves the stack down, a
  state transaction is finished or rolled back before the control plane exits, and a
  failed boot job is retried.
- **Stock Armbian networking is manageable.** Armbian's own
  `/etc/netplan/10-dhcp-all-interfaces.yaml` (`ipv6-privacy`, `dhcp6`) made the
  Network page read-only on the ROCK 5B. IPv6 privacy-extension settings (Netplan
  `ipv6-privacy`, networkd `IPv6PrivacyExtensions=`, NetworkManager
  `ipv6.ip6-privacy`, ifupdown `privext`) are not address intent: they are adopted,
  left out of the managed configuration and restored exactly by Revert. A bare
  `dhcp6: true` (the distribution default) is adopted the same way; `dhcp6-overrides`,
  `gateway6`, `accept-ra: true` and IPv6 addresses or DNS are still refused.
- **Raspberry Pi OS networking.** A Netplan-generated NetworkManager profile
  (`netplan-eth0`, route metric left at `-1`, no interface binding) is adopted: `-1`
  is NetworkManager's Ethernet default of 100, checked against the installed route.
  The binding is not written to a profile Netplan owns, the addressing is read back
  after every write, and Revert restores the profile's files byte for byte. Errors
  name the connection and the property.
- **Backup of a beta-era device restores.** The 1.3.0 beta wrote `"ScaleMode":""`
  for a second output; the export now says `letterbox`, so the device's own backup
  passes its own inspect.
- **Tessera keeps its clock role.** The import takes the clock role and priority from
  the running daemon (a device that never set them was `follower` and became the PTP
  leader of the audio network); without an answer the files decide as before.
- **A refused plugin install can no longer block the plugin lifecycle.** privd
  looked for a job's result file, then asked systemd whether the unit was still
  running; a fast refusal (Tessera 0.2.1's bundle has no `tessera-bundle`) that ended
  between the two read as a job that had vanished, answered HTTP 500 "operator
  investigation required" about once in five tries, left a phantom enabled plugin and
  made every plugin operation answer 409 `busy`. privd now looks again once the unit
  is inactive, control calls a job lost only after three polls, the documented
  `POST /v1/plugins/jobs/<job>/cancel` settles an investigation marker (and no
  longer fails for a unit that has ended), and restarting the control plane settles
  one it finds: the registry returns to what it was before the job.
- **A refused Tessera bundle has no side effect.** The runtime bundle is checked when
  the archive is inspected, before the install is accepted. Previously, where
  `tesseractl` exists, the accepted install had already rewritten
  `/etc/tessera/config.d` and created `/etc/alsa/conf.d/60-ndimon-tessera*` when the
  refusal came.
- **The Tessera plugin installs on a stock Debian 13 or Ubuntu device.** privd
  unpacked the runtime bundle below `/run` and ran `tessera-bundle` from there, but
  `/run` is mounted `noexec` on both, so the install failed with "Permission denied"
  and marked the plugin `broken`. The bundle is now unpacked under
  `/var/lib/ndimon-privd/bundles` (root-only, created by `tmpfiles.d`, listed in the
  unit's `ReadWritePaths`); the checksum, lock, layout and ownership checks are
  unchanged. Every unpacked bundle (41 MB) was also left behind and never removed,
  and after a few installs and uninstalls the 384 MB `/run` of a ROCK 5B was full:
  every plugin job then failed with "No space left on device" or could not leave a
  result (an uninstall that had run answered HTTP 400 "job ended without a durable
  result"). Each unpacked bundle is now removed when its job ends.
  A normal (non-purge) uninstall leaves the plugin's packages removed but configured
  (`rc`) and the dependencies the install added (`polkitd`) installed, as
  `tessera-bundle` documents; "remove and delete data" purges.
- **Keep no longer loses to the page's own status poll.** Keep, Revert, Accept
  current network and the preview wait up to three seconds for a concurrent network
  request instead of answering 409 `network_operation`; the 60-second window is
  unchanged.
- **Quieter log, honest accept.** A network change waiting for its Keep logs
  "followed state import failed" once instead of every two seconds. Accepting the
  current network names a file it could not put back (`warnings`, `unrestored`, and
  the page's notice) instead of answering plain ok.
- **Version, memory and diagnostics.** `/v1/System/version` reports the commit and
  date the installed package was built from, not the previous installer's. The
  memory gauge counts `MemAvailable` (the page cache is not "used"). Silent HDMI
  audio underruns are now counted and logged, and a display that stops committing
  frames after a mode change logs its DRM state; a mode change whose commit failed is
  reported as failed and retried.
- **Building on Ubuntu noble.** `zlib1g-dev` is optional (a mirror that lags a
  security update no longer stops the dependency install), and the FFmpeg 7 runtime
  libraries the NDI SDK's HX decoder loads come from a pinned, temporary plucky
  source that cannot replace noble's own packages, above all the `-dev` packages.
- Fixes found by installing the packages on virtual machines (Debian 13, Ubuntu
  26.04): the decoder and finder no longer crash-loop on a fresh install
  (`libavahi-client3`), a stock cloud-init Netplan is readable and adoptable, the
  control plane no longer probes the network every second, Tessera installs report
  their refusal and leave nothing broken, and package hooks no longer print errors.
- A second round on the virtual machines: **upgrading a 1.3.x device that had kept a
  network change no longer fails** (the lock file 1.3.0 left in
  `/var/lib/ndimon/net/pending`, mode 0644, is taken over; any other lock is
  refused naming the file, and a refused legacy network store no longer keeps the
  device down: the boot job reports it, the network page stays locked until it is
  repaired and the next boot retries). **The first network change of a device that
  never kept one is accepted** (it was refused with `fixed_port_backend`). **A write
  no longer waits a second** for the finder. A writable `/usr/local/lib` no longer
  makes the network page fail, and an unreadable network directory is shown on the
  page with its path and reason. Reinstalling the packages without a reboot no
  longer leaves the decoder refusing writes (`follower revision is ahead`). The
  finder logs as it runs, the upgrade leaves no dangling unit links or empty
  `/opt/ndimon-r/scripts`, and a network change no longer announces a Tessera
  restart on a device without Tessera.
- **A Tessera 0.1.0 device with no name no longer fails the upgrade** (found on a
  ROCK 5B: the import stopped with "Tessera name must be imported" after the old
  services were already stopped, and the device stayed dark until a manual edit).
  The device takes the host name, which is Tessera's own default. More generally,
  a legacy setting group that cannot be imported is left at its defaults and
  reported instead of stopping the upgrade (the package output ends with `state
  migration: NOT IMPORTED, left at its defaults: <group>: <reason>`, and the
  original files stay as `<file>.pre-control`), and the package configuration steps
  that only tidy up are reported at the end instead of stopping it.
- **A network recovery that cannot finish no longer silences the appliance** (found
  by Tessera's run on a Raspberry Pi, where the same NetworkManager build and its
  Netplan write-back left the audio stack stopped for minutes). The media services
  are started again and are not stopped by each retry; the Network page says why
  the previous settings were not restored and offers **Accept current network**
  (`POST /v1/Network/recovery/accept`) after showing the live addresses and any
  foreign configuration. A recovery that fails at boot is reported by
  `ndimon-net-boot` instead of failing it, so the control plane, finder and
  decoder still start. While such a recovery is unfinished, Keep and Revert answer
  `409` (`apply_failed`, step `recover`, with the reason) instead of 500; that is
  not a token failure and does not count toward the rate limit. For a NetworkManager profile that Netplan owns, the
  restore takes the profile's files after one reload (NetworkManager re-saves
  them), waits for NetworkManager to settle after `nmcli connection up`, and
  compares the files again ignoring only NetworkManager's own timestamp and
  `ipv6.ip6-privacy: "-1"` lines.
- **A Tessera configuration without `tesseractl`** (the 0.1.0 plugin never shipped
  it) no longer crash-loops the control plane: Tessera's files are left alone, the
  rest of the device stays configurable, and the Tessera page reports the daemon
  offline. A refused plugin install answers `400` instead of `500`. `ndimon-net-boot`
  no longer logs `Unit tesserad.service not found` at every boot of a device without
  Tessera, the decoder logs the periodic `reload_config` only when it changed the
  configuration, a plugin runtime is no longer refused for a few seconds after a
  stack restart, and the upgrade checks that the state directory and `/etc` can be
  written before it stops the old stack (a failing check leaves it running).
- **The display no longer freezes after a change of output resolution** (found on
  a ROCK 5B: when a sender restarted with another size, for example 1080p to 720p,
  the picture froze until the watchdog reconnected a minute later, in four of four
  tries). A new connection decided its stream type from a flag left over from the
  previous one, so a first frame that arrived before the receiver noticed the
  connection skipped the FrameSync setup; the mode change for that frame then held
  the receive thread long enough for queued audio to starve the video for good.
  Stream detection now starts with every connection, and a waiting video frame is
  taken before audio. The 1.3.0 beta has the same race (its receive code is
  identical) but its mode change is about 0.1 s shorter and it did not freeze in
  18 tries. The receiver logs `No video frame 3 s after the connection`
  when a source sends audio only. An 18-change run on the board (twelve 1080p/720p
  output changes, 2160p/1080p and rate changes) no longer freezes.
- **The decoder and finder wait for the network chosen for NDI at boot** (found on
  a ROCK 5B after the first network Keep on a DHCP port: both services exited and
  were restarted twice at every boot, which showed as failed units in the journal
  of the boot an operator watches). They wait up to a minute for the interface to
  get its address instead of failing; the engine still keeps managed links out of
  `network-online.target` on purpose (an offline media lease must not hold the boot
  hostage), and a network that never comes up is still refused rather than
  replaced by automatic selection.
- **Restore review on a device with no committed network.** A 1.3.x backup's network
  section no longer reads "cannot be applied on this system (status unavailable)"
  on a device that has not kept a network change yet: it is compared with the
  profile the Network page proposes, and a restore starts from it. The Plugins row
  of the review compares each plugin's section with what the device would export
  and no longer calls the device's own backup "changed".

### Known limits in this release

- The packages list all four network backends as validated (see the upgrade notes)
  because the engine passed its isolated lifecycle scenarios with the real tools on
  each; no hardware or switch is certified.
- The NetworkManager backend on Raspberry Pi OS (NetworkManager writing its profile
  back to Netplan) is covered by tests that simulate that write-back and by a
  Raspberry Pi-shaped profile in the isolated lab. It has not yet run on a real
  Raspberry Pi running NDIMon-R.
- One fault seen on the ROCK 5B has diagnostics but no known cause: the HDMI audio
  output re-prepares its PCM repeatedly (silent underruns, now counted and logged).
- The automatic resolution change still runs on the receive thread and takes about
  0.3 s on the ROCK 5B (0.2 s on 1.3.0, cause not found); the picture and audio
  catch up through FrameSync, but nothing is received meanwhile.
- No project release key is provisioned and no live GitHub release lookup or
  download has been verified yet; the lookup and the install are tested against a
  fake release source only.
- Network audio (Tessera, engine v2) is still experimental: sustained clock
  stability, audible HDMI, physical Dante interoperability and hardware PTP
  through VLANs are not certified.
- Per-output NDI transport/HX/tally/network assignment and independent video delay
  stay deferred with receiver-process work.

### Details

#### DNS search domains and a readable Network page on unadoptable hosts

- Networks carry optional DNS search domains (`dns_search`, management only, at
  most six host names). All four backends write and adopt them on the management
  definition: Netplan `nameservers.search`, NetworkManager `ipv4.dns-search` (also
  restored by Revert), networkd `Domains=`, ifupdown `dns-search`. Lease-provided
  domains are never written; `UseDomains=`/`use-domains` and routing-only `~`
  domains on adopted management are refused as unrepresentable. Documents without
  search domains, backups and Tessera handoff files are unchanged. Stock cloud-init
  Netplan (`match`, `set-name`, `nameservers.search`) now adopts.
- Settings → Network edits search domains for the management network (DHCP or
  static). The device also accepts web requests addressed as
  `hostname.<search domain>`.
- When the device's network configuration cannot be adopted, `/v1/Network/status`
  and `/profiles` stay readable: they report the refusal (reason and the key and
  file, never the value) and the live address, and the page shows them read-only
  in a notice. Changes stay refused with the same reason.

#### Control plane

- A serialized state actor owns one desired-state document (`state.json`, with the
  previous revision kept as `state.prev.json`) covering device, outputs, NDI, audio,
  network, Tessera, display, presets and plugins. Commands carry optional
  compare-and-set revisions and impact tokens; confirmation planning, recoverable
  persistence (a pending marker is committed or rolled back at the next start), and
  per-domain apply, rollback and replay after service restarts are part of the
  actor's transaction. Sources the actor does not own are observed and reported as
  drift.
- Files that other programs read are generated from the document through
  `ndimon-privd` with a shared ownership/path table and a separate SHA-256 manifest
  (`rendered.json`); shared device files keep every byte the actor does not own.
- `ndimon-control` serves everything natively on port 80 (IPv4 and IPv6 socket
  activation): the UI, `/api/v1`, the legacy `/v1` compatibility routes, plugin
  assets and the event streams. A path no router claims is a local 404. It runs as
  `ndimon-control` with `NoNewPrivileges=yes` and no filesystem write exceptions
  beyond its state, import, backup and runtime directories.
- Added `GET /api/v1/session` (sign-in status and CSRF token for a reload) and
  `POST /api/v1/session/password`. The web UI signs in, changes the password, and
  reads and writes Settings, System, Backup, the pending network change and Plugins
  through the native store and `/api/v1/commands`; the flattened legacy status
  snapshot and the `/api/events` stream are no longer used by any page. Legacy
  session and password routes keep their shapes.
- Added typed live-status events with decoder, finder, Tessera, network and services
  sources and opaque JSON data (TypeScript `unknown`), delivered at
  `/api/v1/events` with `resync` frames and 15-second keep-alives.
- Front-door sessions expire after eight idle hours and require a CSRF token for
  cookie-authenticated changes. Legacy scrypt passwords upgrade to Argon2id after
  login; backups preserve both formats for restore and rollback.
- A supervisor inside the control plane replaces the watchdog service. It recovers
  lost sources with backoff (5, 10, 20, then 30 s), follows `watchdog_mode`
  (`active`, `passive`, `disabled`) and keeps `/api/health` and `/api/watchdog-stats`.
  systemd owns process liveness. The control plane is `Type=notify` with a
  `WatchdogSec=30` heartbeat, and its stop finishes the state transaction in flight.
- Native backup and restore: capture, review, apply and undo are coordinated in
  Rust. A restore is one confirmed change of the native settings, with network
  staged last and plugin state after. Schema-1 backups of 1.3.x restore unchanged;
  the device section also carries the additional host names.
- The web UI's **Update now** button asks once, then installs the newest signed
  release the device reports as `latest_version` (it says so when there is none and
  sends nothing). The device finds that release itself when a release key is
  configured. Each service's restart dialog says what that restart does.

#### Privileged daemon and packaging

- `ndimon-privd` is socket-activated, root and exits when idle. Its request set is
  closed and typed; plugin lifecycle, Tessera artifact generation, CPU sampling,
  hostname, NTP, the reboot timer, services, network jobs and updates are requests.
  Independent system-manager jobs survive the disconnection of a caller. It has one
  caller: `ndimon-control`, through `/run/ndimon-privd/control.sock`.
- Four Debian packages: `ndimon-control`, `ndimon-privd`, `ndimon-runtime` and
  `ndimon-ui`, built by `cargo xtask build-deb` with native Rust maintainer hooks
  (no debhelper-generated scripts). Users and groups come from `sysusers.d`,
  directories from `tmpfiles.d`. The packages depend on `systemd (>= 244)`. The
  runtime package is built per distribution release.
- `cargo xtask install` builds the packages on the appliance and installs them with
  apt. It never installs Node or npm; the committed UI bundle ships, and the pinned
  Bootstrap files are fetched from the registry tarballs the lockfile records,
  checked against their sha512 `integrity` and unpacked by the installer.
- Added minisign-signed release manifests, build-host release, verification and
  key-generation tooling, and tag-triggered artifact assembly in CI. The trust list
  starts empty; the deployment provisions the first public key.
- Added verified prebuilt-package update jobs with bounded HTTPS downloads,
  progress, package identity checks and force-only downgrades.
- The decoder and the finder write and log their SDK configuration only when its
  content changes.

#### Node.js removed from the device

- Deleted the Node.js REST API (server, routes and modules), the `ndimon-api` unit,
  the `ndimon-legacy-api` package, the front-door proxy and the private sockets
  between them. `api/` keeps only the pinned Bootstrap manifest that the build copies
  into the packaged UI. No package depends on or ships Node.
- Manifest-v1 JavaScript plugins stay installed with their files and data but cannot
  run (see the upgrade notes).
- Upgrades stop, disable and remove `ndimon-api`, its sockets, drop-ins and
  installed tree, clear the port-80 capability that the 1.3.x installer gave the
  `node` binary, and drop the `ndimon` user from the old privileged-socket group,
  without touching plugin code or data.
- `ndimon-privd` has one caller, `ndimon-control` (its own socket and group); upgrades
  retire the former API socket and group.
- The repository holds no Python and no shell. `cargo xtask check-no-scripts` has no
  allowlist and also catches extensionless Python.
- A quick close of the restore dialog while it is still animating in no longer leaves
  it open.

#### Plugins

- Manifest v2 plugins run as separate processes in sandboxed transient units with a
  framed socket to the control plane, a bounded JSON state schema, typed commands,
  consumed events, a static UI and exact privileged command names. Plugin state is
  part of the document, so it has revisions and backup.
- Plugin lifecycle (install, enable, disable, update, remove, purge, force) runs as
  durable jobs: the actor records the job, the supervisor follows it, and a restart
  of the control plane resumes it. Pending enables are consumed once. Install replies
  of a manifest-v1 plugin say that nothing will enable it.
- Installing the Tessera plugin: privd verifies the runtime bundle (layout, checksum,
  lock, platform) on the staged tree before anything on the device changes, so a
  refused bundle leaves no plugin files and no `broken` entry and the message
  reaches the caller. `POST /v1/plugins/tessera/{enable,disable,update,uninstall}`
  reach the plugin lifecycle. The packaged Tessera fragment is listed only for a
  device that has accepted Tessera.
- Tessera is part of the device document: its device settings, labels, clock, flows
  and the locks that pin them are rendered into `config.d` and the ALSA PCM files,
  and the daemon is reloaded only when a file differs or it is another process than
  the one last reloaded (and once after the control plane starts).
- Typed Tessera client over `control.sock` with the tested wire contract, instead of
  a JavaScript plugin.

#### C++ decoder and finder

- Both accept revisioned `apply_config` snapshots of whole document domains over
  their sockets (the decoder's existing one, and a new `/run/ndimon/finder.sock`
  for the finder). The control plane replays the accepted configuration after
  either restarts.
- Decoder and finder use private NDI SDK directories under
  `/run/ndimon/ndi/<service>/`, written before and after SDK initialization.
  Runtime status, discovery catalogs and triggers moved to `/run/ndimon`, with
  compatible links.
- The audio engine waits up to two periods, rounded up, for the master sink. The
  previous truncated wait (512 frames = 10.67 ms waited 10 ms) timed out before most
  periods completed, so the master flipped between the HDMI sink and the software
  timer about once a second and HDMI audio was garbled.
- Drift-compensated ALSA sinks target three periods of fill instead of two. The
  Tessera ALSA plugin reports `delay()` as queued frames plus a lead of about one
  period; with two periods only about half a period was really queued, the engine
  writes one period at a time, and the queue ran dry between writes (an xrun and an
  audible gap every 3–4 s, drift loop pinned at 3000 ppm).

#### Network engine

- A transactional engine with networkd, Netplan, NetworkManager and ifupdown
  adapters: journaled apply, a recovery timer armed before activation, a separate
  bounded apply job, a 60-second Keep window, recovery at expiry, failure and boot,
  migration of the shipped Python journals and VLAN state, and explicit Tessera
  ownership handoff. Mutation is enabled per backend by the root-owned certification
  file (see the upgrade notes).
- Real backend lifecycle, wildcard/metric, storage-failure, SIGKILL and reboot checks
  run in disposable containers. Live checks fixed stock
  networkd selector handling, local VLAN parent discovery, generated-file scan races
  and boot recovery waiting on its own media-service ordering dependency. Pending
  NetworkManager VLAN removals retain ownership until commit-time pruning;
  asynchronous link deletion succeeds only after a fresh absence check. Concurrent
  atomic journal reads reopen unlinked inodes without relaxing ownership or
  single-link checks.
- **Keep changes** is never disabled while the confirmation dialog is open. The
  browser no longer waits for the "applied" phase or compares the page address with
  the new management address, and the engine no longer requires the confirmation
  request to arrive on that address (policy `token_only`; `require_management_address`
  restores the check). If a confirmation cannot succeed, the reason is shown in the
  dialog. The confirmation token remains the credential and the 60-second revert timer
  is unchanged.
- The confirmation rate limit (5 failures per minute) counts only a wrong token.
  Pressing **Keep changes** with nothing pending, while the change is still being
  applied, or after it has expired no longer counts. The reasons are reported
  separately ("no network change is pending", "confirmation token does not match",
  "the network change is still being applied"), and a still-applying change returns
  HTTP 409 with a "try again in a moment" message.
- Network profiles on Netplan hosts also write `00-ndimon-<iface>.network` for each
  profile network. Armbian's stock `Name=e*` Netplan file sorts before Netplan's own
  per-interface files and also matches VLAN interfaces, so a static VLAN received DHCP
  instead of its address, the apply failed with "a configured network did not obtain
  an IPv4 address" and was reverted. The pinned files are journaled and removed on
  rollback.
- Restarting `ndimon-net-boot` no longer leaves the stack down. The control plane
  finishes (commits or rolls back) the state transaction in flight before it exits,
  within its stop timeout, and refuses new writes meanwhile. The reader privd uses for
  network jobs rolls back the pending marker of a writer that died exactly as the
  control plane does when it starts, and waits only for a live writer. A leftover
  temporary state file of a dead writer is removed when the state is loaded.
- `ndimon-net-boot.service` retries a failed boot job (`Restart=on-failure`) and
  queues control, finder, decoder and `tesserad` again whenever it succeeds. A failed
  dependency used to leave them in "Dependency failed" for good.
- The control plane no longer probes the network every second. Guards, replay and the
  observer use a status that does not run first-adoption discovery, kept for 30 s and
  dropped when a network command may have changed it. Replay sends hostname, NTP and
  reboot schedule to privd only when they differ from what the process last applied,
  and privd renders only the artifacts whose bytes, owner or mode changed. A
  state-changing API write on a test guest fell from about 480 ms to about 110 ms.

#### Packages, hooks and install

- `ndimon-runtime` depends on `libavahi-client3` and `libavahi-common3`, which
  `libndi` needs: a fresh install no longer crash-loops the decoder and finder.
- Package hooks: no red "Failed to disable unit" lines on install and upgrade (only
  units that exist are disabled); `.pre-p8` copies only of data that existed before
  the install; no "unit file changed on disk" warnings from the preinst; the bytecode
  of the old helpers is removed with their tree; `apt remove` and `purge` delete the
  dangling enablement links of the package's units.
- `ndimon-finder.service` no longer declares `StateDirectory=ndimon`. On every finder
  start systemd re-owned all of `/var/lib/ndimon` to `ndimon:ndimon` (mode 0755),
  undoing the installer's `root:ndimon 3770` parent and root-only `net/pending`,
  `net/vlans` and `plugins`. The network helper then reported "unsafe VLAN index
  directory ownership or permissions" and plugin installs failed with "unsafe plugin
  state directory". Re-running the installer restores the intended ownership.
- The README describes the login limiter as a sliding window with a growing
  `Retry-After`, and the SDK homes as private (`/run/ndimon/ndi/{decoder,finder}`).

## 1.3.0 — 2026-10-20

### Plugins and network audio

- Install trusted plugin archives through Plugins, or update them from a
  configured feed. Enable, disable and remove plugins without replacing the
  decoder core. Plugins can contribute settings and backup extensions.
- Added the optional Tessera network-audio plugin with device/channel names,
  encoding and latency, RX/TX counts, clock controls, static flows, service
  controls and diagnostics. Install the archive matching your OS/architecture.
- Runtime updates are delivered in plugin bundles. Updating NDIMon core does
  not automatically replace an installed Tessera runtime.

### Per-output audio

- Added explicit opt-in audio engine v2 at 48 kHz; v1 remains the default.
- Each output can select multiple NDI/Tessera inputs and HDMI/Tessera
  destinations, allowing the same mix to play through HDMI and network audio.
- Inputs have gain and delay; destinations have independent delay. Each delay
  accepts 0–5000 ms, with at most 5000 ms combined per active route. Disabled
  outputs retain their saved choices.
- Shared destinations mix different sources through one physical writer.
  Duplicate source-to-destination contributions and overlapping writers are
  refused. Revision checks prevent one browser from overwriting newer changes.
- Saved routes do not switch engines. Selecting v2 or returning to v1 requires
  confirmation and restarts the decoder. Endpoint recovery preserves saved intent.

### Management and media networks

- Added named untagged/tagged networks on one wired port, with DHCP/static IPv4
  and independent management, shared NDI and Tessera role assignments.
- VLAN IDs are 1–4094, with up to eight tagged networks. Configure the switch
  trunk separately. The appliance lists its configuration, not all switch VLANs.
- Network settings shows read-only usage links. Assigned networks need a
  replacement before removal. Only management supplies a gateway and DNS.
- Changes stay in a draft until preview/apply. Once activation is ready, click
  Keep changes through the new management address within 60 seconds. Revert,
  expiry, failure or an unconfirmed reboot restores the previous settings.
- Added management/media traffic isolation and locked audio/NDI interface
  bindings. Backups restore networks and assignments in one protected transaction.

### Interface and receiver improvements

- Refreshed navigation, mobile layout, live output status and unsaved-edit feedback.
- Current Connections, Source Presets and full-width output cards collapse
  independently. Each output's blue Advanced control starts collapsed.
- Added Inter splash text, gradient/flat backgrounds, status pills, logo
  placement, OSD styling and live preview controls.
- Corrected shared SDK configuration and transport selection. RUDP is the
  default; transport changes restart the decoder so the SDK rereads settings.
- Improved queued-frame ownership, returning-sender endpoint lookup, bounded
  source retries, settings persistence, discovery and receive/drop reporting.
- Corrected active decode reporting, packed-YUV colours and rotation support.
  The Standard SDK software-decodes HX; allocating an unused hardware decoder
  does not enable hardware HX decoding.

### Known limitations

- Audio v2 and the current Ubuntu 24.04 arm64 Tessera bundle remain experimental.
  Sustained clock stability, external Dante hardware interoperability and
  hardware PTP support are not certified.
- Delays are manual. Automatic HDMI/Dante lip-sync, independent per-output video
  delay and HDMI ARC/eARC capture are unavailable.
- NDI transport, HX preference, tally and NDI network assignment remain global.
  Bluetooth routing/calibration and automatic vendor-controller subscriptions
  are not implemented.
- Standard SDK HX decoding remains on the CPU. Quarter-turn software rotation,
  HDR conversion, IPv6/Wi-Fi setup and multi-port network management are unavailable.

### Upgrade notes

1. Export a backup before updating. Existing appliance settings are preserved.
2. Network profiles require nftables; run the updated dependency setup when it
   is missing. An update using --no-deps does not install missing packages.
3. Upgrade plugins separately with matching OS/architecture bundles. Installing
   or viewing a plugin does not activate experimental audio v2.
4. Configure switch tagging and access to the new management network before
   applying a change, then explicitly Keep it when ready.
5. When NDIMon hosts Tessera, manage networking in NDIMon. Source publication
   does not update or restart an installed appliance.

## 1.2 beta

Included backup/restore, primary wired DHCP/static addressing with recovery,
scheduled reboot, NTP, authentication and source presets.
