# Legacy import and rendering contract (schema 1)

This is the contract between the 1.3.x `/etc` files and the desired-state
document: `ndimon-state-import` reads the files listed here into the document, and
`ndimon-render` renders them back. Paths below are in `DesiredState`. `N` is a
stable **1-based** channel, 1–8; the IPC/DRM zero-based index is never persisted.
Missing values use the Rust defaults (from the shipped templates and the named
readers). Only known keys are imported; unknown keys are reported and their original
text is retained rather than silently lost. The importer keeps each original file
as `<filename>.pre-control`. No renderer introduces a second writer for a setting.

| Legacy file | Key | Document path / conversion |
|---|---|---|
| `/etc/ndimon-device-settings.json` | `mode` | `device.mode` (`decode` / `encode`; encoder operations remain unsupported) |
| same | `videooutput` | Legacy primary fallback for `outputs.1.video.resolution` **only if dec1 file is absent** (`Config::get_output`); `hdmi`, empty and `auto` mean `auto` |
| same | `ndi_recv_name` | `device.ndi_alias`; trim, preserve case; OS hostname is derived by `alias_hostname()` when alias is changed |
| same | `audio_engine` | `audio.engine`; absent / non-v2 legacy value means `v1` |
| same | `decode_mode`, `watchdog_mode`, `ntp_server` | `device.decode_mode`, `device.watchdog_mode`, `device.ntp_server` |
| same | `reboot_schedule_enabled`, `reboot_schedule_time`, `reboot_schedule_days[]` | `device.reboot_schedule.enabled`, `.time`, `.days[]`; days normalized to distinct Sun-first weekday names (long names accepted); malformed/empty imported lists and invalid imported times use the legacy effective defaults |
| same | `AnalogAudioInGain`, `AnalogAudioOutGain` | `device.analog_audio_in_gain`, `.analog_audio_out_gain`; parse decimal strings (template `"50"`); retained compatibility settings, not decoder engine gain |
| same | `plugin_feeds[]`, `allow_insecure_feeds` | `device.plugin_feeds[]`, `.allow_insecure_feeds`; missing = empty / false |
| same | `web_allowed_hosts[]` | `device.web_allowed_hosts[]`; missing = empty; at most 16 strict DNS names, normalized to lower case, no IP literals or wildcards |
| `/etc/ndimon-about-settings.json` | `GroupName`, `MACAddress`, `device_name`, `host_name` | `device.about.group_name`, `.mac_address`, `.device_name`, `.host_name`; these are editable API metadata, not observed hardware identity |
| OS hostname | hostname | `device.hostname`; absent = unmanaged, not a fabricated device identity |
| `/etc/ndimon-decN-settings.json` | `ChNum` | `outputs` map key N, taken from filename; report a conflicting embedded channel, do not redirect the file |
| same | `SourceName`, `SourceIP` | `outputs.N.source.{name,endpoint}`; empty / `None` name means `null`; endpoint is empty or canonical IPv4 with optional `:port` |
| same | `SourceSelection` | `outputs.N.source_selection` (default `NDI`) |
| same | `videooutput` | `outputs.N.video.resolution`: empty / `auto` → `"auto"`; `WIDTHxHEIGHT[@HZ]` → `{"mode":{"width","height","refresh_hz"}}`; absent refresh = 0 (pick preferred matching resolution). Report unsupported strings; never silently choose another mode |
| same | `ScaleMode`, `rotation`, `output_alias` | `outputs.N.video.scale_mode`, `.rotation`, `outputs.N.alias` |
| same (global decoder reads dec1) | `NDIAudio` | `audio.outputs.1.enabled`: `NDIAudioEn` / `NDIAudioDis`; also controls the v1 primary render. For later channels the v2 output enable is authoritative |
| same | `ScreenSaverMode` | `outputs.N.screensaver`: `SplashSS`→`splash`, `BlackSS`→`black`, `CaptureSS`→`capture` |
| same (global decoder reads dec1) | `TallyMode`, `ColorSpace` | `ndi.tally` (`TallyOn`→`on`, `TallyOff`→`off`, `VideoMode`→`video`), `ndi.color_space` (`YUV`→`yuv`, `RGB`→`rgb`) |
| `/etc/ndimon-rx-settings.json` | `Rxpm` | `ndi.receive`: RUDP→`rudp`, Multicast→`multicast`, M-TCP→`multi_tcp`, UDP→`udp`, TCP→`tcp` |
| same | `Rxpm_user_set` | Deliberately dropped migration marker **after** applying the 1.3.x rule: untouched TCP without `true` becomes RUDP. Explicit TCP survives. Renderer always writes marker `true` |
| `/etc/ndimon-find-settings.json` | `NDIDisServIP` | `ndi.discovery_servers[]`; split comma-separated entries, trim; empty = no server |
| same | `NDIDisServ` | Dropped redundant enable flag; renderer derives it from IP presence, as 1.3.x did |
| `/etc/ndi-group.json` | `ndi_groups` | `ndi.groups[]`; split comma-separated groups, preserve **case and order**, blank legacy string defaults to `public`. Never lowercase or merge `Production` with `production` |
| `/etc/ndi-config.json` | root JSON string | `ndi.off_subnet_sources[]`; split comma-separated IPv4 values; empty = [] |
| `/etc/ndimon-audio-settings.json` | `version` | Import discriminator 1/2, dropped after normalization; render version 2 graph for v2, v1 projection for v1. Schema version belongs to the whole document |
| same | `revision` | Dropped domain counter after import; one top-level `revision` whose first value is 0; never take max of unrelated counters as a CAS revision |
| same | `engine.enabled`, `.rate`, `.period_frames`, `.master`, `.memory_budget_mib` | `audio.settings` with identical keys (memory budget default 64 MiB from C++); **not** `audio.engine` (which selects v1/v2) |
| same | `video_delay_ms` | `audio.video_delay_ms`, primary only |
| same v1 | `sources.ID.type`, `.enabled`, `.channels`, `.alsa_pcm` | `audio.sources.ID.kind`, `.enabled`, `.channels`, `.alsa_pcm`; NDI has no PCM; derive `.output=primary`, `.endpoint=ndi-N`; retain arbitrary ALSA PCM and ID |
| same v1 | `sources.ID.gain_db`, `.delay_ms` | `audio.sources.ID.gain_db`, `.delay_ms`; retained even disabled |
| same v1 | `sinks.ID.type` | Dropped constant `alsa` (only possible sink type) |
| same v1 | `sinks.ID.enabled`, `.channels`, `.alsa_pcm`, `.delay_ms`, `.mix.SOURCE` | `audio.sinks.ID` identical fields, including the entire `mix` map. Unknown mix source IDs are retained (legacy C++ warns and ignores them) |
| same v1 | entire source/sink matrix | Build primary `audio.outputs.N` choices plus one `audio.routes.ID` per matrix contribution. Effective gain is -60 if either component is <= -60, otherwise their sum; effective delay is the sum. Retain inactive routes and node factors. V1 effective values can exceed v2 limits: preserve under v1; refuse v2 activation until normalized, never clamp silently |
| same v1 | `ndi`, `hdmi`, `net-in`, `net-out` aliases | Keep IDs for exact v1 projection. `ndi` is NDI primary; `hdmi` PCM `auto` resolves to actual primary. `ndimon_net_in/out` map resources to `tessera:main:rx/tx`, full range starting 1, using the same nodes; no parallel v1 settings document |
| same v2 | `sources.ID.type`, `.enabled`, `.channels`, `.output`, `.alsa_pcm`, `.endpoint`, `.resource`, `.channel_start`, `.channel_count` | `audio.sources.ID.kind` and identical remaining keys; missing range defaults to start 1/count channels; v2 node gain/delay defaults 0 |
| same v2 | `sinks.ID.type`, remaining endpoint keys | Drop constant type; `audio.sinks.ID` identical keys. Missing delay/mix defaults 0/{} |
| same v2 | `routes.ID.{output,source,sink,enabled,gain_db,delay_ms,mapping}` | `audio.routes.ID` identical keys; absent mapping = `default`; concrete routes remain runtime authority |
| same v2 | `outputs.N.{enabled,primary,inputs[],destinations[],legacy_matrix}` | `audio.outputs.N` identical keys; inputs contain `endpoint,gain_db,delay_ms`; destinations contain `endpoint,delay_ms` |
| same v2 | `outputs.N.definitions.{sources,sinks}` | `audio.outputs.N.definitions` typed nodes with the same conversions as above; retain disabled/orphan definitions, never require online hardware |
| same v2 | `outputs.N.legacy.settings` | Resolve v1 settings through `legacy.sources/sinks` into typed `audio.outputs.N.definitions.sources/sinks[ALIAS]`, retaining original alias IDs, PCM, enabled flags, channel geometry, gain/delay/mix factors (including disabled settings). Fill identity/resource fields from the canonical node. Preserve concrete routes and `legacy_matrix`; engine/video delay come from the authoritative top-level graph. No duplicate settings document or engine model |
| same v2 | `outputs.N.legacy.sources`, `.sinks` | The typed per-output definitions retain aliases as keys; canonical target identity is retained in endpoint/resource/output/channel range fields. Multiple aliases for one physical endpoint remain distinct draft definitions, while active graph nodes stay canonical. Renderer resolves v1 aliases from these definitions instead of inventing names |
| same v2 | `outputs.N.definitions.*` disabled node metadata | Retain all supported endpoint fields; this is a draft of the same graph, not another engine model |
| `/etc/ndimon-splash-settings.json` | `bg_idle`, `bg_live`, `accent_idle`, `accent_live`, `logo_x_pct`, `logo_y_pct`, `logo_w_pct`, `text_idle`, `text_live`, `text_height_pct`, `backdrop`, `show_signal_text`, `show_device_name`, `show_device_url`, `show_sources_available` | `display.splash` with identical keys; defaults are the 1.3.x splash defaults; percentages 0–100 (the clamp the 1.3.x API applied, not the older comment bounds in the C++ source) |
| same | `logo_path` | Import only empty or `/etc/ndi-splash-logo.(png|jpg|jpeg)`; content-address PNG/JPEG bytes → `display.splash.logo.{sha256,media_type}`. Renderer chooses managed location, never accepts an arbitrary path |
| same | `show_box` | Dropped obsolete decoration (1.3.x already ignored it); `backdrop` supersedes it |
| `/etc/ndimon-osd-settings.json` | `enabled`, `text` | `display.osd.enabled`, `.text` (128 UTF-16 code units, the 1.3.x limit) |
| `/etc/ndimon-presets.json` | `presets[].name`, `.source`, `.ip` | `presets[NAME].source.{name,endpoint}`; `position` preserves legacy array order; max 32 named presets, 64 UTF-16 units per name |
| `/etc/ndimon-plugins.json` | `NAME.installed`, `.enabled`, `.version` | `plugins.NAME` identical fields; installed defaults false (loader manifest may establish presence during import) |
| same | `NAME.pending_enable`, `.enable_error`, runtime `state`, `error` | Not desired fields. Imported pending enable establishes enabled intent; the native host consumes it once and journals pending/error/actual activation in the state file's plugin runtime journal (a 1.3.x `/var/lib/ndimon/plugin-runtime.json` is imported once and never written again). Native `Plugins` queries project this observation |
| `/var/lib/ndimon-network/committed.json` | `version`, `revision` | Drop domain version/revision after validating v1; new schema/revision is global |
| same | `interface`, `backend` | `network.interface`, `.backend`; no document before adoption = empty interface, null backend, [] networks, empty roles |
| same | `networks[].{id,name,vlan_id,mode,address,prefix,gateway,dns}` | `network.networks[]` identical keys; DHCP values normalized to empty address/gateway/DNS and prefix 0 by the 1.3.x validator |
| same | `management`, `ndi`, `tessera` | `network.roles.management`, `.ndi`, `.audio` respectively; roles may share a network |
| legacy address rule / new setting | token-only confirm | `network.confirm_policy=token_only` default; configurable `require_management_address` |
| legacy VLAN index / single-network files | managed VLAN rows / base address settings | Merge into adopted profiles using the 1.3.x helper's rules for current state and base settings; require a representable effective configuration, including backend/interface; journal paths/tokens are not desired settings |
| `/etc/tessera/config.d/60-ndimon.toml`, `/var/lib/tessera/state.toml` | main device `name`, `sample_rate`, `encoding`, `latency`/`latency_us`, `rx_channels`, `tx_channels`, channel labels | `tessera.device.{name,sample_rate,encoding,latency_us,rx_channels,tx_channels,rx_labels,tx_labels}`; import effective runtime settings, including latency (incident 8). Labels keyed by 1-based channel, absent = daemon default |
| same | `discovery.advertise_devices`, clock `role`, `priority` | `tessera.device.advertise_devices`, `tessera.clock.role`, `.priority`; runtime default name/latency must be read, not guessed |
| same | host-owned locked settings | `tessera.locks` maps document-relative paths to owner descriptions; native reconciler owns and locks them, including flows, clock and binding; binding value derives from `network.roles.audio` |
| `/etc/tessera/config.d/61-ndimon-flows.toml` | `[[flow]] id,direction,peer,port,any_source,frames_per_packet` | `tessera.flows[]` identical keys; missing any_source=false; absent peer/packet size=null |
| same | `device="main"`, `channels=[]` | Drop constants; all flows carry **all** channels in their direction on main. Never turn [] into a zero-channel selection |
| `/etc/tessera/config.d/62-ndimon-network.toml` | `network.interfaces`, `locked` | Derived from `network.roles.audio` and host ownership; no independent binding to edit |
| `/etc/tessera/config.d/90-probe-flows.toml` | controller-created flows | Import/deduplicate into `tessera.flows` only when representable; report conflicting peer/port/direction; renderer retires this second writer. Empty field file stays empty |
| `/etc/alsa/conf.d/60-ndimon-tessera.conf`, `61-ndimon-tessera-ranges.conf` | PCM definitions / ranges | Entirely derived from `audio` endpoint resources/ranges + `tessera.device`; no separately editable PCM model |
| Tessera plugin backup restore | flows, legacy audio aliases | `RestoreTessera` applies both domains in one global-CAS transaction |
| Tessera plugin framework state | enabled | `tessera.enabled` mirrors the installed Tessera plugin enable setting during transition; generic plugin installation metadata remains in `plugins.tessera` |
| `/etc/ndimon-auth.json` | all (hash, salt, auth metadata) | **Excluded**: separate private secret file; never copied into the document, an event or an impact (a backup carries it only when the operator asks) |
| `ndimon-decN-status.json`, `ndimon-sources.json`, `NDIFinderSrcList.json`, `ndi_src_find_param`, `ndimon-network-interfaces.json`, `/run/ndimon/net-pending`, `/tmp/ndimon-dec1-cap`, watchdog stats | all | **Excluded**: observed/transient state, catalog, pending transaction or action trigger; do not import source lists as selected sources |
| `.ndi/ndi-config.v1.json`, distro network files, nftables, service drop-ins | all | Generated artifacts / foreign drift. Import supported desired settings from owning domains only; SDK/service runtime output is not desired state |
| `/var/lib/ndimon-network/{pending.json,lock}`, `/var/lib/ndimon/net/{pending,vlans}` journal metadata | tokens, snapshots, phase, boot id, timers | **Excluded** from committed document: recovery journal / pending revision belongs to network engine + observed state |
| `/etc/ndimon-{firmware-version,build-commit,build-date,source-dir}`, plugin manifests, `.broken`, `bundle-info.json` | all | Package/runtime metadata and diagnostics, not operator settings |
| browser drafts, localStorage confirmation token | all | Not document state; drafts carry revision, token is a transient credential |

## API action coverage

Reads of a stored domain use `Query::State` / `StateDomain`; catalogs and runtime
reads use `Catalog`, `Status`, `Health`, `Observed`, `AudioStatus`, `AudioSettings`,
`OutputAudio`, `Services`, `WatchdogStats`, `Version`, `VaapiInfo`, `NetworkStatus`,
`NetworkProfiles`, `PluginStatus`, `PluginJob`, `TesseraStatus/Metrics/Diagnostics/Flows`.
SSE uses `Query::Events`; `resync` is framing, not a persisted `Event` variant.

| Legacy mutations / special reads | Contract operation |
|---|---|
| Device settings + ApiSettings POST | `SetDevice` (audioEngine → `SetAudioEngine`; output-alias → `SetOutputAlias`) |
| decodesetup / shared output-discovery-transport restore | `PutOutputSettings` atomically merges outputs, optional complete NDI and retained audio-output enable flags; global snapshot CAS |
| decodeTransport / finder settings | `SetNdi` for global tally/color/transport/discovery/groups/off-subnet; transport restart requires confirmation |
| connectTo / explicit forget / reconnect | `SelectSource`, `ForgetSource`, `ReconnectSource`; `RetrySource` uses a transient endpoint without changing saved intent and rejects a stale saved source name |
| capture (legacy GET with a side effect) | `CaptureFrame` command |
| VideoOutput resolution / rotation / scale | `SetOutputVideo` (adapter reads snapshot then replaces video settings) |
| finder refresh / reset | `RefreshNdi`, `ResetNdiDiscovery` commands |
| Audio PUT engine / output / settings / PATCH source or sink | `SetAudioEngine`, `PutOutputAudio`, `PutAudio`, `PutAudioSource`, `PutAudioSink`; adapters merge patch fields before validation |
| Backup export / snapshots / inspect / apply / undo | `ExportBackup`, `BackupSnapshots`, `InspectBackup` queries; `ImportBackup`, `UndoRestore` commands |
| Network apply / profiles / VLAN PUT/DELETE / rollback | `StageNetwork` with complete profile snapshot; `RollbackNetwork`; adapters update profiles/assignments atomically |
| Network check / pending / profiles preview | `CheckNetworkAddress`, `NetworkPending`, `PreviewNetwork` queries |
| net-confirm / net-revert | `ConfirmNetwork`, `RevertNetwork`; token-only endpoints answer with the typed confirmation errors (`not_pending`, `expired`, `still_applying`, `token_mismatch`, `wrong_address`, `rate_limited`) |
| Plugins install / upload / update / enable / disable / remove / job cancel / reset / feed refresh | `InstallPlugin` (managed archive identifier containing the uploaded/feed SHA256, bound version and enable/force flags), `UpdatePlugin`, `SetPluginEnabled`, `RemovePlugin`, `CancelPluginJob`, `ResetPluginState`; `Plugins`/`PluginFeed`/`RefreshPluginFeed`/`PluginJob` queries (feed refresh is an observation, without desired-state revisions) |
| Out-of-process plugin-specific actions | `PluginAction` with plugin-scoped action/field payload; native Tessera uses typed operations instead |
| Presets save / recall / delete | `SavePreset` (explicit source, as today), `RecallPreset` (explicit output), `DeletePreset`; recall submits native `SelectSource` |
| Splash config / OSD / preview / upload logo | Native `SetDisplay`, live `PreviewSplash`, `SetLogo`; internal display restore compiles one `ImportBackup { scope: display }`. Images are signature-checked PNG/JPEG, SHA-256-addressed under `/var/lib/ndimon/assets/`; privd publishes the legacy `/etc/ndi-splash-logo.*` pointer target. |
| Status service restart / System reboot / softreboot / update | `ServiceControl`, `Reboot`, `RestartStack`; `InstallUpdate {version, force, ...}` exclusively for signed privd releases (optional `package` retained; version is required by this backend) |
| Tessera device / channels / clock / labels / flows | `SetTessera`, `SetTesseraLabel`, `PutTesseraFlows` (adapters preserve other settings using revision CAS; device counts atomically resize retained legacy audio aliases) |
| Tessera network-role / audio enable/disable/delay/routing / service | `StageNetwork`, `PutAudio` (engine+graph together), `TesseraService` |
| Encoder setup/transport; vendor subscriptions | `Query::Unsupported` / typed `ApiError::Unsupported`; preserve current unsupported status, no imaginary writer |
| login / logout / password / auth-status | Separate `SessionCommand` login/logout/change-password and `Query::Session`; passwords are redacted transient values, never state commands |

Dynamic capabilities (EDID modes, supported rotation, endpoint readiness,
video-delay memory cap, foreign network adoption and locks) are checked by the
planner against observed state. Pure `validate()` enforces portable invariants;
it cannot promise a particular display plane supports 180 degrees.

Device/System queries are native through `Hostname`, `Ntp`, `RebootSchedule`,
`AboutMe`, `Status`, `Version` and `VaapiInfo`. AboutMe remains observed appliance
identity; it does not replace the editable `device.about` metadata. Native System
actions require impact confirmation; legacy System adapters retain the UI's
existing confirmation and response semantics.
Native preset save/list/delete use domain CAS and compact `{presets:[...]}`
rendering; native recall submits outputs SelectSource and preserves the legacy
response bytes. Backup schema 1
remains the public file format. Native sections compile into one `ImportBackup`
with global CAS; capture, review and apply are coordinated in Rust, and
manifest-v1 plugin sections report "restorable after plugin support is available".
Password records only cross the backup
transport and private 0600 upload/snapshot files, never the desired document.
`legacy_sections` and `legacy_fingerprint` describe transitional impact without
embedding any legacy settings or password record in the command/document.
Native network integration: `/v1/Network/profiles{,/preview,/apply,/rollback}`
and base status/check/apply/rollback/VLAN reads compile to the network module.
Independent VLAN PUT/DELETE remain 409 refusals. Stage reserves a pending revision;
Confirm alone publishes desired intent through the actor's durable network decision.
`POST /api/net-{pending,confirm,revert}` use those confirmation error kinds and share
five mismatches/unclassified errors per actual peer per rolling minute. The
unchanged UI's preview creates a session-bound exact-draft impact receipt.


Plugin processes: `plugins.<name>.state` is a bounded JSON object owned only
by the installed v2 manifest. `SetPluginData` commits a complete validated subtree
with global CAS; `/v1/plugins/<name>/commands/<command>` executes the declared
command outside the actor and submits that proposal. Native plugin backup calls
retain schema-1 `{version,data}` sections without installing packages or changing
registry flags. Job/activation journals are envelope observations, not desired data.
