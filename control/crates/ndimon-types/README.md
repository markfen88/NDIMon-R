# ndimon-types

The public contract of the control plane: one desired-state document and the
commands, queries, events, origins, impacts and errors that act on it. Every other
crate, the generated TypeScript of the web UI and the JSON Schema for external tools
are derived from these types. Nothing here does I/O.

## What is in it

| Module | Contents |
|---|---|
| `model` | `DesiredState` (`schema_version`, one `revision`, and the domains `device`, `outputs`, `ndi`, `audio`, `network`, `tessera`, `display`, `presets`, `plugins`), their parts (including the Tessera device, clock and flows, and per-plugin state) and defaults |
| `wire` | `Domain`, `Command`/`CommandRequest`/`CommandReply`, `Query`, `Event`, `Origin`, `Impact` with its sealed `ImpactToken`, `ApiError`/`ErrorEnvelope`, `Observed`, `SessionCommand` and the backup document |
| `network` | The network request wrapper, ownership handoff types and the trusted per-connection `NetworkSessionPath` |
| `plugins`, `tessera` | Plugin backup sections and exports; Tessera endpoint naming (PCM names and channel ranges) |
| `validation` | `validate()` plus helpers (`alias_hostname`, `valid_web_hostname`, `valid_dns_search_domain`, `MAX_DNS_SEARCH`) |

`SCHEMA_VERSION` is 1. [LEGACY-MAPPING.md](LEGACY-MAPPING.md) maps every 1.3.x file
and API action onto these types.

## Rules

- Enums and public structs are `#[non_exhaustive]`; tags are serde external tags in
  snake_case and unit variants are plain strings. Fields are added, not changed,
  within a schema version.
- The document, its domains and the commands reject unknown fields.
- `validate()` returns every portable field error at once, path-qualified (for
  example `network.networks[1].dns_search`). It is independent of serialization and
  I/O, so it cannot know whether an EDID mode exists, a plane can rotate, an endpoint
  is online or the host's network can be adopted; the actor checks those against
  observed state before applying.
- Secrets and runtime state never belong in `DesiredState`: password records,
  confirmation tokens and observations are separate. Server-only request fields
  (`network_session`, the activity fence) are skipped by serde, TypeScript and the
  schema.
- `Impact::sealed` digests the impact, the command's arguments and its context into
  an opaque `v1-<sha256>` token. Sending the command again with `expect_impact` applies
  it only if the impact computed on arrival has the same token.

## Features

`serde` (default) adds JSON serialization and the token sealing. `ts` adds ts-rs 11
derives and `schema` adds schemars 1 derives; each enables `serde`. CI also runs
`cargo check -p ndimon-types --no-default-features`, so the crate must build without
serde. Regenerate the TypeScript (`ui/src/generated/`) and the schemas
(`control/schema/`) with `cargo xtask export-types`; never edit them by hand.
