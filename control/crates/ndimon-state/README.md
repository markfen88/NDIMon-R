# ndimon-state

The desired-state actor of NDIMon-R. One Tokio task owns the one desired-state
document; every change to the appliance's configuration, from the web UI, the
CLI, a plugin or the package migration, is a typed command that enters its queue.
The actor validates the command, plans its effects and impact, persists a new
revision, applies it through the privileged daemon and the C++ services, and
emits events. Nothing else writes configuration.

This crate does not speak HTTP: authentication, CSRF, SSE framing, body limits,
listener setup and shutdown belong to `ndimon-control`, which calls the
[`StateHandle`](src/lib.rs) described here. To add or change a domain, follow
the domain modules.

| Module | Role |
|---|---|
| `lib.rs` | `StateHandle` (the cloneable front of the actor), the actor loop, queries, observed state and drift |
| `domains/` | One module per domain implementing `DomainModule`, and the registration and command-routing tables |
| `plan.rs` | The generic planner: diff the candidate against the committed document, merge domain plans and seal the impact |
| `commands.rs` | Command activity tracking for the live UI |
| `persist.rs` | `StateFile`, the on-disk envelope, and the stage/commit/rollback protocol |
| `apply.rs` | The `Applier` boundary and `ProductionApplier` (privd client, decoder, finder and Tessera clients) |
| `follower.rs`, `ipc.rs` | Delivery of complete domains to the C++ services and the transports they use |
| `import.rs`, `bin/ndimon-state-import.rs` | First-start import of the 1.3.x `/etc` files and the root migration binary |
| `network_authority.rs` | The decision lock and the network engine's commit authority |
| `backup/` | Schema-1 backup capture, review, restore and undo |

## Document, domains and ownership

`DesiredState` (in `ndimon-types`) has nine domains: `network`, `tessera`,
`audio`, `device`, `outputs`, `ndi`, `display`, `presets` and `plugins`. All nine
are **native** in 1.4.0: the table `ndimon_render::OWNERSHIP` is the one policy,
and state registration, the renderer and privd's write gate all read it. A
native domain changes only through commands; its `/etc` files are generated
projections and are never read back as input. A domain marked followed would
instead be re-imported from its legacy files (see Following); none is.

`domains::REGISTRY` is the ordered table of domain modules, and
`domains::COMMAND_ROUTES` is the sole table that decides which domain owns a
command (each command has exactly one owner). The order matters: network first,
Tessera before audio, so a Tessera reload happens before the audio consumers on
both apply and rollback. Audio is a single domain: the engine choice (v1/v2) and
the graph are one transaction, so routes without their engine cannot be saved.

## Revisions and commands

The envelope carries one **document revision**, bumped by every committed change,
and a **last-changed revision per domain**. A command carries optional
`expect_revision`, `expect_impact` (the sealed impact token) and `confirm`:

- Native commands compare `expect_revision` with the document revision. A legacy
  adapter calls `StateHandle::submit_domain(request, origin, domain)` and is
  compared with the domain's revision instead, inside the actor; an unrelated
  change cannot invalidate an audio save or its confirmation token. A stale value
  is refused with `revision_conflict` carrying the current one.
- A command that changes nothing keeps its revisions. Revision values can skip
  numbers; they never go backwards. Legacy counters are discarded at import and the
  first revision is zero. Revisions stay within JavaScript's exact integer range.
- The actor validates the complete candidate with `ndimon_types::validate`,
  plans the changed paths and actions and seals the impact with
  `Impact::sealed`. Interactive (UI) and plugin origins must present a matching
  `expect_impact` or `confirm: true` for a disruptive change; a stale token is
  refused even with `confirm: true`. Boot and migration origins are exempt.
- Commands that produce no document diff (reconnect a source, refresh discovery,
  preview a splash, reboot, restart a service, network Keep and Revert) are
  *imperative*: they pass the same validation, CAS, confirmation and preflight in
  the same queue, run their effect and emit progress without allocating a revision
  or a `state_changed` event, and never render artifacts or replay an irreversible
  operation on rollback.

## Persistence and the transaction

`/var/lib/ndimon/state` (0750, owned by `ndimon-control`) holds `state.json`,
`state.prev.json`, `state.pending` while a write is in flight, `state.lock` (the
decision lock) and `rendered.json` (the manifest of what privd wrote: path,
revision, SHA-256, owner, group, mode). `state.json` is the envelope:
schema version, document revision, the typed document, the per-domain revisions,
the original text of the legacy files (so unknown keys survive) and a few
operation journals (the network commit decision, the plugin job and activation
journal, 32 recent plugin results). Files are 0640 (the lock file 0600). Every
write creates a temporary file, fsyncs it, renames it and fsyncs the directory.

A transaction runs in this order, all inside one actor turn and under the
shared decision lock:

1. Route the command, reduce a clone of the envelope, validate, plan, check the
   confirmation, preflight the changed domains.
2. Write `state.prev.json`, create and fsync `state.pending`, then atomically
   replace `state.json` with the candidate. Removing and fsyncing `state.pending`
   is the commit point.
3. Apply: one render through privd (`RenderArtifacts { revision }`, which reads
   the candidate from `state.json`), then each changed domain's apply hook in
   registry order, then the follower deliveries.
4. Commit, then emit `apply_progress(started)`, `apply_progress(done)`,
   `state_changed` and `observed`.

On a failure the actor restores the previous envelope, re-renders and re-applies
the previous configuration through the rollback hooks and reports `apply_failed`;
the revision does not change. A failed rollback is reported explicitly, can leave
the observed state out of step with the document and needs operator attention.
A crash with `state.pending` present restores `state.prev.json` the next time the
state is loaded (by the actor at start, or by privd's network jobs), and
temporary files of dead writers are removed. Readers other than the actor must not
inspect a staged `state.json` during a transaction.

## Rendering

`ndimon-render` is the single declaration of artifact paths, owner, group and
mode (`PATHS`) and of the per-domain renderer, import and drift projection
(`REGISTRY`). Privd renders the whole set before it replaces any file, and only
what changed. Generated files carry no provenance keys (readers parse them);
provenance is `rendered.json`.

- The device settings and the decoder's `ndimon-decN-settings.json` are *mixed*
  files (`MIXED_PATHS`): several domains own different keys. The renderer replaces
  only the native value spans in the newest text of the file and preserves every
  other byte, unknown keys, spacing and escapes included; privd rereads these
  files under `/etc/.ndimon-config.lock` before rendering. The audio domain owns
  only `audio_engine` in the device file, the device domain the rest.
- The network engine's artifacts (backend files, bindings, slices, recovery
  images) are excluded from the generic render so an unrelated write cannot
  replace staged state; the engine publishes them itself.
- Freshly generated JSON is compact, with integral floats serialized as integers.
  Unchanged imported audio keeps its input text and only its `revision` changes.

## Effects, followers and replay

`Applier` is the boundary between the actor and the machine, with a fake in tests.
`ProductionApplier` talks to privd (the framed client), the decoder
(`/tmp/ndi-decoder.sock`, JSON lines, bounded reply, 1500 ms), the finder
(`/run/ndimon/finder.sock`) and Tessera (`/run/tessera/control.sock`, 3 s).

The C++ services are *followers*: the actor sends them complete accepted domains
as `{"action":"apply_config","revision":N,"config":{"<domain>":...}}`, using the
global revision, 1-based output IDs and newline framing. The decoder accepts
`device`, `outputs`, `ndi`, `audio`, `display` and `network`; the finder accepts
only a complete `ndi`. `follower::deliver` probes `status`, retries transport
failures five times (100, 200, 400, 800 ms), rejects an unsuccessful reply, a
non-empty `refused` list or a wrong acknowledgement, and accepts only the
immediately reserved revision `R+1` when recovering committed `R`; it never
advances durable CAS. Audio keeps its `reload_audio` path (live graph changes) and
a privd decoder restart (engine and v1 master/period changes); video-delay memory
is checked with the decoder's `check_audio` before such a change is staged.

`StateHandle::start` renders the committed state at boot (`render_boot`) before
listeners open; it does not wait for the followers. `spawn_replay` then sends a
`Replay` message through the actor queue: it retries missing or restarted sockets
with a backoff from 100 ms to 10 s, and after success repeats every 30 s, so a
decoder, finder or `tesserad` that restarted gets the current native domains again.
Replay is not a command, an old snapshot cannot overtake one, and it never
repeats a disruptive action. Domains' `replay` hooks reinstall accepted
configuration (device: hostname, non-empty NTP, the reboot timer and the complete
follower domain), and the privileged settings go to privd only when they differ
from what this process last applied, so after a restart everything is asserted
once and an unchanged system costs no request. Failed replays mark their domain
`failed` in the observed state until a later replay succeeds.

`spawn_observers` runs each native domain's `observe` future in one abortable
task group; their observations enter the actor queue as events and update
`Query::Observed` (per-domain sync status: in sync, applying, pending restart,
pending confirmation, failed, drift).

## First start, import and following

On a first start the actor imports every declared legacy input (the table of
`ndimon-render::PATHS` plus the Tessera state and configuration, the hostname and
the ALSA files), validates the result and writes the initial document. Malformed
saved audio fails the import rather than being silently repaired. Unknown keys
are logged by name and the original text is retained. The package migration runs
the `ndimon-state-import` binary once as root, with no arguments, before any
service starts and while the writers are stopped: it preserves each present
original as `<file>.pre-control` (create-new, never replaced), imports the effective
Tessera name and latency and representable flows, content-addresses the splash
logo into `/var/lib/ndimon/assets`, initializes `/var/lib/ndimon/state` and hands
the directory and files to `ndimon-control`. Existing state is recovered rather
than re-imported. The network journals and VLAN state of 1.3.x are imported by
the network engine's own migration at boot.

`spawn_follower` (still started by `ndimon-control`) polls the legacy input files
every two seconds and compares their complete bytes, so atomic replacements and
coarse mtimes are handled. For a domain whose ownership is followed it would
reimport and emit `state_changed`; with every domain native it only keeps the
retained source text current and detects **drift**: when a generated file differs
from what the renderer produced, the actor emits a deduplicated `drift` event
with the expected and found hashes and shows it in `Query::Observed`. The actor
never adopts a hand edit of a generated file; the next render replaces it. Text-only
updates consume no revision and do not replace `state.prev.json`. Inaccessible
(root-only) inputs keep their last imported text: there is no privileged read
request, and permissions are not to be widened to work around that.

## Backup and restore

`backup/` implements schema-1 backup documents (the same format and checksum as
1.3.x, extended with `device.web_allowed_hosts`) for export, review, restore and
undo, coordinated in Rust: `capture`, `restore`, `undo` and a review that
validates the checksum and normalizes legacy discovery and splash fields. Restore
goes through the same reducers, validation, planner, confirmation, asset preflight,
single render, apply/rollback hooks and commit as any command (`ImportBackup`,
one revision for all changed native domains, global CAS). Restore order is audio,
outputs, NDI, display, device, presets, Tessera, plugins; the network section is
never part of the ordinary import: a changed network is staged last, as a normal
confirmed `StageNetwork` with the trusted connection context, and one Keep commits
it, so a restore with a changed network has one settings revision and one network
revision. A last-stage failure is reported in `results.network` and does not undo
the restored settings.

Collaborators are injected: `backup::environment::BackupIo` supplies host
metadata and the password record (imported hashes are written through privd
`WriteLegacyConfig`, never rehashed, and invalidate sessions through the
credential stamp) and `backup::plugins::PluginBackup` (`available`, `export`,
`import`) supplies plugin sections. `ndimon-control` wires `backup::plugins::Native`
with `StateHandle::with_plugin_backups`; schema-1 plugin sections keep
`{version,data}`, at most 1 MiB each, import only plugin-owned state through
`SetPluginData` with global CAS and never install packages or change
enabled/version intent. A manifest-v1 plugin is always unavailable and reports
"restorable after plugin support is available". Tessera contributes through its
native domain, not through that trait. Upload files and the five most recent
pre-restore snapshots (`/var/lib/ndimon/backups`, 0600) embed logo bytes, so a
restore can rehydrate assets after unreferenced content was collected; uploads
expire after fifteen minutes. Logos use the managed `AssetStore`
(`/var/lib/ndimon/assets/<sha256>` with a PNG/JPEG reader projection) with
current, previous and in-flight retention.

## Plugins and Tessera

The `plugins` domain owns installed, enabled and version intent, each process
plugin's `state` subtree, manifests, catalog and feed queries, uploads and static
UI assets; `domains/plugins/host.rs` is the production host (see
[plugins](../../../docs/plugins.md)). `StateFile::plugin_runtime` is the sole
activation journal (the 1.3.x `plugin-runtime.json` is imported once and then
read-only) and `plugin_job` holds the one in-flight lifecycle job. Reservation
metadata is saved durably before any privileged effect, including no-diff
reassertions; a lost acknowledgement leaves an investigation marker and an
uncertain job blocks new lifecycle work, while a known failure releases the
reservation and restores only the plugin's prior intent. One supervisor outside
the actor polls `PluginJobStatus` and submits terminal results through a private
`Complete` message (never an HTTP command); compatibility handlers wait for the
retained result for up to 310 seconds, the supervisor requests cancellation at 300
seconds and keeps observing after an HTTP disconnect, and a restart resumes the
saved job. Package rollback stays in privd and is never replayed by the actor.
Manifest-v2 processes have per-plugin framed sockets, schema-checked commands and
proposals that commit complete subtrees with global CAS; uploads stream to private
files with a 200 MiB cap. Manifest-v1 plugins cannot run and stay installed and
inactive.

The `tessera` domain renders `60-ndimon.toml`, `61-ndimon-flows.toml`, the probe
flow file (empty, a second writer is retired), the two ALSA files and, with the
network domain, `62-ndimon-network.toml`; privd validates a complete staged
configuration with `tesseractl config check` before publishing. Device and clock
values are pinned in the daemon's `locked` lists, and the flow lock
(`[control] locked = ["flows"]`) is written when the running daemon reports that
it can enforce one. Tessera 0.3.0 or later is the supported runtime; an older
daemon is reported as a flow-ownership conflict (drift) in the observed state
instead of being given a setting it would reject. Because Tessera refuses direct
setters for locked values, the actor applies these settings through `ReloadConfig`
after rendering and returns the daemon's own impact and token. A replay reloads the daemon only when a file
below `/etc/tessera/config.d` differs from what the last reload saw or the daemon
is another process than the one reloaded (told from `metrics.daemon.uptime_ms`),
and once after the control plane starts. See [Tessera](../../../docs/plugins/tessera.md).

## Network

`StateAuthority` implements the network engine's one commit decision: the network
intent, document and domain revisions, the job identity and the backend metadata
are replaced together by compare-and-swap under the decision lock, and unrelated
writes preserve them. There is no second authoritative store. Network Stage, Keep,
Revert, Rollback and the handoff commands are imperative and use `live_checked`
through the common dispatcher: their reducers validate without publishing pending
intent, the module asks privd's engine for a preview, seals its exact impact with
the network revision and checks the forwarded confirmation before Stage. The
trusted socket context (`NetworkSessionPath`: accepted peer and local address) is
server-only, excluded from serde, schemas and impact digests, and is never taken
from JSON, `Host` or forwarding headers. A pending network change reserves the
next revision: the network guard refuses every unrelated write, import and plugin
completion with `locked` until Keep or Revert, and the network observer reports
secret-free pending state and drift. Guards, replay and the observer read a network
status that does not run first-adoption discovery, kept for 30 seconds and dropped
whenever a network command may have changed it. Runtime effects release the decision lock
before the RPC; Keep commits through `StateAuthority`, after which the actor
refreshes and replays the accepted document, and a failed follower replay cannot
undo a successful network decision.

## Shutdown

`StateHandle::shutdown(limit)` waits for the transaction in flight, refuses every
later write with "shutting down" and leaves no `state.pending` marker (a failed
rollback is finished under the decision lock exactly as the next start would).
`ndimon-control` calls it with an 8-second limit while the HTTP server drains.

## Integration in the control plane

```rust,ignore
use std::{path::PathBuf, sync::Arc};
use ndimon_state::{StateHandle, apply::ProductionApplier, ipc::UnixDecoder};

// After ndimon_state::domains::plugins::{host,runtime}::initialize(..).
let applier = Arc::new(ProductionApplier {
    memory: Default::default(),
    privd: ndimon_privd_proto::client::Client::default(),
    tessera: ndimon_tessera::Client::default(),
    decoder: Arc::new(UnixDecoder::default()),
    finder: Arc::new(UnixDecoder {
        socket: "/run/ndimon/finder.sock".into(),
        ..UnixDecoder::default()
    }),
});
let state = StateHandle::start(
    "/var/lib/ndimon/state", PathBuf::from("/"), applier.clone(),
).await?;
let replay = state.spawn_replay();
let follower = state.spawn_follower(PathBuf::from("/"));
let observers = state.spawn_observers(&applier);
```

(`ndimon-control`'s `main.rs` is the reference and also wires the backup
collaborators.) Use `state.query`, `state.submit(CommandRequest, Origin)` and
`state.subscribe()` for command, query and SSE handling; preserve typed errors and
statuses, derive the UI origin from the authenticated session's public ID and, on
broadcast lag, send an SSE `resync` and read a fresh snapshot. Abort the replay,
follower and observer handles on shutdown. The legacy URL adapters in
`ndimon-compat` take `StateHandle: FromRef<State>` and
`ndimon_compat::legacy_router()` is merged into the authenticated router; the
routers perform no authentication, CSRF, origin or request-size checks, the front
door does.

Start order on the device: root migration with the writers stopped, privd's socket
available, actor start with recovery and the boot render, public listener ready,
then the decoder and finder start. The control identity needs the `ndimon` group
(rendered files) and the supplementary groups of its unit.

## Compatibility limits

- Validation reports portable field errors, not the exact wording and ordering of
  the retired JavaScript validator.
- Output saves select audio v2 atomically and may return `confirmation_required`
  for the decoder restart; the `audioEngine` POST uses confirmation and apply
  instead of deferring an unconditional restart. V1 master and period changes
  restart with confirmation rather than persisting a partial subset.
- Matrix normalization returns before/after contribution arrays, ordered by typed
  route identifiers rather than object insertion order.
- Fresh JSON is compact; property order, optional-field combinations and
  floating-point exponent spellings are not proven byte-identical to 1.3.x output.
  Unchanged imported audio and mixed files are exact.
- Importer gaps that still hold: the splash logo is imported by content address
  only from the legacy path; legacy VLAN and base-network adoption happen in the
  network engine, not here; nested unknown keys are retained but not diagnosed.
