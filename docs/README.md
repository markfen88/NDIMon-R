# NDIMon-R documentation

The [main README](../README.md) describes `1.4.0`. The [changelog](../CHANGELOG.md)
holds the release notes, the breaking changes and the upgrade notes of this
release, and the earlier 1.3.0 notes. Experimental audio features retain their
opt-in and certification limits. Source publication does not update the live
appliance.

## Operator guides

| Task | Documentation |
|---|---|
| Install, first boot, UI, security, API and recovery | [README](../README.md) |
| Upgrade a 1.3.x appliance, what is imported and retired, what to do afterwards | [Upgrading from 1.3.x](../README.md#upgrading-from-13x) |
| Roll back to a previous release | [Rollback](../README.md#rollback) |
| Management, shared NDI and Tessera VLANs; DNS search domains; Keep/Revert; backend validation; handoff | [Network profiles and assignments](network-vlans.md) |
| Install/configure Tessera, name channels, define flows and choose audio routes | [Tessera network audio](plugins/tessera.md) |
| Write, package, install and update plugins; the trust boundary | [Plugin authoring and lifecycle](plugins.md) |
| Provision release keys, verify and publish signed releases, update a device | [Signed releases and updates](releases.md) |
| Audio graph requests, revisions, endpoint ranges and migration errors | [Per-output audio API](api/output-audio.md) |

NDIMon-R owns network settings when it hosts Tessera. The audio daemon and
runtime belong to the Tessera project; the host adapter, the UI and the pinned
bundle packaging live here. NDIMon-R 1.4.0 needs **Tessera 0.3.0 or later**. The
standalone Tessera network controls are separate from the plugin; moving a device
between the two takes an explicit
[ownership handoff](network-vlans.md#ownership-handoff). Runtime updates require
rebuilding, testing and replacing the plugin archive; they are not automatically
mirrored.

## Developer guides

| Area | Documentation |
|---|---|
| The Rust workspace: crates, `cargo xtask` tasks, build and check | [control/README.md](../control/README.md) |
| Front door: sessions, CSRF, Host allowlist, events, supervisor | [ndimon-control](../control/crates/ndimon-control/README.md) |
| State actor, domains, followers, backup, plugin host | [ndimon-state](../control/crates/ndimon-state/README.md) |
| Typed privileged daemon, wire protocol, jobs, capabilities | [ndimon-privd](../control/crates/ndimon-privd/README.md) |
| Network engine library, job entry points, handoff protocol | [ndimon-netengine](../control/crates/ndimon-netengine/README.md) |
| Release verification library | [ndimon-release](../control/crates/ndimon-release/README.md) |
| Generated-file renderers and domain ownership table | [ndimon-render](../control/crates/ndimon-render/README.md) |
| Legacy configuration import contract | [LEGACY-MAPPING.md](../control/crates/ndimon-types/LEGACY-MAPPING.md) |
| Browser TypeScript: store, typed client and build | [ui/README.md](../ui/README.md) |

## Known limits

Network audio remains experimental: a namespace or local PCM check does not
establish a physical switch, a Dante device, audible HDMI, automatic lip-sync,
hardware PTP or long-duration operation, and no network backend is recorded as
validated by default (see [Backend validation](network-vlans.md#backend-validation)).
Sustained clock stability and external hardware interoperability are not certified.

## Deferred work

Per-output receiver transport/HX/tally/network isolation, Bluetooth delay
calibration, HDMI ARC/eARC capture, and a separate plugin release and
compatibility pipeline remain deferred. Automatic vendor-controller subscriptions
are not implemented.
