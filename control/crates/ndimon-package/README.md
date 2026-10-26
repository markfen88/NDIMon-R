# ndimon-package

The Debian maintainer scripts of all four NDIMon-R packages, written as one Rust
program. `cargo xtask build-deb` copies the compiled `ndimon-package` binary into
each package as `preinst`, `postinst`, `prerm` and `postrm`, so dpkg executes an ELF
file directly and no shell, Python or Node interpreter takes part in an install,
upgrade or removal. The binary reads `DPKG_MAINTSCRIPT_NAME`,
`DPKG_MAINTSCRIPT_PACKAGE` and its first argument (the dpkg action), refuses to run
unless it is root, and rejects an unknown package name.

## Hooks

| Hook | Action | Does |
|---|---|---|
| `preinst` | `install`, `upgrade` | First writes and syncs a probe file in `/var/lib/ndimon` and `/etc` (the nearest existing parent on a first install), so a full disk, a read-only mount or a file where a directory belongs fails the hook while the old stack is still running; then stops every unit of the stack (including retired ones such as the watchdog, updater and the Node API) after a `daemon-reload`; disables retired units that still exist; moves manual units out of `/etc/systemd/system` (keeping a `.pre-p8` copy of each) so they no longer shadow the packaged ones |
| `postinst` | `configure`, **runtime package only** | The migration below |
| `prerm` | `remove`, `deconfigure` | Stops the stack |
| `postrm` | `remove`, `purge` | Removes only the dangling enablement links of the package's own units and reloads systemd |

Other dpkg actions (abort, failed-upgrade) do nothing, so recovery data is kept.
The migration runs when `ndimon-runtime` is configured, because dpkg configures it
after the three packages it depends on, so everything it needs is unpacked:

1. Create users, groups and memberships from `sysusers.d`, drop the earlier
   account-wide `ndimon-priv` grant (and the group when only service accounts were in
   it), remove the retired API's socket group, and create directories from
   `tmpfiles.d`.
2. Seed any missing default file into `/etc` (mode 0640); existing files are never
   overwritten.
3. Copy `/var/lib/ndimon-network` and the earlier network journals once to
   `*.pre-p8`, only when they hold data.
4. Run `ndimon-state-import` (builds the state document from the existing
   configuration and keeps `<file>.pre-control` copies) and the privd network boot
   job (imports earlier network journals). A failure here aborts the hook before
   the next step deletes anything. Content these refuse does not make them fail
   (see the rule below).
5. Refresh the udev rule for Rockchip RGA devices, then delete the files of earlier
   releases: sudoers rule, the privileged and network helper scripts, the standalone
   watchdog and updater, the Node API's unit, drop-ins, runtime directory and
   installed tree (`/opt/ndimon-r/api`, unless a source checkout lives there), and the
   `cap_net_bind_service` capability the 1.3.0 installer gave the Node binary. Plugin
   code and data are never touched.
6. Enable and start the privd and control sockets, `ndimon-net-boot`, the control
   service, the decoder and the finder (the last two without waiting, since the
   decoder needs a display), and write the package version from
   `/usr/share/ndimon/version` to `/etc/ndimon-firmware-version`.

### The rule for a migration step

The old stack is already stopped when step 1 starts, so **a step that cannot
migrate something must not leave the appliance down** (a Tessera 0.1.0 config
without a device name once stopped the import and left the appliance dark; a lock
file left by 1.3.0 did the same to the network migration).

* *Essential* steps (accounts and directories, the state import, the network boot
  job, enabling and starting the units) still stop the hook when they fail: there
  is nothing to start without them. They contain content refusals where the
  content is read. `ndimon-state-import` leaves a setting group whose legacy files
  it cannot import, or whose values do not validate, at its defaults and prints
  `state migration: NOT IMPORTED, left at its defaults: <group>: <reason>` (the
  originals stay as `<file>.pre-control`); a Tessera device without a name takes
  the host name. The network migration takes over the lock file 1.3.0 created and
  reports any other store it will not take over (`ndimon-net-boot` exits 0 and the
  journal says why; network changes stay locked until it is repaired).
* *Tidy-up* steps (old group memberships, default settings, recovery copies, the
  udev refresh, removal of retired files, stale unit links) are reported as
  `NOT MIGRATED, <what>: <error>`, the rest is configured and started, and the
  hook ends with the list.

## Console helpers

`ndimon-package console-acquire` and `console-release` unbind and rebind the kernel
framebuffer consoles and blank the framebuffer, so the decoder can take DRM master
and give the display back. `ndimon-r.service` runs them in `ExecStartPre` and
`ExecStopPost`.
