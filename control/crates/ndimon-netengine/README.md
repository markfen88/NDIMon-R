# ndimon-netengine

The privileged network engine of NDIMon-R: journaled, recoverable network
transactions on the primary wired port. The library never starts a server,
accepts an executable path or calls a shell; every external program is a fixed
`Tool` with explicit argv and a deadline. `ndimon-privd` hosts it (socket
requests and the `network-job` entry points) and the state actor supplies the one
commit authority. Operator behaviour is described in
[network profiles and VLANs](../../../docs/network-vlans.md).

## What it manages

The document is `ndimon_types::NetworkProfiles`: one physical wired interface,
named networks (untagged, or a VLAN ID 1–4094; at most eight tagged), the roles
`management`, `ndi` and `audio` (the Tessera role) assigned to networks, the
detected backend and a confirm policy. Only the management network carries a
gateway, DNS servers and DNS search domains. Independent VLAN or base mutations
do not exist; a change is always a complete document (`stage`).

Four backends are implemented. The backend is detected when the host is first
adopted and then stored with the document.

| Backend | Adopts | Owns and writes |
|---|---|---|
| systemd-networkd | The effective `.network` of the port from `/usr/lib`, `/usr/local/lib`, `/run` and `/etc` `systemd/network`, including drop-ins | `/etc/systemd/network/00-ndimon-<interface>.network` and `.netdev` files with exact interface matches, sorted before any distribution file |
| Netplan (networkd renderer) | The merged Ethernet definition (match, MAC and rename are followed); a renderer other than networkd is refused (`unsupported_renderer`) | `/etc/netplan/90-ndimon.yaml`, plus the same `00-ndimon-*` networkd pins, so a distribution `Name=e*` file cannot win |
| NetworkManager | The base connection by UUID; its automatic route metric (`-1`) is refused | `ndimon-vlan-<interface>` connections. Removed VLAN connections stay until commit and are pruned afterwards |
| ifupdown | The port's `inet` stanza (only `address`, `gateway`, `dns-nameservers`, `dns-search`, `metric` and `vlan-raw-device` are representable) | `/etc/network/interfaces.d/ndimon-<interface>` and `ndimon-vlans`, which `/etc/network/interfaces` must include directly with `source` or `source-directory` (`ifupdown_include_missing` otherwise). ISC dhclient is required (`unsupported_dhcp_client` if it is absent or another client is installed) and DHCP suppression on media networks uses a generated dhclient enter hook |

A configuration the document cannot express is refused, never trimmed or
guessed: mixed DHCP and static addressing, several gateways or static addresses,
custom routes, hooks, policy routing, IPv6 settings, a foreign VLAN, an unknown
backend, a link that is not a plain Ethernet port. The refusal code is stable and
names the key and file but never the value
(`unrepresentable_configuration: nameservers.foo in network.ethernets.end0
(/etc/netplan/50-cloud-init.yaml)`). The managed files carry the route metric the
backend would otherwise have used, so a managed default route keeps its place.
Distribution files that match a managed interface are reported as drift
(`foreign`), with the selector and interface, and block the change when they
would win over the managed definition.

Beside the backend files the engine writes `/etc/ndimon-network-interfaces.json`
(the role to interface binding, keys `management`, `ndi` and `tessera`), the
`ndimonndi.slice` and `ndimontessera.slice` units with `70-ndimon-network.conf`
drop-ins for the decoder, the finder and `tesserad`, the Tessera binding
`/etc/tessera/config.d/62-ndimon-network.toml`, the pending banner
`/run/ndimon/net-pending`, and the only nftables table it owns,
`inet ndimon_management` (web access only through management, no forwarding
between the configured interfaces, NDI and Tessera traffic only on their own
interfaces plus loopback). The generic artifact renderer never writes these
files: `ndimon_render` excludes the network artifacts, so an unrelated state
write cannot replace staged backend files, bindings or recovery images.

### Confirm policy

`confirm_policy` is part of the document. `token_only` (default) accepts a
confirmation from any address that holds the transaction token;
`require_management_address` additionally requires the request to arrive on the
new management address (the trusted local socket IP, the pending management IP
and the management interface's only live IPv4 must agree; mapped IPv4 is
normalized and forwarded headers are never proof). The token is always required.
Neither policy changes the nftables guards. The operator wording of the
confirmation window follows the policy.

### DNS search domains

A management network (DHCP or static) can carry up to six search domains: host
names of 1–253 ASCII characters made of labels of 1–63 letters, digits and
hyphens, without a trailing dot; order, case and duplicates are kept. They are
rendered into the management definition only (Netplan `nameservers.search`,
NetworkManager `ipv4.dns-search`, networkd `Domains=`, ifupdown `dns-search`) and
read back from the same keys when the host is adopted; NetworkManager's property
is part of the snapshot, so Revert restores it. Lease-provided domains are never
written. Routing-only `~domains`, internationalized names, more than six entries,
any other `nameservers` key and, on a DHCP definition, lease-domain settings
(`UseDomains=` other than `no`, Netplan `use-domains` other than `false`) are
refused as unrepresentable. Documents without search domains serialize exactly as
before.

### Refused adoption stays readable

When the host cannot be adopted, `status()` still succeeds: `profiles` is the
unadopted default, `networks` holds the live addressing of the port read-only, and
`refusal { reason, detail }` carries the stable code and the key and file. `stage`,
`preview` of a candidate and the handoff calls fail with the same refusal.
Polling callers use `observe()`, which does not run first-adoption discovery for a
network nobody has committed (no backend tool runs for it).

### Certification gate

`EngineConfig::certified_backends` is empty by default. Privd fills it from
`/etc/ndimon-network-certified-backends.json`, a root-owned JSON array of backend
names (`"networkd"`, `"netplan"`, `"networkmanager"`, `"ifupdown"`) that the
`ndimon-privd` package installs listing all four (a conffile an operator may edit).
A missing, unreadable or malformed file leaves it empty and is logged once; it does
not fail the request or the boot job. While the
detected backend is not listed, `status()` and `preview()` work (`writable` is
false) and `stage`, the apply job, `import_handoff` and `commit_handoff` are
refused with `unsupported { what: "backend_not_certified" }` (a candidate that
changes nothing still stages as unchanged). Passing automated tests do not
certify a host by themselves; the packages list all four backends because each
passed the lifecycle (apply, Keep, Revert, expiry, reboot recovery) with the real
tools in disposable containers, and an operator withdraws a
backend that has not been accepted on their backend and host combination.

## Integration contract

Construct `Engine::new(host, Arc<dyn Authority>, config)`. Production `Host` uses
`/`, Linux boot ID and uptime, and the fixed command runner. `Host::fixture`
accepts a private filesystem root, a fake runner and a fake clock for tests.

`Authority` is mandatory. Its `load` and `compare_and_commit` must access the ONE
atomic desired-state/network commit decision, including the global revision, the
complete `NetworkProfiles`, backend metadata and the transaction identity. A
successful replacement is the commit point. A failure after the rename is
resolved by reading back the exact replacement content and identity. The
`network_commit_revision` is persisted with the job ID and both are preserved
across unrelated global desired-state writes; the decision revision must equal
the journal's expected next revision even when the current global revision has
advanced meanwhile. Never write two authoritative commits. Privd supplies
`StateAuthority`, which CAS-replaces the actor's envelope under its shared
decision lock and preserves unrelated domains.

Public methods:

- `status()` and `observe()` return desired and observed profiles, usage, drift,
  `writable`, `externally_managed`, `quiesced`, a secret-free pending transaction
  and, when adoption was refused, the `refusal`.
- `preview(&NetworkProfiles, expect_revision)` validates, snapshots a reviewable
  plan in memory and reports role and service impact, removals, the policy and
  the 60-second window.
- `stage(&NetworkProfiles, expect_revision, Option<SessionPath>)` returns
  `Stage { unchanged, pending, token }`; the secret is returned only here.
- `pending(token)`, `confirm(token, Option<IpAddr>)` and `revert(token)` are the
  three token-scoped operations. The secret is serialized only into their
  authorized replies.
- `rollback()`, `boot()` and `sync_tessera()` are privileged lifecycle operations.
- `migrate_legacy()` imports and recovers the state of 1.3.x explicitly (see
  Upgrade); `boot()` also resumes it.
- `prepare_handoff()`, `cancel_handoff(id)`, `import_handoff(source)` and
  `commit_handoff(source, impact, session)` implement the Tessera handoff.
- `run_apply_job(job_id)` and `run_recovery(job_id)` are the system-manager entry
  points.
- `check_address(Ipv4Addr)` is the read-only ARP check (`true` means duplicate).
  `guard_immediate_session` exposes the typed refusal of changes that would cut
  the caller's own session.

`validation::parse` accepts the current shared document, `import_legacy` adapts
the flat JSON of 1.3.x and `import_semantic` the nested IPv4 envelope of the
Tessera exchange. They return the revision separately; the shared type remains
the only document. Unknown fields and types and inputs above 65,536 bytes are
refused; private journals are capped at 2 MiB.

## Privileged job argv and scheduling

Privd installs `/usr/libexec/ndimon/ndimon-privd` and exposes exactly:

```text
/usr/libexec/ndimon/ndimon-privd network-job apply <64-lowercase-hex-job-id>
/usr/libexec/ndimon/ndimon-privd network-job recover <64-lowercase-hex-job-id>
/usr/libexec/ndimon/ndimon-privd network-job boot
```

They dispatch to `run_apply_job`, `run_recovery` and `boot`. No token is accepted
on argv; only the public job ID is present. The commands are callable by root and
use the same authority and configuration as socket operations. A job ID is valid
only after strict syntax checking and must match the durable journal under the
lock; stale jobs do nothing. (`ndimon-privd --network-boot` remains an alias of
`network-job boot`.)

Stage persists the pending journal, then invokes, as an argument vector:

```text
/usr/bin/systemd-run --quiet --collect --unit=ndimon-network-guard-<id>
 --on-active=180000ms --on-unit-inactive=10s
 --timer-property=AccuracySec=100ms --timer-property=RandomizedDelaySec=0
 --property=RuntimeMaxSec=90s --property=TimeoutStopSec=2s
 --property=KillMode=control-group --property=NoNewPrivileges=yes
 --property=ProtectClock=yes --property=CapabilityBoundingSet=~CAP_SYS_TIME
 -- /usr/libexec/ndimon/ndimon-privd network-job recover <id>
```

Only after that succeeds does an equivalent `ndimon-network-apply-<id>` timer start
with `--on-active=3000ms`, no repeating option, and the `apply` job argv. After
durable readiness, an `ndimon-network-window-<id>-<generation>` timer uses the
remaining 60-second deadline and repeats every 10 seconds for failed recovery.
The guard remains armed throughout. Early recovery jobs check the monotonic
deadline and rearm a generation-specific timer. Recovery also ensures exactly one
`ndimon-network-retry-<id>` repeating 10-second timer, including after boot.
Cleanup stops only this transaction's exact timer names. Transient timers survive
the control plane and privd exiting, but **not a reboot**: the boot oneshot is
mandatory.

`ndimon-net-boot.service` runs `network-job boot` (`Type=oneshot`,
`RemainAfterExit=yes`, `Restart=on-failure`). It is ordered before the control
plane, the finder, the decoder and `tesserad`, which require it, and a successful
start queues those units again (`ExecStartPost=… network-job queue`, which names
`tesserad` only where its unit is installed), so a failed boot job can never
leave the stack down. Do not restart the oneshot when restarting its consumers;
stage starts it first if it is not active.

## Errors and confirmation accounting

All functions return `Result<T, ndimon_types::ApiError>`. Convert with
`ndimon_types::ErrorEnvelope::from(error)` at the HTTP boundary and use
`http_status()`; never parse `Display` strings. Confirmation kinds are exactly
`not_pending`/`expired` (410), `still_applying`/`wrong_address` (409),
`token_mismatch` (403) and `rate_limited` (429); a malformed token is
`invalid`/400. Apply failures carry `domain: network`, a step and a stable reason.
Pending keeps the original apply and recovery failures separately, and a failed
recovery is not reported as restored.

The HTTP front door owns the shared rolling per-peer limiter (five mismatches in
60 seconds). It checks the limiter before parsing and calls
`counts_network_token_failure()`, which is true for `token_mismatch` and internal
errors only: pressing Keep with nothing pending, while still applying or after
expiry never counts. One budget covers Pending, Keep and Revert; mapped IPv4 is
normalized, entries are bounded and hits expire at age >= 60,000 ms. The library
implements the deterministic syntax, lookup, token, boot-expiry, recovery,
readiness, address and commit ordering.

## Upgrade from 1.3.x

`migration.rs` reads both shipped Python pending-journal layouts, the flat
`committed.json`, the old base transaction snapshot and the validated VLAN index,
and imports them as the first native decision. An in-flight old transaction
restores its original files and activation first; a completed Python commit is
recognized and kept. A separate durable upgrade journal and a repeating recovery
job preserve the before-set across failed recovery or an interrupted import. The
old helpers are never executed (they are gone). Invalid legacy state refuses the
migration and is not silently dropped. The historical writable staging ancestors
are traversed with descriptor-relative, no-follow reads into the private
0700/0600 stores; ordinary engine storage keeps its stricter parent checks. The
import allocates exactly the actor's next revision; legacy counters are never
adopted as global revisions. The old `committed.json` is an import source only.

## Ownership handoff

Handoff moves a configured port between NDIMon-R and a standalone Tessera
installation without two managers editing it. The exchange is explicit: both
sides' writer locks, intent and artifact hashes. The receiving side's changes use
the ordinary pending journal and confirmation window, and a peer's journal bytes
never become NDIMon's journal. Ordinary operations refuse a foreign owner.

The native commands are `prepare_network_handoff`, `cancel_network_handoff {id}`,
`import_network_handoff {source:{product,id}}` and
`commit_network_handoff {source:{product,id}}`; the privileged bridge has the
matching `handoff_prepare`, `handoff_cancel`, `handoff_import` and
`handoff_commit` calls. Public aliases are POST `/v1/Network/handoff/prepare`,
`/cancel`, `/import` and `/commit` behind the normal login and CSRF checks, with
the counterpart error envelope. `import` returns `result.impact.token`; send it as
`expect_impact` on `commit`. Commit returns the usual secret pending token, usable
only through Pending, Keep and Revert. It never keeps the change implicitly.

The root-private exchange has exactly these top-level fields: `schema`, `id`,
`product`, `version`, `network`, `backend`, `adoption`, `artifacts`,
`nftables_table`, `bindings`, `created_at`. `network` is the semantic shape with
nested `ipv4`, semantic `audio`, optional product `ndi` and `policies.confirm`.
`adoption` holds the interface, the parent and VLAN NetworkManager UUIDs, the
Netplan parent definition identity and the adopted metrics, and no foreign
configuration text. `artifacts` is a list of `{path,sha256}` and `bindings` maps
semantic roles to interface names. The receiving engine refuses a normalization
that would alter intent; its added NDI role follows Tessera's audio attachment,
and NDIMon's own binding bytes keep the legacy `tessera` key.

The releasing product writes `quiesce.json` as `{id}`. The receiving product takes
its own mutation lock, then the releasing product's, and writes a provisional
`external.json` with `id`, `receiver`, `transaction` and `document_sha256`. The
releasing product stays fenced throughout the receiver's transaction. The
receiver's authoritative commit decides ownership: before Keep or recovery the
releasing product owns the configuration, after Keep the receiver does. The
receiver writes the same identity as `<id>.done` only after that decision is
durable. A failed publication leaves the ordinary pending journal and timers for
cleanup and cannot roll back a kept decision. A successful rollback records the
restored state before releasing the peer, so later cleanup cannot replay
snapshots over a new transaction. Counter values changing in a live nftables
table do not invalidate a review; changed guard configuration does.

The releasing product checks the matching marker, document hash and `.done` on
boot and on requests, archives its network decision and private lifecycle records
under `archive/<id>/` and clears quiescence. The shared `state.json` envelope is
retained because it also owns other domains; the external marker fences the
archived network section, including replay to the followers. A stale or malformed
`.done` is reported in `stale_done` and grants no ownership. Artifact hash failures
use the typed `drift {path}`/409 refusal.

Two engines of the same product cannot hand off to each other: they share a state
root and artifact prefix.
