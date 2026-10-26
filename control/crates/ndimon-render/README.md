# ndimon-render

Pure projection of the desired-state document onto the files that other programs
read. It performs no I/O: the state actor calls it to learn the bytes to publish,
and `ndimon-privd` is the process that writes them (atomically, with the declared
owner and mode). The same tables drive drift detection, and `ndimon-state-import`
reads these same files to build a device's first document.

## Ownership

`OWNERSHIP` in `src/lib.rs` is the only domain ownership policy. Every domain of
the document is `Native` at 1.4.0: audio, device, outputs, ndi, network, tessera,
display, presets and plugins. A native domain is changed only by an actor command;
its legacy files are generated, never edited. `Followed` (a domain whose files
other writers still own and the actor re-imports when they change) remains in the
type and is the value of any domain missing from the table; none is followed at
1.4.0. `render` returns only artifacts of native domains.

## Registry and paths

`REGISTRY` dispatches one `DomainRenderer` per domain: a renderer, an optional
importer from the legacy files, an optional drift projection and an optional
predicate that includes or excludes an artifact. `PATHS` declares every
destination with its path ID, domain, owner, group and mode, and its order is the
publication and manifest order.

| Domain | Artifacts | Owner:group, mode |
|---|---|---|
| audio | `/etc/ndimon-audio-settings.json`; `audio_engine` in `/etc/ndimon-device-settings.json` | root:ndimon 0640 |
| device | `/etc/ndimon-device-settings.json`, `/etc/ndimon-about-settings.json` | root:ndimon 0640 |
| outputs | `/etc/ndimon-dec1-settings.json` … `dec8` | root:ndimon 0640 |
| ndi | `/etc/ndimon-rx-settings.json`, `ndimon-find-settings.json`, `ndi-group.json`, `ndi-config.json` | root:ndimon 0640 |
| display | `/etc/ndimon-splash-settings.json`, `ndimon-osd-settings.json` | root:ndimon 0640 |
| presets, plugins | `/etc/ndimon-presets.json`, `/etc/ndimon-plugins.json` | root:ndimon 0640 |
| tessera | `/etc/tessera/config.d/60-ndimon.toml`, `61-ndimon-flows.toml`, `90-probe-flows.toml` (always empty), `/etc/alsa/conf.d/60-ndimon-tessera.conf`, `61-ndimon-tessera-ranges.conf` | root:root 0644 |
| network | `committed.json`, the interface bindings, the pending banner, the service slice drop-ins, `62-ndimon-network.toml` | declared here, published by the network engine |

Tessera artifacts are rendered only when Tessera is configured in the document
and its plugin is installed. Network artifacts are never part of the generic
render (`network::include_artifact` is false): the engine publishes them inside
its own transaction, so an unrelated actor write cannot replace staged backend
files, bindings or recovery images. Authentication, journals and observed files
(sources, status, catalogs) are not rendered at all. Tessera's importer lives in
`ndimon-state` because it must read the daemon's effective settings, so its
renderer entry has no import slot.

## Shared files

Some files are written by more than one domain. `/etc/ndimon-device-settings.json`
holds device keys and the audio engine; the per-output `dec` files hold outputs,
NDI and audio fields. `MIXED_PATHS` lists them. For these, `ndimon-privd` rereads
the file under the same lock that guards other writers, and the renderer replaces
only its own top-level keys with `replace_key`: every byte outside the replaced
value, including unknown keys, spacing and escape spelling, is preserved. Mixed
paths are composed in `REGISTRY` order and every artifact for a mixed path carries
the final bytes, so publication order cannot erase a peer's value. A domain's drift
projection compares only its own keys.

## Format

JSON written for the C++ readers is compact and has no provenance keys.
`node_json` serializes integral floating-point numbers as integers, the form those
readers and earlier releases used. Unchanged imported audio keeps its input text and
only replaces `revision`. Provenance lives beside the state in `rendered.json`,
which `ndimon-state` writes (path, document revision, SHA-256, owner, group and
mode of the bytes that were published); each artifact carries the revision at
which its domain last changed.

## Entry points

`render` and `render_with_revisions` produce the native artifacts and take the
last-read text of the mixed files. `render_path` renders one declared path, which
also previews a followed domain. `drift_bytes` yields the expected and observed
bytes after the domain's drift projection. `manifest` summarizes artifacts for
`rendered.json`. `legacy::import_json` builds a document from the legacy files; the
mapping it follows is in [LEGACY-MAPPING.md](../ndimon-types/LEGACY-MAPPING.md).
