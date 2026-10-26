# NDIMon-R plugins

A plugin adds a tab to **Plugins**, keeps a small piece of validated
configuration and may run its own program. Plugins are **out-of-process**
(manifest v2): the program is a prebuilt executable in any language, started by
systemd in a sandbox, and it talks to `ndimon-control` over a framed JSON
protocol on a Unix socket. No plugin code runs inside the control plane, and the
device has no JavaScript runtime. Plugins written for 1.3.x (manifest v1, in-process
JavaScript) stay installed but cannot run; see
[Manifest v1 (JavaScript) plugins](#manifest-v1-javascript-plugins).

## Trust model

- Any authenticated administrator can install a plugin. Install only archives
  from publishers you trust.
- A feed over HTTPS authenticates the feed server and protects the transfer. The
  SHA-256 in the feed entry checks the archive bytes against that server; it is
  not a publisher signature. HTTP feeds need the device setting
  `allow_insecure_feeds: true` and lose those protections. Feed, checksum, archive
  and redirect URLs all follow the same rule, and URLs with credentials are refused.
- The plugin **process** is unprivileged and sandboxed (see
  [The process unit](#the-process-unit)). It never talks to the privileged daemon:
  it can only ask the host to run one of the `priv_commands` its manifest declares.
- A `priv/<command>` executable runs as **root**. It is trusted code, exactly like
  a package maintainer script. Make it idempotent and keep it small.
- The framework cannot undo the external side effects of such a command.

## What a plugin is

### Archive and installed layout

A plugin is a gzip-compressed tar archive with `plugin.json` at its root (no
enclosing directory). For example, on your packaging machine:

```sh
tar -czf hello-plugin_1.0-1_amd64.tar.gz -C hello .
```

```text
plugin.json            the manifest
bin/hello              the process executable named by process.exec
ui.html                the UI fragment, plus any other static UI assets
priv/<command>         one root executable per entry of priv_commands
```

The device accepts only regular files and directories. Links, devices, absolute or
`..` paths, control characters in names and duplicate entries are refused. The
archive is limited to 200 MiB, the expanded tree to 1 GiB and 20,000 entries, and
the tar metadata stream is bounded as well. Ownership and permissions in the
archive are ignored: everything is installed root-owned, 0755 when the archive
marked it executable and 0644 otherwise.

On the device:

| Path | Owner, mode | Purpose |
|---|---|---|
| `/opt/ndimon-r/plugins/<name>/` | root, 0755 | Installed plugin files. `priv/ALLOWED` is generated from `priv_commands` at install time |
| `/var/lib/ndimon/plugins/<name>/` | ndimon:ndimon, 0700 | Framework state directory, removed by a purge |
| `/var/lib/ndimon/state/state.json` | ndimon-control | Desired state: `plugins.<name>` holds `installed`, `enabled`, `version` and the plugin's own `state` subtree, and the same file is the durable journal of running lifecycle jobs |
| `/etc/ndimon-plugins.json` | root:ndimon, 0640 | Generated projection of the accepted registry. Do not edit |
| `/var/lib/ndimon/import/plugin-downloads/` | ndimon-control, 0700 | Upload and feed staging |

The control plane is the only writer of the registry. A plugin never writes it; it
proposes a new `state` subtree and the host commits it (see
[State, commands and events](#state-commands-and-events)).

### Manifest

`plugin.json` is at most 64 KiB. Unknown fields are rejected. A minimal manifest:

```json
{
  "schema_version": 2,
  "name": "hello",
  "version": "1.0-1",
  "title": "Hello",
  "min_ndimon": "1.4.0",
  "process": {"exec": "bin/hello", "args": []},
  "state": {
    "schema": {"type": "object", "additionalProperties": false,
               "properties": {"message": {"type": "string", "maxLength": 80}},
               "required": ["message"]},
    "default": {"message": "hello"}
  },
  "commands": {"set_message": {
    "input": {"type": "object", "additionalProperties": false,
              "properties": {"message": {"type": "string", "maxLength": 80}},
              "required": ["message"]},
    "output": {"type": "object"}
  }},
  "events": ["state_changed"],
  "ui": "ui.html"
}
```

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | 2–32 characters: a lower-case letter, then lower-case letters, digits and `-` |
| `schema_version` | with `process` | Must be `2` when `process` is present |
| `version` | yes | Debian version syntax (starts with a digit; letters, digits and `.+:~-`; at most 128 characters). Ordering follows Debian rules, including epoch, revision and `~` |
| `title` | yes | 1–40 characters, shown on the tab |
| `min_ndimon` | yes | Minimum NDIMon-R version, same syntax and ordering as `version` |
| `icon` | no | A Bootstrap Icons class: `bi-` followed by lower-case letters, digits and `-`. Default `bi-puzzle` |
| `process` | v2 | `exec`: path of the executable relative to the plugin directory (at most 240 characters, components of `A-Za-z0-9._-`, no `.` or `..`); `args`: at most 32 arguments of at most 1024 bytes without control characters |
| `state` | with `process` | `schema` (an object schema) and `default` (an object that validates against it) |
| `commands` | no | Up to 64 typed commands. A name is 1–64 characters of `a-z0-9_-`; each has an `input` and an `output` schema |
| `events` | no | Event kinds the process consumes, without duplicates: `state_changed`, `observed`, `drift`, `resync` |
| `ui` | no | Simple file name of the UI fragment. Default `ui.html` |
| `priv_commands` | no | Names (same syntax as plugin names) of the root commands the process may request |
| `services` | no | `.service` units the plugin controls on enable and disable |
| `supplementary_groups` | no | Groups added to the process unit |
| `readwrite_paths` | no | Paths the process unit may write |
| `config_files` | no | Absolute paths of configuration files the plugin owns (documentation and validation only) |

`state`, `commands` and `events` are refused in a manifest without `process`. The
fields `routes`, `backup`, `install` and `uninstall` belong to manifest v1; a v2
plugin does not use them and no install or uninstall script is run for it.

`min_ndimon` is checked against the installed firmware version with Debian
ordering when the archive is prepared and again as root during installation. An
install on an older host is refused with HTTP 409 `host_too_old`, and so is a
device whose firmware version file is missing. `min_ndimon` is not a tested-build
matrix: an accepted host version does not mean every feature combination was
verified. Test on the minimum version you declare.

### Validation rules

| Declaration | Rule |
|---|---|
| Lists | No duplicates in `priv_commands`, `services`, `supplementary_groups`, `readwrite_paths`, `config_files` or `events` |
| `services` | Names end in `.service` (at most 200 characters of `A-Za-z0-9_@.-`) and equal `<name>.service` or start with `<name>-`, `<name>@`, `ndimon-<name>.` or `ndimon-<name>-`. Exceptions: `tesserad.service` for `tessera` and `bluealsa.service` for `bluetooth`. `ssh.service`, `sshd.service`, `ndimon-r.service`, `ndimon-finder.service`, `ndimon-control.service` and the retired `ndimon-api.service` and `ndimon-watchdog.service` are never allowed |
| `readwrite_paths`, `config_files` | Absolute, only `A-Za-z0-9_./-`, no `..` |
| `readwrite_paths` | Must equal, or lie below, `/run/<name>`, `/var/lib/<name>` or `/etc/<name>`. Only `tessera` may also use any path starting with `/run/tessera`, `/var/lib/tessera` or `/etc/tessera` |
| `supplementary_groups` | Group names follow the plugin-name syntax. Forbidden: `root`, `sudo`, `wheel`, `adm`, `shadow`, `disk`, `docker`, `lxd`, `kmem`, `systemd-journal`, `ndimon`, and the reserved `ndimon-priv`, `ndimon-priv-control`, `ndimon-priv-api` and `ndimon-control`. `tessera` and `tessera-admin` are available only to the `tessera` plugin. As root the install allows `audio`, `video`, `render` and `bluetooth`, and any other group only with a GID of 1000 or more. Every group must exist when the install finishes |
| `state.schema`, command schemas | Data-only JSON Schema subset, at most 32 KiB, 16 levels deep and 1024 nodes: `type` (`object`, `array`, `string`, `boolean`, `integer`, `number`, `null`), `properties`, `required`, boolean `additionalProperties`, `items`, `minLength`/`maxLength`, `minItems`/`maxItems`, `minProperties`/`maxProperties`, `minimum`/`maximum`, and `enum` with at most 256 values. References, regular expressions and unions are refused. The state schema must have `type: object` |
| Plugin data | A state subtree is at most 1 MiB of JSON and 32 levels deep |

The grants of `services`, `readwrite_paths` and `supplementary_groups` apply only
to that plugin's own units; there is no shared host process that could inherit them.

## Process protocol

The host creates `/run/ndimon-control/plugins/<name>/control.sock` (the directory is
0700 and owned by `ndimon-control`) and starts the plugin's unit. The process
connects to the socket whose path it finds in `NDIMON_PLUGIN_SOCKET`.

### The process unit

Privd starts the executable as a transient unit, `ndimon-plugin-runtime-<name>.service`,
bound to `ndimon-control.service`:

- a dynamic, unprivileged user per plugin; no capabilities; `NoNewPrivileges`;
- read-only system (`ProtectSystem=strict`, `ProtectHome`), private `/tmp` and
  devices, `UMask=0077`, 5-second stop timeout;
- **only `AF_UNIX` sockets**: the process has no IP network access;
- `/run/ndimon-privd` is not visible, so the process cannot reach the privileged daemon;
- the plugin directory is mounted read-only at `/run/ndimon-plugin-code` and the
  executable is started from there with the manifest's `args`; the socket file is
  bound to `/run/ndimon-plugin/control.sock`;
- writable paths are exactly `readwrite_paths`, groups exactly `supplementary_groups`.

The executable and every directory above it must be root-owned and not writable by
group or others, and the file must be executable.

### Framing and deadlines

Every message is a 32-bit big-endian byte length followed by that many bytes of
UTF-8 JSON, 1 byte to 1 MiB. Each message is one envelope of exactly three keys:

```json
{"v":1,"id":7,"req":{"command":{ ... }}}
{"v":1,"id":7,"ok":{ ... }}
{"v":1,"id":7,"err":{"code":"rejected","message":"..."}}
```

A request object has exactly one key, the request name. A reply carries the id of
its request and either `ok` or `err`. Connections may idle, but once a frame has
begun its remaining bytes must arrive within 5 seconds; the handshake, every reply
to a host request and every write have the same 5-second deadline. The host
closes the connection on an oversized, partial or malformed frame, a duplicate
or non-increasing request id, a wrong version, a missing or mismatched reply, or
when the 16-frame queue in either direction overflows. A slow or stuck plugin
never blocks the state actor or other plugins.

### Handshake

The first frame is sent by the process, with id 1 and the name and version from its
installed manifest. The host checks the peer's user id and the identity and answers:

```json
{"v":1,"id":1,"req":{"hello":{"name":"hello","version":"1.0-1"}}}
{"v":1,"id":1,"ok":{"protocol":1,"revision":12,"state":{"message":"hello"}}}
```

`revision` is the global document revision and `state` is the committed subtree
(the manifest default when nothing was saved yet).

### Host to process

Host requests use ids from 2 upward. Answer each with `ok` (any JSON value) or `err`.

| Request | Fields | When |
|---|---|---|
| `start` | `revision`, `state` | After the handshake, after a reconnect, and whenever the plugins domain changes. Always carries the complete committed subtree, so make it idempotent |
| `stop` | `reason` | Before the unit is stopped (disable, remove, replacement) |
| `command` | `name`, `input`, `revision` | An HTTP plugin command. Reply `ok: {"output": ..., "state": ...}`: the output validated against the command's `output` schema and the **complete proposed state subtree** validated against the state schema |
| `event` | `event` | A committed event of a kind listed in the manifest |

Delivered events are `state_changed` (the origin is replaced by a neutral one),
`observed` (detail removed) and `drift` (artifact and foreign-file details
removed). Events follow the commit that caused them and keep their order per plugin.
Secrets, session and authentication data never reach a plugin. The `resync` kind is
accepted in a manifest, but no `resync` event is sent: a reconnect always starts
with `start` and the full state, so a plugin never has to patch missed events.

### Process to host

Process requests use ids greater than 1 and strictly increasing.

| Request | Fields | Effect |
|---|---|---|
| `propose_state` | `expect_revision`, `state` | Validates `state` against the schema and commits it as this plugin's subtree with compare-and-set on the global revision. Reply `ok: {"revision": N}` |
| `privilege` | `command` | Asks the host to run one entry of the manifest's `priv_commands` as root. Reply `ok` with the command's output |

A refusal is `err: {"code":"rejected","message":...,"revision":N,"state":...}`
and carries the current committed revision and subtree. A plugin cannot allocate
revisions, announce committed state, or write files itself; after a rejected
proposal, resynchronize from the reply.

If a request fails or times out the host drops the session. The control plane
restarts the process with backoff (2, 4, 8, 16 and then 30 seconds) while the
plugin stays enabled, and records a runtime error. A crash changes observations, not
the plugin's enabled intent.

## State, commands and events

- **State.** Everything a plugin keeps is `plugins.<name>.state` in the desired
  document, separate from the installed, enabled and version fields. One plugin owns
  one subtree and cannot write other domains. Because the host stores it, a plugin
  gets revisions, events, backup and restore without code.
- **Commands.** `POST /v1/plugins/<name>/commands/<command>` with
  `{"input": {...}, "expect_revision": N}` validates `input`, sends `command` to the
  process and commits the proposed subtree through the normal transaction. Success is
  `{"ok":true,"revision":N,"output":{...}}`. Compare-and-set is global: a stale
  revision answers 409 `revision_conflict`, and a rejected command resynchronizes the
  process with the committed subtree before the error is returned. Commands need an
  enabled process plugin and are refused with 409 `busy` while a lifecycle job runs.
- **Privilege.** Request an exact `priv_commands` name; see
  [Privileged commands](#privileged-commands).

## Backup and restore

A v2 plugin needs no backup code: its accepted `state` subtree is the backup.
A backup includes `sections.plugins[name] = {version, data}` for every enabled
installed plugin; the five automatic pre-restore snapshots include disabled
installed plugins too. Restore validates the data against the installed manifest's
schema and imports it with global compare-and-set. It can restore into an
installed, disabled plugin. It never installs a package and never changes
enabled or version intent, so install the plugin first. A missing plugin is
skipped with a warning; a validation or import failure affects that plugin only,
with details in `results.plugins`. Data must be a plain JSON object of at most
1 MiB; invalid or oversized data is skipped with a warning in the document's
`warnings` array. Tessera's entry in `sections.plugins` is written by its native
domain instead of a plugin process.

## UI fragment

The file named by `ui` is served at `/plugins/<name>/ui.html`, only while the
plugin is enabled and not broken, under the device's normal authentication. It is
an HTML fragment inserted into a pane of the Plugins page. Its inline scripts run
after insertion. Other static files beside it are served below `/plugins/<name>/`
with fixed content types (`html`, `css`, `js`, `json`, `png`, `jpg`, `svg`, `woff2`;
at most 8 MiB each), `Cache-Control: no-store`, `nosniff` and same-origin framing
protection. The manifest, the process executable, `priv/` and the v1 entry files
are never served. Use the page's own Bootstrap and helpers and keep your content
inside the supplied pane.

```html
<div class="result"></div>
<script>
window['NDIMON_PLUGIN_INIT_hello'] = async function(pane) {
  const result = await window.NDIMON.post('/v1/plugins/hello/commands/set_message',
                                          {input: {message: 'hi'}});
  pane.querySelector('.result').textContent = result.ok ? 'saved' : result.error;
  return () => { /* cleanup when the pane is removed */ };
};
</script>
```

The initializer is `window['NDIMON_PLUGIN_INIT_<name>']`; use bracket notation for
names containing a hyphen. It receives the pane and may return a cleanup function.
The host API is:

```text
window.NDIMON = { get(url), post(url, body), toast(message, type), onStatus(callback), state }
```

`post` adds the session's CSRF token and, when the device answers 409
`confirmation_required`, shows the impact and resends the request with
`expect_impact`. `state` is a getter for the host's current flat view of the
device. `onStatus` registers a callback that receives the device status snapshot
the page samples about every 1.5 seconds, including `plugins: [{name, state,
version, title, icon, status}]` (`status` is null for now), and returns an
unsubscribe function. Do not open another event stream; the page holds one.
Dynamic strings must go in through DOM properties and text nodes, not HTML
interpolation.

## Lifecycle

Plugins are managed on the **Plugins** page (upload an archive, or install from a
feed) or through the [HTTP API](#http-api). Installing, updating, enabling,
disabling and removing a plugin restart nothing in the control plane.

| Operation | What happens |
|---|---|
| Install | Prepares the archive (feed download or upload), installs it as root, records the plugin as installed. A fresh install is **disabled** unless `enable: true` was requested |
| Enable | Starts the plugin's declared `services` (or runs its `enable` command when declared), then the host starts the process |
| Disable | Stops the process and the declared services (or runs its `disable` command). Saved state is kept |
| Update | Installs a strictly newer version from the feed. An enabled plugin stays enabled |
| Remove | Stops the process, then removes the files. Configuration and state are kept; **purge** also removes `/var/lib/ndimon/plugins/<name>/`; **force** skips the plugin's own uninstall step and removes the files even if that step fails |
| Reset state | Clears the accepted plugin registry and each plugin's saved state subtree; installed files stay and every plugin ends up disabled (see below) |

An operation that restarts or interrupts something is answered with HTTP 409
`{"ok":false,"error":"confirmation_required","impact":{"token":"...","lines":[...]}}`.
Show the impact, then resend the same request with `expect_impact` set to
`impact.token`, or with `confirm: true` for a script. Install, update, enable,
disable, remove, cancel and reset all ask. An optional `revision` in the body is the
plugins-domain revision for compare-and-set; it is unaffected by changes to other
domains.

### What the root side does on install

Privd runs these steps as a transient system-manager unit named
`ndimon-plugin-<name>-<number>.service` (`RuntimeMaxSec=600`), serialized by a file
lock. They survive the disconnection of the HTTP client.

1. Resolve the archive from a typed identifier inside the upload directories. The
   file must be a regular file owned by `ndimon-control`, at most 200 MiB, opened
   without following links, and it is copied from that descriptor into a root-private
   snapshot.
2. Unpack under the limits above, validate the manifest (name, the version bound to
   the feed entry, the rules in [Validation rules](#validation-rules)), check
   `min_ndimon`, and refuse a downgrade unless forced.
3. Generate `priv/ALLOWED`, keep the previous version as `.previous-<name>`, write a
   `.broken` marker, move the new tree into `/opt/ndimon-r/plugins/<name>/` and create
   the state directory.
4. Re-resolve every supplementary group. A missing or unsafe group fails the install.
5. Remove the marker.

On failure the files stay with a `.broken` marker; a failed update restores the
previous version and marks it broken for review. A plugin in the `broken` state
offers **Retry remove** and **Force remove** in the UI; force skips the plugin's
uninstall step.
Every plugin operation first restores `.previous-<name>` if the live directory is
missing. Service control ignores hidden and broken plugin directories.

### Jobs, timing and concurrency

Install, update, enable, disable and remove start a root job. The control plane
persists the job id and the original plugin intent with the accepted change, so a
restart of the control plane resumes the job. One lifecycle operation runs at a time;
another request is answered with 409 `{"ok":false,"error":"busy"}`. Other domains
keep committing while a job runs.

An HTTP request waits up to 310 seconds for its job's result. The supervisor
requests cancellation after 300 seconds and keeps observing the job even if the
client disconnects. A job whose unit is gone and which left no result on three
polls in a row, or an acknowledgement that was lost, leaves an investigation marker
and blocks further lifecycle work until an operator has looked at it. A job that
finishes while it is being asked about is not that: privd looks for the result again
once it sees the unit inactive, so a fast refusal always reports itself. Cancel one
job with `POST /v1/plugins/jobs/<job>/cancel`; cancelling a job whose unit has ended
is not an error, and it settles an investigation marker (use the id `unknown` for a
marker whose acknowledgement never named a unit). Restarting the control plane also
looks at an investigation marker once: a result that is there after all completes the
job as it would have, and a unit that is gone without one ends the job as failed. A
failed or cancelled job restores only the plugin registry entry it changed;
unrelated commits are kept, and package files are rolled back by privd, never by
replaying hooks.

### Pending enable

An install with `enable: true` records the accepted intent and a durable
pending-enable observation. The supervisor consumes it and runs the normal enable
operation, so a start-up activation and an explicit enable use the same job
supervisor. The result is recorded as success or `enable_error`. A failed activation
is not replayed at the next start; an explicit enable retries it. The status
fields `restart_pending` and `pending_enable` remain in install replies for wire
compatibility; nothing restarts.

### Corrupt registry and reset

If the imported legacy registry or runtime journal is corrupt, catalog reads keep
working and lifecycle writes return 409 `plugins_state_corrupt`.
`POST /v1/plugins/state/reset` clears the accepted registry, including each plugin's
saved state subtree (a v2 plugin starts again from its manifest default), and
recreates `/etc/ndimon-plugins.json` empty. Installed code and framework state
directories are left in place and all plugins are disabled; enable the ones you
want afterward. Privd refuses the reset unless the accepted registry is already
empty, so a failed reset keeps the original state.

## Feeds

`plugin_feeds` in the device settings is a list of at most 32 URLs, set with the
`set_device` command (`POST /api/v1/commands`), for example
`{"command":{"set_device":{"plugin_feeds":["https://forge.example/api/v1/repos/team/plugins/releases?limit=5"]}},"expect_revision":N}`.
With no feeds the Plugins page is upload-only. Two formats are accepted:

- **Release listing.** The JSON array returned by a GitHub or Forgejo/Gitea
  "list releases" API: objects with `assets[]`, each asset having `name`,
  `browser_download_url` and `size`. A release is used only if it has an asset
  named `SHA256SUMS` (`<sha256>  <file name>` lines). Plugin assets are named
  `<name>-plugin_<version>_<arch>.tar.gz` with `<arch>` `amd64` or `arm64`; only the
  device's own architecture is listed, and an asset is listed only if `SHA256SUMS`
  names it.
- **Directory index.** An `index.json` of the form
  `{"plugins":[{"name":"hello","version":"1.0-1","arch":"amd64","asset_url":"https://example.invalid/hello.tar.gz","sha256":"<64 hex digits>","size":123}]}`.
  An entry with no `arch` applies to every architecture.

The feed document may be at most 8 MiB and a `SHA256SUMS` file 1 MiB. All feeds are
fetched within 10 seconds each. Up to 5 redirects are followed under the same
HTTPS and credentials rule. Results are cached for 15 minutes and refreshed in the
background on that interval; `POST /v1/plugins/feed/refresh` refreshes on demand.
Entries are sorted newest first (Debian ordering), and an install without `version`
takes the newest. A feed archive download has a 300-second deadline and a 200 MiB
limit, and its SHA-256 must match the feed entry. The Tessera runtime bundle
(`tessera-bundle_...`) is embedded in the Tessera plugin archive and is not a feed
asset.

## HTTP API

All routes require a session (or bearer token), and mutating requests need the CSRF
header like every other route. Bodies are JSON unless noted.

| Method and path | Purpose |
|---|---|
| `GET /v1/plugins` | `{plugins, plugins_state_error, feed, arch}`. Each plugin has `name`, `version`, `title`, `icon`, `manifest`, `state` (`installed`, `enabled`, `error` or `broken`), and where relevant `error`, `pending_enable`, `enable_error` and `migration_warning` |
| `GET /v1/plugins/feed` | Cached feed result: `{available, plugins, error?}` |
| `POST /v1/plugins/feed/refresh` | Refresh the feed now |
| `POST /v1/plugins/upload?name=<name>` | Raw archive (`application/octet-stream` or `application/gzip`), at most 200 MiB, 300-second deadline. Streamed, independent of the 4 MiB JSON limit. Reply `{ok, name}` |
| `POST /v1/plugins/install` | `{name, source: "feed"\|"upload", version?, enable?, force?, revision?, expect_impact?, confirm?}`. A caller-supplied `path` is refused. A downgrade needs `force: true` |
| `POST /v1/plugins/<name>/enable`, `/disable` | Enable or disable an installed plugin |
| `POST /v1/plugins/<name>/update` | Install the newest feed version; it must be strictly newer |
| `POST /v1/plugins/<name>/uninstall` | `{purge?, force?}` |
| `POST /v1/plugins/<name>/commands/<command>` | A typed command of a v2 plugin (see above) |
| `POST /v1/plugins/jobs/<job>/cancel` | Cancel one validated lifecycle job. Not blocked by `busy` |
| `POST /v1/plugins/state/reset` | Reset the registry (see above) |
| `GET /plugins/<name>/<path>` | Static UI assets of an enabled plugin |

Enabling, disabling and removing are also typed commands on `/api/v1/commands`
(`set_plugin_enabled`, `remove_plugin`); the web UI uses them and follows the job
through the store's observations. Typed commands use global compare-and-set.
Install, update and upload keep their routes because the device prepares the
archive there. Install replies are `{ok, state, log, ...}` and, on failure,
`{ok: false, state, error, log}` where `state` is `absent` when the refused
install left nothing on the device and `broken` when it left files behind.
Typical errors: 400 for an invalid name or source, 404 when the plugin or the
feed entry is missing, 409 for `busy`, `host_too_old`, `plugins_state_corrupt`,
`revision_conflict`, `confirmation_required` and an attempt to downgrade or update
without a newer version, 413 for an oversized archive and 408 for an upload timeout.
The archive prepared for an install (`<name>-<sha256>.tar.gz`) is deleted once the
install succeeds, and prepared archives older than 15 minutes are removed the next
time one is prepared.

## Privileged commands

A manifest lists the root commands it may request in `priv_commands`. For each name
the archive carries an executable `priv/<command>` and the installer writes the list
to `priv/ALLOWED`. When the process asks for `privilege: {command}`, the host checks
its installed manifest and privd checks the manifest and `ALLOWED` again, then
runs the file as root with:

- no arguments and no standard input (a command that needs arguments needs its own
  typed privileged request, which is a change to NDIMon-R itself);
- the plugin directory as the working directory and a cleared environment
  (`PATH`, `LC_ALL=C`, `HOME=/root`, `NDIMON_PLUGIN_DIR`, `NDIMON_PLUGIN_STATE`);
- the 600-second limit of its system-manager job.

The file must be root-owned, not writable by group or others, and executable. Since
no shell is involved, ship an ELF program or a script whose interpreter exists on
the target. A declared `enable` or `disable` command owns that transition;
otherwise the framework starts or stops the manifest's `services`. Service control
is limited to the units a manifest declares. Privd's closed request list, its
sockets and capabilities are described in the
[daemon README](../control/crates/ndimon-privd/README.md).

## Building and packaging

An archive made with `tar` as shown above is enough. From an NDIMon-R checkout,
`cargo xtask build-plugin --plugin NAME` packages `plugins/NAME/` instead:

```sh
cd control
cargo xtask build-plugin --plugin hello --output ../dist/plugins --release-tag hello-v1
```

It validates the manifest with the rules the device applies, refuses symbolic links,
leaves out `tests`, `node_modules`, `__pycache__` and `.git`, requires the
`process.exec` file to be executable and every `priv_commands` entry to exist as an
executable `priv/<command>`, and writes
`NAME-plugin_VERSION_ARCH.tar.gz`, a catalog entry `NAME-plugin_VERSION_ARCH.tar.gz.json`
and a sorted `SHA256SUMS` covering every `*-plugin_*` archive in the output
directory (default `dist/plugins/`; use a clean directory for a release). A `~` in
the version becomes `-` in the file name. Builds are deterministic for the same
inputs (`SOURCE_DATE_EPOCH` defaults to 0). `ARCH` is `amd64` or `arm64`, the
architectures a release feed matches against the device (there is no `any`): pass
`--arch` to name it, otherwise it is the architecture of the build host (Tessera
takes the one of its bundle). A plugin that does not depend on the architecture is
built and published once per architecture.

## First-party plugin: Tessera

Tessera network audio is a first-party plugin with a native control-plane domain.
Its archive carries no process, no lifecycle scripts and no `priv/` files:
`plugin.json`, `ui.html`, the bundle lock files and the pinned runtime bundle.
Privd verifies the bundle against its lock and runs the bundle's own
`tessera-bundle install` or `uninstall`. When no Tessera bundle is installed under
`/opt/ndimon-r/plugins`, the packaged manifest and fragment in
`/usr/share/ndimon/plugins/tessera` are used for a Tessera the device has accepted.
See [Tessera network audio](plugins/tessera.md).

## Manifest v1 (JavaScript) plugins

Releases up to 1.3.x loaded a plugin's `routes.js` into the Node.js API. That
runtime is gone. A manifest without `process` is the **v1 format**, and in 1.4.0:

- the plugin stays installed with its files and data, and nothing ever loads
  `routes.js` or `backup.js`;
- the catalog entry carries `migration_warning`, and the Plugins page shows
  **needs an update to the out-of-process format**; if the plugin was enabled, its
  state is `error` with the same message;
- it cannot be enabled (HTTP 409 with that message, and no hook runs);
- it can be disabled, which clears the error state, and it can be removed, including
  with `force`;
- installing a v1 archive keeps it inactive. Its root `install.sh` and
  `uninstall.sh` are still run by the installer and uninstaller as before, as
  trusted root code, so install v1 archives only from publishers you trust;
- its backup section is neither exported nor restored; review labels it
  "restorable after plugin support is available";
- `/v1/plugins/<name>/*` router paths answer 404.

A process manifest never falls back to loading JavaScript, including when the process
fails to start. Updating a v1 plugin to a v2 archive of the same name is the
migration path.

## Migrating a JavaScript plugin

1. Build a prebuilt executable that implements the
   [process protocol](#process-protocol) in any language.
2. Add `schema_version: 2` and `process` to the manifest, and move the plugin's
   settings into the bounded `state` schema with a default object. Declare each
   command's `input` and `output` schemas and only the event kinds the process needs.
3. Replace custom Express endpoints with typed commands called through
   `POST /v1/plugins/<name>/commands/<command>`, body
   `{"input": {...}, "expect_revision": N}`. Make `start` idempotent: it delivers the
   complete accepted subtree after start-up, a reconnect or an external restore. An
   absent subtree uses the manifest default; an explicitly saved empty object stays
   empty through persistence, backup and reconnect.
4. Keep `ui.html` and its `NDIMON_PLUGIN_INIT_<name>` initializer; other static
   assets are served below `/plugins/<name>/`.
5. Do not move privileged work into the process. Request an exact `priv_commands`
   name and let the host and privd check the installed allowlist independently.
6. Drop `install.sh`, `uninstall.sh`, `routes.js` and `backup.js`: no install or
   uninstall script runs for a v2 plugin, and its backup is the state subtree.
   Anything that was done by an install script (creating units or groups, copying
   binaries) must already exist on the device or be done by a `priv/<command>`
   you invoke, because the process itself runs unprivileged.
7. Publish the archive and upgrade the installed plugin through the Plugins page.
