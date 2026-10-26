//! Closed privileged API. Version 1 uses external snake_case serde tags.
//! Only the transitional legacy JSON/logo variants accept bounded content.
mod values;
pub use values::*;
#[cfg(feature = "client")]
pub mod client;
pub mod codec;
mod network;
mod network_native;
pub use network::*;
pub use network_native::*;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::{IpAddr, Ipv4Addr};

pub const VERSION: u32 = 1;
/// Each privileged caller has its own listening socket, so privd knows who is
/// calling from the socket that accepted the connection on every kernel. The
/// directory is root-owned 0755; each socket is 0660 root:<its own group>.
/// `ndimon-control` is the only caller.
pub const CONTROL_SOCKET: &str = "/run/ndimon-privd/control.sock";
pub const MAX_CONFIG_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub v: u32,
    pub id: u64,
    pub req: Request,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reply {
    pub v: u32,
    pub id: u64,
    #[serde(flatten)]
    pub result: ReplyResult,
}
impl<'de> Deserialize<'de> for Reply {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireReply {
            v: u32,
            id: u64,
            ok: Option<ActionOutput>,
            err: Option<Error>,
        }
        let wire = WireReply::deserialize(d)?;
        let result = match (wire.ok, wire.err) {
            (Some(out), None) => ReplyResult::Ok(out),
            (None, Some(err)) => ReplyResult::Err(err),
            _ => {
                return Err(serde::de::Error::custom(
                    "exactly one of ok or err required",
                ));
            }
        };
        Ok(Self {
            v: wire.v,
            id: wire.id,
            result,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReplyResult {
    #[serde(rename = "ok")]
    Ok(ActionOutput),
    #[serde(rename = "err")]
    Err(Error),
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionOutput {
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub job: Option<PluginJob>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data: Option<Value>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Error {
    #[error("invalid {field}: {reason}")]
    Invalid { field: String, reason: String },
    #[error("permission denied")]
    PermissionDenied,
    #[error("another operation is running")]
    Busy,
    #[error("{step}: {reason}")]
    Failed {
        step: String,
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        exit_code: Option<i32>,
    },
    #[error("unsupported: {what}")]
    Unsupported { what: String },
    #[error("internal: {message}")]
    Internal { message: String },
}
impl Error {
    pub fn invalid(field: &str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            field: field.into(),
            reason: reason.into(),
        }
    }
    pub fn failed(step: &str, reason: impl ToString) -> Self {
        Self::Failed {
            step: step.into(),
            reason: reason.to_string(),
            exit_code: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedUnit {
    Control,
    Decoder,
    Finder,
}
impl ManagedUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Control => "ndimon-control.service",
            Self::Decoder => "ndimon-r.service",
            Self::Finder => "ndimon-finder.service",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitAction {
    Start,
    Stop,
    Restart,
    Reload,
}
impl UnitAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Reload => "reload",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuProfile {
    Decode,
    Restore,
    Disabled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Sun,
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
}
impl Weekday {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sun => "Sun",
            Self::Mon => "Mon",
            Self::Tue => "Tue",
            Self::Wed => "Wed",
            Self::Thu => "Thu",
            Self::Fri => "Fri",
            Self::Sat => "Sat",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RebootSchedule {
    Disabled,
    Daily { time: WallTime },
    Weekly { time: WallTime, days: Vec<Weekday> },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveMode {
    KeepState,
    Purge,
    Force,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum BundleAction {
    Install { restart: bool },
    Uninstall { purge: bool },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginArchive {
    Download { id: ArchiveId },
    Import { directory: ImportId, id: ArchiveId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyConfigFile {
    #[serde(rename = "ndimon-audio-settings.json")]
    Audio,
    #[serde(rename = "ndimon-about-settings.json")]
    About,
    #[serde(rename = "ndimon-device-settings.json")]
    Device,
    #[serde(rename = "ndimon-rx-settings.json")]
    Receive,
    #[serde(rename = "ndimon-find-settings.json")]
    Find,
    #[serde(rename = "ndimon-splash-settings.json")]
    Splash,
    #[serde(rename = "ndimon-osd-settings.json")]
    Osd,
    #[serde(rename = "ndimon-plugins.json")]
    Plugins,
    #[serde(rename = "ndimon-presets.json")]
    Presets,
    #[serde(rename = "ndimon-auth.json")]
    Auth,
    #[serde(rename = "ndi-config.json")]
    NdiConfig,
    #[serde(rename = "ndi-group.json")]
    NdiGroup,
    #[serde(rename = "ndimon-dec1-settings.json")]
    Dec1,
    #[serde(rename = "ndimon-dec2-settings.json")]
    Dec2,
    #[serde(rename = "ndimon-dec3-settings.json")]
    Dec3,
    #[serde(rename = "ndimon-dec4-settings.json")]
    Dec4,
    #[serde(rename = "ndimon-dec5-settings.json")]
    Dec5,
    #[serde(rename = "ndimon-dec6-settings.json")]
    Dec6,
    #[serde(rename = "ndimon-dec7-settings.json")]
    Dec7,
    #[serde(rename = "ndimon-dec8-settings.json")]
    Dec8,
}
impl LegacyConfigFile {
    pub fn filename(self) -> String {
        serde_json::to_value(self)
            .expect("enum serializes")
            .as_str()
            .expect("string enum")
            .into()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogoFile {
    #[serde(rename = "ndi-splash-logo.png")]
    Png,
    #[serde(rename = "ndi-splash-logo.jpg")]
    Jpg,
    #[serde(rename = "ndi-splash-logo.jpeg")]
    Jpeg,
}
impl LogoFile {
    pub fn filename(self) -> &'static str {
        match self {
            Self::Png => "ndi-splash-logo.png",
            Self::Jpg => "ndi-splash-logo.jpg",
            Self::Jpeg => "ndi-splash-logo.jpeg",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Rx,
    Tx,
}
impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rx => "rx",
            Self::Tx => "tx",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TesseraFlowSpec {
    pub id: FlowId,
    pub direction: Direction,
    pub port: UdpPort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<Ipv4Addr>,
    #[serde(default)]
    pub any_source: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames_per_packet: Option<FramesPerPacket>,
}
impl TesseraFlowSpec {
    pub fn validate(&self) -> Result<(), Error> {
        if self.any_source {
            if self.direction != Direction::Rx || self.peer.is_some() {
                return Err(Error::invalid(
                    "flows",
                    "any_source excludes peer and is RX only",
                ));
            }
        } else {
            let p = self
                .peer
                .ok_or_else(|| Error::invalid("flows.peer", "unicast IPv4 required"))?;
            let first = p.octets()[0];
            if p.is_multicast()
                || p.is_loopback()
                || p.is_unspecified()
                || first == 0
                || first >= 240
            {
                return Err(Error::invalid("flows.peer", "unicast IPv4 required"));
            }
        }
        if self.frames_per_packet.is_some() && self.direction != Direction::Tx {
            return Err(Error::invalid("frames_per_packet", "TX only"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TesseraPcmSpec {
    pub direction: Direction,
    pub first_channel: FirstChannel,
    pub channels: ChannelCount,
}

macro_rules! request_set {
    ($($variant:ident $({$($(#[$attr:meta])* $field:ident:$ty:ty),* $(,)?})?),* $(,)?) => {
        #[derive(Debug,Clone,PartialEq,Serialize)]
        #[serde(rename_all="snake_case")]
        pub enum Request { $($variant $({$($field:$ty),*})?),* }
        #[derive(Deserialize)]
        #[serde(rename_all="snake_case",deny_unknown_fields)]
        enum UncheckedRequest { $($variant $({$($(#[$attr])* $field:$ty),*})?),* }
        impl<'de> Deserialize<'de> for Request {
            fn deserialize<D:serde::Deserializer<'de>>(d:D)->Result<Self,D::Error>{
                let request=match UncheckedRequest::deserialize(d)? {
                    $(UncheckedRequest::$variant $({$($field),*})? => Self::$variant $({$($field),*})?),*
                };
                request.validate().map_err(serde::de::Error::custom)?;
                Ok(request)
            }
        }
    };
}
request_set! {
    WriteLegacyConfig { file: LegacyConfigFile, json: Value },
    WriteLegacyLogo { file: LogoFile, base64: String },
    RemoveLogo { file: LogoFile },
    RenderArtifacts { revision: u64 },
    ServiceControl { unit: ManagedUnit, action: UnitAction },
    RestartFinder, RestartStack, StopCore, StartCore, Reboot,
    SetHostname { name: Hostname },
    SetTimeSync { server: Option<NtpServer> },
    SetRebootSchedule { schedule: RebootSchedule },
    StartUpdate,
    InstallUpdate { version: ReleaseVersion, #[serde(default)] force: bool },
    CheckUpdate,
    CpuTuning { profile: CpuProfile },
    NetworkNative { call: NetworkCall },
    NetworkStatus,
    NetworkCheckAddress { address: Ipv4Addr },
    NetworkApply { settings: NetworkSettings },
    NetworkConfirm { token: NetworkToken },
    NetworkRollback,
    NetworkVlanList,
    NetworkVlanApply { vlan: VlanSettings, requester_ip: IpAddr },
    NetworkVlanRemove { id: VlanId, requester_ip: IpAddr },
    NetworkProfilesStatus,
    NetworkProfilesPreview { profiles: NetworkProfiles },
    NetworkProfilesStage { profiles: NetworkProfiles },
    NetworkProfilesConfirm { token: NetworkToken, local_address: Ipv4Addr },
    NetworkProfilesRevert { token: NetworkToken },
    NetworkProfilesRollback, NetworkProfilesBoot, NetworkProfilesSyncTessera,
    PluginRuntimeStart { name: PluginName },
    PluginRuntimeStop { name: PluginName },
    PluginRuntimeStatus { name: PluginName },
    PluginResetState,
    PluginInspect { name: PluginName, archive: PluginArchive, expected_version: Option<DebianVersion> },
    PluginInstall { name: PluginName, archive: PluginArchive, expected_version: Option<DebianVersion>, force: bool },
    PluginUninstall { name: PluginName, mode: RemoveMode },
    // Third-party legacy commands take no free-form argv or stdin. New commands
    // need a typed variant; Tessera's historical arguments are variants below.
    PluginExec { name: PluginName, command: PluginCommandName },
    PluginServiceControl { unit: PluginUnit, action: UnitAction },
    PluginJobStatus { job: PluginJob }, PluginJobCancel { job: PluginJob },
    TesseraWriteFlows { flows: Vec<TesseraFlowSpec> },
    TesseraWriteAlsa { rx: ChannelCount, tx: ChannelCount },
    TesseraWritePcms { ranges: Vec<TesseraPcmSpec> },
    TesseraBundle { action: BundleAction },
    TesseraService { action: UnitAction }, TesseraEnable, TesseraDisable,
}

impl Request {
    /// Polled by the control plane every few seconds; logged at debug, not info.
    pub fn is_routine_read(&self) -> bool {
        matches!(self, Self::CheckUpdate)
            || matches!(self, Self::NetworkNative { call } if call.is_read_only())
    }
    pub fn kind(&self) -> &'static str {
        // Keep request labels payload-free (passwords, tokens and config are never logged).
        match self {
            Self::InstallUpdate { .. } => "install_update",
            Self::CheckUpdate => "check_update",
            Self::WriteLegacyConfig { .. } => "write_legacy_config",
            Self::WriteLegacyLogo { .. } => "write_legacy_logo",
            Self::RemoveLogo { .. } => "remove_logo",
            Self::RenderArtifacts { .. } => "render_artifacts",
            Self::ServiceControl { .. } => "service_control",
            Self::RestartFinder => "restart_finder",
            Self::RestartStack => "restart_stack",
            Self::StopCore => "stop_core",
            Self::StartCore => "start_core",
            Self::Reboot => "reboot",
            Self::SetHostname { .. } => "set_hostname",
            Self::SetTimeSync { .. } => "set_time_sync",
            Self::SetRebootSchedule { .. } => "set_reboot_schedule",
            Self::StartUpdate => "start_update",
            Self::CpuTuning { .. } => "cpu_tuning",
            Self::NetworkNative { .. } => "network_native",
            Self::NetworkStatus => "network_status",
            Self::NetworkCheckAddress { .. } => "network_check_address",
            Self::NetworkApply { .. } => "network_apply",
            Self::NetworkConfirm { .. } => "network_confirm",
            Self::NetworkRollback => "network_rollback",
            Self::NetworkVlanList => "network_vlan_list",
            Self::NetworkVlanApply { .. } => "network_vlan_apply",
            Self::NetworkVlanRemove { .. } => "network_vlan_remove",
            Self::NetworkProfilesStatus => "network_profiles_status",
            Self::NetworkProfilesPreview { .. } => "network_profiles_preview",
            Self::NetworkProfilesStage { .. } => "network_profiles_stage",
            Self::NetworkProfilesConfirm { .. } => "network_profiles_confirm",
            Self::NetworkProfilesRevert { .. } => "network_profiles_revert",
            Self::NetworkProfilesRollback => "network_profiles_rollback",
            Self::NetworkProfilesBoot => "network_profiles_boot",
            Self::NetworkProfilesSyncTessera => "network_profiles_sync_tessera",
            Self::PluginRuntimeStart { .. } => "plugin_runtime_start",
            Self::PluginRuntimeStop { .. } => "plugin_runtime_stop",
            Self::PluginRuntimeStatus { .. } => "plugin_runtime_status",
            Self::PluginResetState => "plugin_reset_state",
            Self::PluginInspect { .. } => "plugin_inspect",
            Self::PluginInstall { .. } => "plugin_install",
            Self::PluginUninstall { .. } => "plugin_uninstall",
            Self::PluginExec { .. } => "plugin_exec",
            Self::PluginServiceControl { .. } => "plugin_service_control",
            Self::PluginJobStatus { .. } => "plugin_job_status",
            Self::PluginJobCancel { .. } => "plugin_job_cancel",
            Self::TesseraWriteFlows { .. } => "tessera_write_flows",
            Self::TesseraWriteAlsa { .. } => "tessera_write_alsa",
            Self::TesseraWritePcms { .. } => "tessera_write_pcms",
            Self::TesseraBundle { .. } => "tessera_bundle",
            Self::TesseraService { .. } => "tessera_service",
            Self::TesseraEnable => "tessera_enable",
            Self::TesseraDisable => "tessera_disable",
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::WriteLegacyConfig { json, .. }
                if serde_json::to_vec(json)
                    .map_err(|e| Error::invalid("json", e.to_string()))?
                    .len()
                    > MAX_CONFIG_BYTES =>
            {
                Err(Error::invalid("json", "config exceeds 4 MiB"))
            }
            Self::WriteLegacyLogo { base64, .. }
                if base64.len() > MAX_CONFIG_BYTES.div_ceil(3) * 4 =>
            {
                Err(Error::invalid("base64", "logo exceeds 4 MiB"))
            }
            Self::SetRebootSchedule {
                schedule: RebootSchedule::Weekly { days, .. },
            } => {
                if days.is_empty()
                    || days.len() > 7
                    || days.iter().enumerate().any(|(i, d)| days[..i].contains(d))
                {
                    Err(Error::invalid("days", "unique, nonempty weekdays required"))
                } else {
                    Ok(())
                }
            }
            Self::NetworkApply { settings } => settings.validate(),
            Self::NetworkVlanApply { vlan, .. } => vlan.validate(),
            Self::NetworkProfilesPreview { profiles } | Self::NetworkProfilesStage { profiles } => {
                profiles.validate()
            }
            Self::TesseraWriteFlows { flows } => {
                if serde_json::to_vec(flows)
                    .map_err(|e| Error::invalid("flows", e.to_string()))?
                    .len()
                    > 60000
                {
                    return Err(Error::invalid(
                        "flows",
                        "flows exceed 60KB plugin write limit",
                    ));
                }
                let mut ids = std::collections::BTreeSet::new();
                let mut rx = 0;
                for flow in flows {
                    flow.validate()?;
                    if !ids.insert(&flow.id) {
                        return Err(Error::invalid("flows.id", "unique flow id required"));
                    }
                    if flow.direction == Direction::Rx {
                        rx += 1;
                    }
                }
                if rx > 1 {
                    Err(Error::invalid(
                        "flows",
                        "only one RX flow; all channels are carried",
                    ))
                } else {
                    Ok(())
                }
            }
            Self::TesseraWritePcms { ranges } => {
                if ranges.len() > 128
                    || ranges.iter().any(|r| {
                        u32::from(r.first_channel.get()) + u32::from(r.channels.get()) > 65536
                    })
                {
                    Err(Error::invalid("ranges", "invalid PCM range"))
                } else {
                    Ok(())
                }
            }
            Self::PluginServiceControl {
                action: UnitAction::Reload,
                ..
            } => Err(Error::invalid(
                "action",
                "plugin service control allows start, stop or restart",
            )),
            _ => Ok(()),
        }
    }
}

pub mod plugin;
