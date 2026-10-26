use std::collections::BTreeMap;

/// Stable 1-based decoder channel, validated as 1..=8 (never a DRM index).
pub type OutputId = u8;
pub type PresetName = String;
pub type PluginName = String;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum OperationMode {
    #[default]
    Decode,
    Encode,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum DecodeMode {
    #[default]
    Auto,
    Hardware,
    Software,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum WatchdogMode {
    Disabled,
    Passive,
    #[default]
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Weekday {
    #[default]
    Sun,
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum ScaleMode {
    #[default]
    Letterbox,
    Stretch,
    Crop,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum ScreenSaver {
    Splash,
    #[default]
    Black,
    Capture,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum TallyMode {
    On,
    #[default]
    Off,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum ColorSpace {
    #[default]
    Yuv,
    Rgb,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Transport {
    #[default]
    Rudp,
    Multicast,
    MultiTcp,
    Udp,
    Tcp,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum AudioEngine {
    #[default]
    V1,
    V2,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum EndpointType {
    Ndi,
    #[default]
    Alsa,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Mapping {
    #[default]
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum NetworkBackend {
    Networkmanager,
    Networkd,
    Netplan,
    Ifupdown,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum AddressMode {
    #[default]
    Dhcp,
    Static,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum ConfirmPolicy {
    RequireManagementAddress,
    #[default]
    TokenOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Direction {
    Rx,
    Tx,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum ClockRole {
    #[default]
    Auto,
    Follower,
    Leader,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Backdrop {
    #[default]
    Gradient,
    Flat,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct DesiredState {
    pub schema_version: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    pub device: Device,
    pub outputs: BTreeMap<OutputId, Output>,
    pub ndi: Ndi,
    pub audio: Audio,
    pub network: NetworkProfiles,
    pub tessera: Option<Tessera>,
    pub display: Display,
    pub presets: BTreeMap<PresetName, Preset>,
    pub plugins: BTreeMap<PluginName, PluginState>,
}

impl Default for DesiredState {
    fn default() -> Self {
        Self {
            schema_version: crate::SCHEMA_VERSION,
            revision: 0,
            device: Device::default(),
            outputs: BTreeMap::from([(1, Output::default())]),
            ndi: Ndi::default(),
            audio: Audio::default(),
            network: NetworkProfiles::default(),
            tessera: None,
            display: Display::default(),
            presets: BTreeMap::new(),
            plugins: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Device {
    pub hostname: String,
    pub ndi_alias: String,
    pub mode: OperationMode,
    pub decode_mode: DecodeMode,
    pub watchdog_mode: WatchdogMode,
    pub ntp_server: String,
    pub reboot_schedule: RebootSchedule,
    pub about: About,
    pub analog_audio_in_gain: u8,
    pub analog_audio_out_gain: u8,
    pub plugin_feeds: Vec<String>,
    pub allow_insecure_feeds: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub web_allowed_hosts: Vec<String>,
}

impl Default for Device {
    fn default() -> Self {
        Self {
            hostname: String::new(),
            ndi_alias: String::new(),
            mode: OperationMode::default(),
            decode_mode: DecodeMode::default(),
            watchdog_mode: WatchdogMode::default(),
            ntp_server: String::new(),
            reboot_schedule: RebootSchedule::default(),
            about: About::default(),
            analog_audio_in_gain: 50,
            analog_audio_out_gain: 50,
            plugin_feeds: Vec::new(),
            allow_insecure_feeds: false,
            web_allowed_hosts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct RebootSchedule {
    pub enabled: bool,
    pub time: String,
    pub days: Vec<Weekday>,
}

impl Default for RebootSchedule {
    fn default() -> Self {
        Self {
            enabled: false,
            time: "04:00".into(),
            days: vec![
                Weekday::Sun,
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
                Weekday::Sat,
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct About {
    pub group_name: String,
    pub mac_address: String,
    pub device_name: String,
    pub host_name: String,
}

impl Default for About {
    fn default() -> Self {
        Self {
            group_name: "public".into(),
            mac_address: String::new(),
            device_name: String::new(),
            host_name: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Output {
    pub alias: String,
    pub source: Option<NdiSourceRef>,
    pub video: OutputVideo,
    pub screensaver: ScreenSaver,
    pub source_selection: String,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            alias: String::new(),
            source: None,
            video: OutputVideo::default(),
            screensaver: ScreenSaver::Black,
            source_selection: "NDI".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct OutputVideo {
    pub resolution: Resolution,
    pub scale_mode: ScaleMode,
    pub rotation: u16,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Resolution {
    #[default]
    Auto,
    Mode {
        width: u32,
        height: u32,
        refresh_hz: f64,
    },
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct NdiSourceRef {
    pub name: String,
    pub endpoint: String,
}

impl NdiSourceRef {
    pub fn new(name: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            endpoint: endpoint.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Ndi {
    pub receive: Transport,
    pub discovery_servers: Vec<String>,
    pub groups: Vec<String>,
    pub off_subnet_sources: Vec<String>,
    pub tally: TallyMode,
    pub color_space: ColorSpace,
    // Register the outputs as receivers with the Discovery Server (monitoring).
    #[cfg_attr(feature = "serde", serde(default = "default_true"))]
    pub receiver_advertising: bool,
    // Let a Discovery Server controller choose the outputs' sources. New
    // installs start with it off; earlier documents and imports had it on.
    #[cfg_attr(feature = "serde", serde(default = "default_true"))]
    pub remote_control: bool,
}

impl Default for Ndi {
    fn default() -> Self {
        Self {
            receive: Transport::default(),
            discovery_servers: Vec::new(),
            groups: vec!["public".into()],
            off_subnet_sources: Vec::new(),
            tally: TallyMode::default(),
            color_space: ColorSpace::default(),
            receiver_advertising: true,
            remote_control: false,
        }
    }
}

/// One graph for both engines. Node gain/delay/mix retain v1 factorization,
/// including inactive settings; v2 routes are the concrete runtime authority.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Audio {
    pub engine: AudioEngine,
    pub settings: AudioEngineSettings,
    pub sources: BTreeMap<String, AudioSource>,
    pub sinks: BTreeMap<String, AudioSink>,
    pub outputs: BTreeMap<OutputId, OutputAudio>,
    pub routes: BTreeMap<String, AudioRoute>,
    pub video_delay_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct AudioEngineSettings {
    pub enabled: bool,
    pub rate: u32,
    pub period_frames: u32,
    pub master: String,
    pub memory_budget_mib: u32,
}

impl Default for AudioEngineSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            rate: 48_000,
            period_frames: 512,
            master: "hdmi".into(),
            memory_budget_mib: 64,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct AudioSource {
    pub kind: EndpointType,
    pub enabled: bool,
    pub channels: u16,
    pub output: Option<OutputId>,
    pub alsa_pcm: Option<String>,
    pub endpoint: String,
    pub resource: String,
    pub channel_start: u32,
    pub channel_count: u16,
    pub gain_db: f64,
    pub delay_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct AudioSink {
    pub enabled: bool,
    pub channels: u16,
    pub alsa_pcm: String,
    pub endpoint: String,
    pub resource: String,
    pub channel_start: u32,
    pub channel_count: u16,
    pub delay_ms: u32,
    pub mix: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct AudioRoute {
    pub output: OutputId,
    pub source: String,
    pub sink: String,
    pub enabled: bool,
    pub gain_db: f64,
    pub delay_ms: u32,
    pub mapping: Mapping,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct OutputAudio {
    pub enabled: bool,
    pub primary: bool,
    pub inputs: Vec<AudioInput>,
    pub destinations: Vec<AudioDestination>,
    pub legacy_matrix: bool,
    pub definitions: AudioDefinitions,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct AudioDefinitions {
    pub sources: BTreeMap<String, AudioSource>,
    pub sinks: BTreeMap<String, AudioSink>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct AudioInput {
    pub endpoint: String,
    pub gain_db: f64,
    pub delay_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct AudioDestination {
    pub endpoint: String,
    pub delay_ms: u32,
}

impl AudioSource {
    pub fn ndi(output: OutputId) -> Self {
        Self {
            kind: EndpointType::Ndi,
            enabled: true,
            channels: 2,
            output: Some(output),
            alsa_pcm: None,
            endpoint: format!("ndi-{output}"),
            resource: String::new(),
            channel_start: 1,
            channel_count: 2,
            gain_db: 0.0,
            delay_ms: 0,
        }
    }
    pub fn alsa(pcm: impl Into<String>, channels: u16) -> Self {
        Self {
            kind: EndpointType::Alsa,
            output: None,
            alsa_pcm: Some(pcm.into()),
            channels,
            channel_count: channels,
            endpoint: String::new(),
            ..Self::ndi(1)
        }
    }
}

impl AudioSink {
    pub fn new(pcm: impl Into<String>, channels: u16) -> Self {
        Self {
            enabled: true,
            channels,
            alsa_pcm: pcm.into(),
            endpoint: String::new(),
            resource: String::new(),
            channel_start: 1,
            channel_count: channels,
            delay_ms: 0,
            mix: BTreeMap::new(),
        }
    }
}

impl AudioInput {
    pub fn new(endpoint: impl Into<String>, gain_db: f64, delay_ms: u32) -> Self {
        Self {
            endpoint: endpoint.into(),
            gain_db,
            delay_ms,
        }
    }
}
impl AudioDestination {
    pub fn new(endpoint: impl Into<String>, delay_ms: u32) -> Self {
        Self {
            endpoint: endpoint.into(),
            delay_ms,
        }
    }
}
impl AudioRoute {
    pub fn new(output: OutputId, source: impl Into<String>, sink: impl Into<String>) -> Self {
        Self {
            output,
            source: source.into(),
            sink: sink.into(),
            enabled: true,
            gain_db: 0.0,
            delay_ms: 0,
            mapping: Mapping::Default,
        }
    }
}
impl Default for Audio {
    fn default() -> Self {
        let mut sink = AudioSink::new("auto", 2);
        sink.endpoint = "hdmi-1".into();
        sink.resource = "hdmi:1".into();
        sink.mix.insert("ndi".into(), 0.0);
        Self {
            engine: AudioEngine::V1,
            settings: AudioEngineSettings::default(),
            sources: BTreeMap::from([("ndi".into(), AudioSource::ndi(1))]),
            sinks: BTreeMap::from([("hdmi".into(), sink)]),
            outputs: BTreeMap::from([(
                1,
                OutputAudio {
                    enabled: true,
                    primary: true,
                    inputs: vec![AudioInput::new("ndi-1", 0.0, 0)],
                    destinations: vec![AudioDestination::new("hdmi-1", 0)],
                    ..OutputAudio::default()
                },
            )]),
            routes: BTreeMap::from([("primary".into(), AudioRoute::new(1, "ndi", "hdmi"))]),
            video_delay_ms: 0,
        }
    }
}

/// None backend + empty interface/networks/assignments means not adopted.
/// Import the effective wired configuration before the first network mutation.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct NetworkProfiles {
    pub interface: String,
    pub backend: Option<NetworkBackend>,
    pub networks: Vec<NetworkProfile>,
    pub roles: NetworkRoles,
    pub confirm_policy: ConfirmPolicy,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct NetworkRoles {
    pub management: String,
    pub ndi: String,
    pub audio: String,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct NetworkProfile {
    pub id: String,
    pub name: String,
    pub vlan_id: Option<u16>,
    pub mode: AddressMode,
    pub address: String,
    pub prefix: u8,
    pub gateway: String,
    pub dns: Vec<String>,
    // NE-2.12a: management-only DNS search domains, DHCP or static. Omitted
    // when empty so documents without them stay schema-1 byte-identical.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    pub dns_search: Vec<String>,
}

impl NetworkProfile {
    pub fn dhcp(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            vlan_id: None,
            mode: AddressMode::Dhcp,
            address: String::new(),
            prefix: 0,
            gateway: String::new(),
            dns: Vec::new(),
            dns_search: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Tessera {
    pub enabled: bool,
    pub device: TesseraDevice,
    pub clock: TesseraClock,
    pub flows: Vec<TesseraFlow>,
    pub locks: BTreeMap<String, String>,
    // Reference to the adopted audio network; None follows the audio role.
    #[cfg_attr(feature = "serde", serde(default))]
    pub network_binding: Option<String>,
    // Enforce the 0.2.2 protocol lock; false is the explicit old-daemon fallback.
    #[cfg_attr(feature = "serde", serde(default = "default_true"))]
    pub protocol_flow_lock: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct TesseraDevice {
    pub name: String,
    pub sample_rate: u32,
    pub encoding: u8,
    pub latency_us: u32,
    pub rx_channels: u16,
    pub tx_channels: u16,
    pub rx_labels: BTreeMap<u16, String>,
    pub tx_labels: BTreeMap<u16, String>,
    pub advertise_devices: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct TesseraClock {
    pub role: ClockRole,
    pub priority: u8,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct TesseraFlow {
    pub id: String,
    pub direction: Direction,
    pub peer: Option<String>,
    pub port: u16,
    pub any_source: bool,
    pub frames_per_packet: Option<u16>,
}

impl Tessera {
    /// Latency and device name are platform/runtime defaults: import them rather
    /// than substituting a value from the editor. The bundle profile is 2x2/48k/24.
    pub fn new(name: impl Into<String>, latency_us: u32) -> Self {
        Self {
            enabled: true,
            device: TesseraDevice {
                name: name.into(),
                sample_rate: 48_000,
                encoding: 24,
                latency_us,
                rx_channels: 2,
                tx_channels: 2,
                rx_labels: BTreeMap::new(),
                tx_labels: BTreeMap::new(),
                advertise_devices: true,
            },
            clock: TesseraClock {
                role: ClockRole::Auto,
                priority: 128,
            },
            flows: Vec::new(),
            locks: Self::owned_locks(),
            network_binding: None,
            protocol_flow_lock: true,
        }
    }
    pub fn owned_locks() -> BTreeMap<String, String> {
        [
            "device.name",
            "device.sample_rate",
            "device.encoding",
            "device.latency",
            "device.rx_channels",
            "device.tx_channels",
            "device.rx_channel_labels",
            "device.tx_channel_labels",
            "clock.role",
            "clock.priority",
            "flows",
        ]
        .into_iter()
        .map(|key| (key.into(), "ndimon-control".into()))
        .collect()
    }
}
impl TesseraFlow {
    pub fn tx(id: impl Into<String>, peer: impl Into<String>, port: u16) -> Self {
        Self {
            id: id.into(),
            direction: Direction::Tx,
            peer: Some(peer.into()),
            port,
            any_source: false,
            frames_per_packet: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Display {
    pub splash: Splash,
    pub osd: Osd,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Splash {
    pub bg_idle: String,
    pub bg_live: String,
    pub accent_idle: String,
    pub accent_live: String,
    pub logo: Option<AssetRef>,
    pub logo_x_pct: f64,
    pub logo_y_pct: f64,
    pub logo_w_pct: f64,
    pub text_idle: String,
    pub text_live: String,
    pub text_height_pct: f64,
    pub backdrop: Backdrop,
    pub show_signal_text: bool,
    pub show_device_name: bool,
    pub show_device_url: bool,
    pub show_sources_available: bool,
}

impl Default for Splash {
    fn default() -> Self {
        Self {
            bg_idle: "#0D1B2A".into(),
            bg_live: "#0D2B1A".into(),
            accent_idle: "#4488CC".into(),
            accent_live: "#22FF88".into(),
            logo: None,
            logo_x_pct: 50.0,
            logo_y_pct: 50.0,
            logo_w_pct: 50.0,
            text_idle: "No Signal".into(),
            text_live: "Signal Available".into(),
            text_height_pct: 4.0,
            backdrop: Backdrop::Gradient,
            show_signal_text: true,
            show_device_name: true,
            show_device_url: true,
            show_sources_available: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Osd {
    pub enabled: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct AssetRef {
    pub sha256: String,
    pub media_type: String,
}

impl AssetRef {
    pub fn new(sha256: impl Into<String>, media_type: impl Into<String>) -> Self {
        Self {
            sha256: sha256.into(),
            media_type: media_type.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Preset {
    pub source: NdiSourceRef,
    // Legacy list order; absent in older state envelopes.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub position: Option<u32>,
}

impl Preset {
    pub fn new(source: NdiSourceRef) -> Self {
        Self {
            source,
            position: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct PluginState {
    pub installed: bool,
    pub enabled: bool,
    pub version: String,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    #[cfg_attr(feature = "ts", ts(optional, type = "{ [key: string]: unknown }"))]
    pub state: Option<BTreeMap<String, serde_json::Value>>,
}

#[cfg(feature = "serde")]
fn default_true() -> bool {
    true
}
