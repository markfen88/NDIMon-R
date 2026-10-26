# ndimon-compat

The legacy HTTP surface of NDIMon-R 1.3.x, kept as thin adapters over the state
actor. Scripts, the Tessera page fragment and plugins written against the `/v1/...`
URLs keep working: each route parses the historical request shape, compiles it to
typed commands and queries on `ndimon-state`, and renders the historical response
envelope. The adapters hold no state of their own and never write files or talk to
a daemon directly.

## Use

`legacy_router::<S>()` returns an axum `Router<S>` for any state `S` from which a
`StateHandle` can be extracted (`StateHandle: FromRef<S>`). `ndimon-control` merges
it into its authenticated router. The adapters do not authenticate and do not check
CSRF, Origin or Host: the front door's middleware does, and supplies the `UiSession`
extension (the session's public ID) that becomes the origin of each command. Without
that extension an adapter labels its commands `legacy-http`. Bearer credentials are
never used for attribution. `audio_router` can be mounted alone under `/v1/Audio`.

| Module | URLs |
|---|---|
| `audio` | `/v1/Audio/*` (endpoints, outputs, engine, settings, source and sink patches, status) |
| `outputs` | `NDIDecode` and `VideoOutput` (connect, decode setup, modes, resolution, scale, rotation), `DeviceSettings/output-alias` |
| `ndi` | `NDIFinder`, and the receive transport |
| `device`, `system`, `status` | `DeviceSettings/*`, `AboutMe`, `ApiSettings`, `/v1/System/*` and `/api/v1/system/*`, the `NDIEncoder` stubs (404) |
| `display` | `/v1/Splash/*` (config, OSD, logo upload, preview) |
| `presets` | `/v1/Presets/*` |
| `backup` | `/v1/Backup/*` (export, inspect, apply, snapshots, undo) |
| `network` | `/v1/Network/*`, the token-only `/api/net-pending`, `/api/net-confirm`, `/api/net-revert` and the handoff routes |
| `plugins` | `/v1/plugins/*` lifecycle, feed, upload, job cancel and process-plugin commands, and the static `/plugins/{name}/...` assets |
| `tessera` | `/v1/plugins/tessera/*`, the unchanged routes of the Tessera page |

## Behaviour that matters

- A write carries the domain revision it was read at (`revision`) and is compared
  with that domain only, so an unrelated change does not invalidate it. A stale
  revision answers 409 with the legacy message. Disruptive changes answer
  `confirmation_required` with the impact and are resubmitted with
  `expect_impact` or `confirm`; these are request controls and never saved fields.
- Legacy JSON is rendered in the historical field order with compact numbers
  (integral floats as integers), so existing clients see the same bytes.
- System actions (reboot, restart-stack, update) set blind confirmation on behalf of
  the already-confirmed legacy UI request; native commands always require the impact
  token or `confirm`. Updates are refused unless a trusted release key exists. With
  one, `GET /v1/System/version` also reports `latest_version` and `updates_available`
  from the update agent's cached release lookup.
- Saving an output's audio (`PUT /v1/Audio/outputs/:ch`) selects engine v2 in the same
  command (routes and engine are one change); while v1 runs the decoder restart needs
  confirmation.
- Independent VLAN `PUT`/`DELETE` are refused with 409; the single-address
  `/v1/Network/apply` is a wrapper over a complete profile transaction. Network
  changes are refused with `backend_not_certified` unless the backend is listed in
  `/etc/ndimon-network-certified-backends.json` (the `ndimon-privd` package installs
  it listing all four; an operator may withdraw one).
- Manifest-v1 JavaScript plugins get no router: only manifest-v2 process plugins
  have `/v1/plugins/{name}/commands/{command}` routes.
