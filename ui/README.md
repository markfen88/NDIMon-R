# Browser UI

The web UI is a single page written in strict TypeScript. Node.js 22 builds it on
the build host and in CI only: the device serves committed files and
never runs Node. `ndimon-control` serves them from `/usr/share/ndimon/public`.

| Path | Role |
|---|---|
| `ui/src/` | TypeScript sources |
| `ui/src/generated/` | Types exported from the Rust contract; never edit by hand |
| `api/public/` | Committed build output and `index.html`. The directory name is historical: there is no API process, and `api/` holds only the manifest and lockfile that pin Bootstrap and Bootstrap Icons |
| `api/public/vendor/` | Bootstrap files fetched by `cargo xtask build-vendor`; not committed |

## How the page is assembled

`index.html` loads four scripts in this order: Bootstrap, `ui/csrf.js`,
`network-settings.js` and `ui/page.js`.

- `csrf.ts` wraps `window.fetch` so that every same-origin request that is not a
  GET, HEAD or OPTIONS carries the session's `X-CSRF-Token`. It runs first, so the
  network controller and plugin fragments are covered too.
- `network-settings.ts` is the network profile editor (its own bundle, built
  without the extra strict-mode directive so it behaves like a classic script).
- `page.ts` imports the section modules in page order and `legacy-globals.ts`
  exposes their functions as globals, because the markup uses inline handlers and
  plugin fragments call `window.NDIMON`. The sections are `auth`, `navigation`,
  `sse`, `initial-load`, `ui-update`, `settings`, `actions`, `presets`,
  `services`, `watchdog`, `scheduled-reboot`, `ndi-groups`,
  `backup-restore-network`, `software-update`, `plugins` and a few helpers.
- `api.ts` and `store.ts` are also emitted as ES modules (`api/public/ui/api.js`,
  `store.js`) so the unit tests can import them.

## Data flow

- **Typed client (`api.ts`).** `ApiClient` signs in and out
  (`/api/v1/session`), reads `GET /api/v1/state`, `/state/{domain}` and
  `/observed`, and writes with `POST /api/v1/commands`. It sends `X-CSRF-Token`
  on every write. A failed request throws `ApiFailure` carrying the generated
  `ErrorEnvelope`; code branches on the error kind, never on the message. A
  `409 confirmation_required` reply is passed to the caller's confirm callback and,
  if accepted, the same command is resent with `expect_impact` set to the impact
  token. At most two such retries are made, so an impact that keeps changing is
  shown again each time and then refused.
- **Store (`store.ts`).** `StateStore` keeps the desired-state document, the
  observations (`Observed`), the decoder and finder status and a telemetry sample.
  It opens one `EventSource` on `/api/v1/events`. A `state_changed` event for the
  next revision re-reads only the changed domains and publishes them together; a
  gap, a mixed revision or a `resync` event re-reads everything. Older revisions
  are ignored, requests are queued so a slow reply cannot overwrite a newer one,
  and a dropped stream is reopened after five seconds. Subscribers hear the
  changed domains.
- **Live view (`live.ts`).** `liveView()` combines the store's desired state with
  the decoder and finder status and the telemetry sample. It is derived on demand
  and never kept, so no page holds an old copy. Hardware observations decide which
  outputs exist; saved settings for absent outputs draw no tiles.
- **Writes (`native-page.ts`).** `nativeCommand` sends a command at the revision
  the editor loaded and shows the device's impact text in the confirmation dialog
  before resending. It maps typed refusals (`revision_conflict`, `locked`,
  `invalid`, `unsupported`, `apply_failed`) to the messages the cards show. Each
  editor remembers the revision it was filled from, so a save made against a
  document that changed meanwhile is refused with `revision_conflict` and offers
  a reload. A card with unsaved edits is not redrawn from the store.
- **Telemetry.** CPU, memory, temperatures, uptime and plugin status are sampled
  from `/api/status` every 1.5 s and handed to the store; they touch no desired or
  observed state.
- **Compatibility routes.** Calls that have no native query or command yet use
  the legacy routes: `/v1/Network/profiles` and its preview (live addresses and
  the profile editor's draft shape), the token-only `/api/net-pending`,
  `/api/net-confirm` and `/api/net-revert`, `/api/services`, `/api/health`,
  `/api/watchdog-stats`, `/v1/AboutMe`, `/v1/DeviceSettings/ntp` and
  `/reboot-schedule`, `/v1/Backup/export`, `inspect`, `apply` and `snapshots`,
  `/v1/Splash/uploadLogo`, and the plugin feed, upload and install routes under
  `/v1/plugins`. Every other write is a typed command. The control plane serves
  the legacy routes natively, and scripts may keep using them.

## Generated types

`src/generated/` is written by `cargo xtask export-types` from the Rust types in
`control/crates/ndimon-types`, together with the JSON Schema in `control/schema/`.
Commands, impacts, events, errors and every desired-state domain import these
types, so a contract change that the UI does not follow fails `tsc`. Revisions
are JSON numbers; do not do arithmetic on them outside JavaScript's exact integer
range.

## Build and check

From `control/` on a build host with Node 22:

```sh
cargo xtask build-ui     # npm ci, npm run build, then the pinned Bootstrap files
cargo xtask check-ui     # npm run check (tsc --noEmit); dependencies must be installed
cargo xtask build-vendor # only the Bootstrap and Bootstrap Icons files
```

`build-ui` and `check-ui` start npm with fixed arguments and no shell.
`build-vendor` needs no npm: it downloads the tarballs named in
`api/package-lock.json`, verifies their sha512 `integrity` and unpacks only the
files the page loads into `api/public/vendor/`. `cargo xtask install` and
`cargo xtask build-deb` call it, so a device that builds its own packages needs
neither Node nor npm. Commit the rebuilt `api/public` output with the source
change; CI rebuilds and refuses any difference or new file there.
