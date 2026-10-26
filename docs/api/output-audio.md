# Per-output audio

For the GUI workflow, see [Outputs → Advanced → Audio](../../README.md#per-output-audio-experimental).
Device, flow and reusable channel-name setup stays in the
[Tessera plugin](../plugins/tessera.md). Network/VLAN assignment is configured
separately through [network profiles](../network-vlans.md), not in an output
audio graph.

Per-output audio needs audio engine v2, which is experimental: sustained clock
stability and external hardware interoperability are not certified. The engine
itself is process-wide.

## Where audio settings live

Audio is one domain of the device's desired-state document, owned by the state
actor in `ndimon-control`. The domain holds the engine choice (`v1` or `v2`) and
the graph together: sources, sinks, per-output choices, routes and the video
delay. Nothing else writes it. Every route below, the web UI, `host` helpers of
plugins and the native commands end up as the same typed command on the actor,
which validates the whole candidate, plans the effect, asks for confirmation if
it disrupts something, saves it and applies it. A failed apply restores the
previous graph and live audio.

`/etc/ndimon-audio-settings.json` is a generated projection of that domain: the
actor renders it through `ndimon-privd` and then tells the decoder to reload it
(`reload_audio` on the decoder's IPC socket). Do not edit it; a change made
outside the API is reported as drift and replaced on the next render. A change
of engine or of the v1 master or period size restarts the decoder instead.

## Choosing the engine

The engine stays process-wide, and routes and engine are one change. Saving an
output's audio selects engine v2 in the same command:

- While v1 is running, the save would restart the decoder, so it is refused with
  `409 confirmation_required` and an impact that says so ("Switch to audio engine
  v2…", "Restart the decoder; video and audio pause briefly"). Resend the same
  request with `expect_impact` set to the impact's `token` (or with
  `confirm:true`) to apply it. The UI shows the impact text before it does.
- While v2 is running, a save is applied live and restarts nothing.
- `POST /v1/Audio/engine` accepts exactly `{audio_engine:"v1"|"v2",confirm:true}`
  and nothing else. Selecting v1 returns to the original decoder audio path and
  keeps every saved choice; routes of enabled outputs become inactive. The
  decoder restarts when the engine changes. `POST /v1/DeviceSettings/audioEngine`
  selects the engine the same way through the device-settings surface.

An output with an empty input or destination list is saved with `enabled:false`.

## Reading

- `GET /v1/Audio/endpoints` returns `{revision,inputs:[],destinations:[]}`. Each
  endpoint has an opaque `id` (for example `ndi-1`, `hdmi-1`,
  `tessera-rx-1-2`), `label`, `provider` (`ndi`, `hdmi`, `tessera` or `unknown`),
  `channels`, `rate`, `ready`, and optional `reason`, `mapping` and stable
  one-based `output`. NDI inputs bind the specified receiver. HDMI destinations
  bind the specified connector. Tessera publishes its named main-device full range
  (at most 16 channels) and individual mono channels; its labels are edited in the
  plugin settings. A mono input feeds both stereo channels; selecting two monos is
  a mix, not a stereo pair. Saved choices whose provider is gone remain visible as
  `unknown` and not ready.
- `GET /v1/Audio/outputs/:ch` returns
  `{revision,enabled,inputs:[{endpoint,gain_db,delay_ms}],destinations:[{endpoint,delay_ms}]}`
  (plus `legacy_matrix` and a `warning` when per-destination gains from an older
  configuration are preserved).
- `GET /v1/Audio/engine` returns `{audio_engine,experimental:true}`;
  `GET /v1/DeviceSettings/audioEngine` returns `{audio_engine}`.
- `GET /v1/Audio/settings` returns the legacy-shaped settings document, a
  projection of the graph with the audio revision.
- `GET /v1/Audio/status` returns the decoder's audio status: master (`timer`
  when it fell back to the timer), rate, period size, per-source and per-sink
  readiness and xruns, drift compensation, per-route delay and capacity, and
  `memory_bytes` against `memory_budget_bytes`. It answers 503 when the decoder
  cannot be reached.
- Natively, `GET /api/v1/state/audio` returns the domain with the document
  `revision` and the `domain_revision`.

## Writing

`PUT /v1/Audio/outputs/:ch` takes the shape above plus the current `revision`.
Output IDs are stable one-based `1..8`. Optional body keys are
`normalize_matrix`, `confirm` and `expect_impact`; any other key is refused.

The same change as a native command, on `POST /api/v1/commands` with
`expect_revision` set to the document revision:

```json
{"command":{"put_output_audio":{"output":1,"normalize_matrix":false,
  "audio":{"enabled":true,"primary":false,"legacy_matrix":false,
    "inputs":[{"endpoint":"ndi-1","gain_db":0,"delay_ms":0}],
    "destinations":[{"endpoint":"hdmi-1","delay_ms":0}],
    "definitions":{"sources":{},"sinks":{}}}}},
 "expect_revision":42}
```

| Native command | Effect |
|---|---|
| `put_output_audio` | One output's choices; compiles the cartesian product into routes and selects engine v2 |
| `set_audio_engine` | `{engine:"v1"\|"v2"}`; restarts the decoder, so it needs confirmation |
| `put_audio` | The whole audio domain (graph, settings, engine) |
| `put_audio_source`, `put_audio_sink`, `remove_audio_endpoints` | Single endpoint definitions; the reserved ids `ndi` and `hdmi` cannot be removed |

Revisions differ by surface. Native commands compare `expect_revision` with the
global document revision. The `/v1/Audio` and `/v1/DeviceSettings/audioEngine`
routes compare the `revision` in the body with the audio domain's own revision,
which an unrelated change (a splash colour, a network profile) does not move. A
stale value answers `409 revision_conflict` with the current `revision`; reload
before saving. A no-op command changes no revision.

A disruptive command without confirmation answers
`409 confirmation_required` with `impact: {token, scope, lines, restarts, interruptions}`.
Resend it with `expect_impact: <token>`; if the impact changed meanwhile, the
token no longer matches and the new impact is returned. `confirm:true` is a
blind confirmation for scripts and does not check the token. A stale token is
refused even with `confirm:true`. While a network change waits for its Keep
decision, every other write, including audio, is refused with `locked`.

## Rules

Each active input is routed to each active destination. Individual delays are
integer `0..5000` ms, gains `-60..12` dB (`-60` mutes); the input and destination
delay of an active pair must add up to at most 5000 ms. The graph holds at most
8 outputs, 32 inputs and 32 destinations per output, and 32 sources and sinks.
Empty selections mute the output. Muted selections retain their drafts without
opening devices.

One canonical physical destination is opened once and mixes different receiver
sources from multiple outputs. Duplicate active contributions from the same
physical source to the same physical destination are rejected, and so are
overlapping range choices in one input list and separate overlapping physical
writers. Disabled drafts do not contribute.

Known endpoints retain active routes during temporary disconnection, so receiver
and connector recovery can resume their audio. A known offline endpoint stays
configured with runtime readiness false; the decoder retries failed ALSA opens
every two seconds without restarting. Unknown-provider choices remain pending as
metadata.

| Error | Meaning |
|---|---|
| `409 revision_conflict` | The revision sent is not the current one |
| `409 confirmation_required` | The change restarts or interrupts something; resend with the impact token |
| `409` with `restart_required: true` | The decoder could not reuse a retained physical PCM. The previous file and live graph are kept; select v1, save, then explicitly select v2 |
| `409 legacy_edit_unsupported` | A primary-output edit through the legacy settings routes cannot be represented safely; use Outputs (natively this is `locked` on that field) |
| `409 matrix_confirmation_required` | The edit would replace preserved per-destination gains or mutes; the reply carries before/after contributions. Resubmit with `normalize_matrix:true` |
| `400` with the errors listed | Validation failed; each error names its field (`effective delay exceeds 5000 ms`, overlapping channel selections, duplicate endpoint, shared endpoint geometry differs, …) |
| `400` with `error_code: "resource_exhausted"` | The graph needs more buffer memory than `memory_budget_mib` (default 64 MiB, `1..512`); the reply carries `requested_bytes` and `budget_bytes` |
| `503` | The decoder is offline or did not answer in time |

Legacy settings and plugin helpers (`PUT /v1/Audio/settings`,
`PATCH /v1/Audio/sources/:id` and `/sinks/:id`) project the primary output only.
Their writes preserve other outputs and require the audio revision. An advanced
primary selection that cannot be represented returns `legacy_edit_unsupported`,
and a changed legacy destination matrix returns `matrix_confirmation_required`.
Such writes carry `confirm` and `expect_impact` as request controls; these are
never stored as audio settings.

A restore of a backup that contains audio goes through the same validation,
planning and confirmation as a command.
