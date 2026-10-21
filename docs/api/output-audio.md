# Per-output audio

For the GUI workflow, see [Outputs → Advanced → Audio](../../README.md#per-output-audio-experimental).
Device, flow and reusable channel-name setup stays in the
[Tessera plugin](../plugins/tessera.md). Network/VLAN assignment is configured
separately through [network profiles](../network-vlans.md), not in an output
audio graph.

The audio engine stays process-wide. Saving an output graph never selects engine v2 or restarts the decoder. Engine v2 remains experimental; sustained clock stability and external hardware interoperability are not certified.

`GET /v1/Audio/endpoints` returns `{revision,inputs:[],destinations:[]}`. Each endpoint has an opaque `id`, `label`, `provider`, `channels`, `rate`, `ready`, and optional `reason`, `mapping`, and stable one-based `output`. NDI inputs bind the specified receiver. HDMI destinations bind the specified connector. Tessera publishes its named main-device full range (at most16 channels) and individual mono channels. Mono input feeds both stereo channels; selecting two monos is a mix, not a stereo pair. Tessera labels are edited in its plugin settings. Saved unavailable choices remain visible.

`GET /v1/Audio/outputs/:ch` returns `{revision,enabled,inputs:[{endpoint,gain_db,delay_ms}],destinations:[{endpoint,delay_ms}]}`. PUT the same shape with the current global `revision`. A stale revision returns409 `revision_conflict`; reload before saving. Output IDs are stable one-based1..8. Individual delays are integer0..5000ms, gains -60..12dB; active input+destination delay must not exceed5000ms. Empty selections mute the output. Muted selections retain their drafts without opening devices. Known endpoints retain active routes during temporary disconnection, so receiver and connector recovery can resume their audio. Known offline endpoints can remain configured with runtime readiness false; the control thread retries failed ALSA opens every two seconds without restarting the process. Structural busy or invalid applies still preserve the prior graph. Unknown-provider orphan choices remain pending in metadata.

Each active input is routed to each active destination. One canonical physical destination is opened once and mixes different receiver sources from multiple outputs. Duplicate active contributions from the same physical source to the same physical destination are rejected. Overlapping range choices in one input list and separate overlapping physical writers are rejected. Disabled drafts do not contribute. Canonical graphs contain concrete PCM/range/channel metadata so the decoder can start without Node.

`GET /v1/Audio/engine` reports `{audio_engine,experimental:true}`. `POST /v1/Audio/engine` accepts `{audio_engine:"v2",confirm:true}` or explicit rollback `"v1"`; changing the engine restarts the decoder. Returning to v1 restores the original decoder audio path. Changes that cannot reuse a retained physical PCM return409 `restart_required`, preserving the previous file and live graph; select v1, save, then explicitly select v2.

Legacy settings and plugin helpers project the primary output. Writes preserve other outputs and require revision CAS. Advanced primary selections that cannot be represented safely return409 `legacy_edit_unsupported`; use Outputs instead. Legacy destination matrices keep their gains and hard mutes across delay and disable/enable edits. Changing their mix returns409 `matrix_confirmation_required` with before/after impact until explicitly resubmitted with `normalize_matrix:true`.

All API graph writes share a serialized queue; plugin lifecycle operations acquire an exclusion gate before starting a separate root CLI. A running or unresolved lifecycle job blocks API graph writes. Manually invoking the root CLI outside this framework is an administrative operation and must not overlap API edits. Root PCM preparation writes a bounded union of previous and requested ranges, preserving old aliases through failed prepare/apply rollback. JSON and live audio are restored on native apply failure; structured native resource errors retain requested/budget byte counts.
