# ndimon-privd-proto

The closed protocol between `ndimon-control` and `ndimon-privd`, plus the framing
and an async client. It is the only definition of what the root daemon will do: a
`Request` enum of typed variants, each built from validated newtypes. No request
carries a path, an executable, a shell fragment, free-form arguments or arbitrary
file content. Two content-bearing variants remain for fixed targets only
(`WriteLegacyConfig` and `WriteLegacyLogo`, which name one file of a closed list and
are bounded to 4 MiB).

## Wire

One request and one reply per connection: a big-endian `u32` byte length (1 to 8 MiB,
`MAX_FRAME_BYTES`) followed by JSON.

```json
{"v":1,"id":7,"req":{"set_hostname":{"name":"studio-1"}}}
{"v":1,"id":7,"ok":{"stdout":"","stderr":""}}
{"v":1,"id":7,"err":{"invalid":{"field":"name","reason":"invalid hostname"}}}
```

Tags and fields are snake_case with serde's external tag; unit variants are strings
(`"reboot"`). `VERSION` is 1, unknown fields are rejected, and a reply holds exactly
one of `ok` and `err`. Errors are `invalid`, `permission_denied`, `busy`, `failed`
(with the exit code when a program failed), `unsupported` or `internal`. The control
socket is `CONTROL_SOCKET` (`/run/ndimon-privd/control.sock`); `ndimon-control` is its
only caller.

## What it contains

- `Request` and `Reply`/`ActionOutput`/`Error`; `codec` (`encode`, `read`, `write` and,
  with the `client` feature, `read_async`/`write_async`).
- `values`: validated types such as `Hostname`, `NtpServer`, `ReleaseVersion` (Debian
  version grammar), `PluginName`, `PluginUnit`, `ArchiveId`, `ChannelCount`, with the
  enums `ManagedUnit`, `UnitAction`, `LegacyConfigFile`, `LogoFile`, `RebootSchedule`,
  `CpuProfile`.
- The request groups: system (`SetHostname`, `SetTimeSync`, `SetRebootSchedule`,
  `Reboot`, `RestartStack`, `ServiceControl`, `CpuTuning`), artifacts
  (`RenderArtifacts { revision }`), updates (`InstallUpdate { version, force }` and the read-only `CheckUpdate`;
  `StartUpdate` is a tombstone that is always refused), the network (`NetworkNative {
  call }` for the native engine, with its trusted `NetworkSocketContext`, plus the
  earlier flat `Network*` variants), plugins (`PluginInspect`, `PluginInstall`,
  `PluginUninstall`, `PluginExec`, `PluginServiceControl`, `PluginRuntimeStart/Stop/
  Status`, `PluginJobStatus/Cancel`, `PluginResetState`) and Tessera (`TesseraWriteFlows`,
  `TesseraWriteAlsa`, `TesseraWritePcms`, `TesseraBundle`, `TesseraService`,
  `TesseraEnable`, `TesseraDisable`).
- `plugin`: the shared manifest types (`Manifest`, the manifest-v2 `Process`,
  `StateSchema`, `CommandSchema`), the bounded JSON Schema subset
  (`validate_schema`, `validate_value`), and the name, service-unit, group and path
  rules (`permitted_service`, `permitted_group`, `relative_path`, `native_lifecycle`)
  used by the native host, privd and `cargo xtask build-plugin`.
- With the `client` feature: `client::Client` (`request` returns when a job is
  accepted; `execute` also waits for a system-manager job's durable result by polling
  `PluginJobStatus`, within the client's timeout, 30 seconds by default).

## Boundaries

This crate validates shape and bounds only. Authorization (which socket may send
which request) and execution belong to `ndimon-privd`; a new request needs a handler
there, because dispatch is an exhaustive match. Adding a variant changes the wire
contract, so the control and privd packages are upgraded together.
