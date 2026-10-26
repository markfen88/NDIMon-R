use crate::*;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Domain {
    Device,
    Outputs,
    Ndi,
    Audio,
    Network,
    Tessera,
    Display,
    Presets,
    Plugins,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Origin {
    Ui {
        session: String,
    },
    Cli {
        uid: u32,
    },
    Plugin {
        name: String,
    },
    Tessera {
        peer: String,
    },
    Boot,
    Migration,
    /// A Discovery Server controller routed an output (the SDK already switched it).
    DiscoveryServer,
}

impl Origin {
    pub fn requires_confirmation(&self) -> bool {
        !matches!(
            self,
            Self::Boot | Self::Migration | Self::Tessera { .. } | Self::DiscoveryServer
        )
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct DevicePatch {
    pub hostname: Option<String>,
    pub ndi_alias: Option<String>,
    pub mode: Option<OperationMode>,
    pub decode_mode: Option<DecodeMode>,
    pub watchdog_mode: Option<WatchdogMode>,
    pub ntp_server: Option<String>,
    pub reboot_schedule: Option<RebootSchedule>,
    pub about: Option<About>,
    pub analog_audio_in_gain: Option<u8>,
    pub analog_audio_out_gain: Option<u8>,
    pub plugin_feeds: Option<Vec<String>>,
    pub allow_insecure_feeds: Option<bool>,
    pub web_allowed_hosts: Option<Vec<String>>,
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
pub enum Service {
    Decoder,
    Finder,
    Control,
    Watchdog,
    Tessera,
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
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
    Reload,
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
pub enum RestoreScope {
    All,
    Settings,
    Sources,
    Audio,
    Network,
    Display,
    Plugins,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct BackupDocument {
    pub document: DesiredState,
    pub assets: Vec<AssetRef>,
    // Transitional effects only; legacy settings and secrets never enter state.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    pub legacy_sections: Vec<String>,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "String::is_empty")
    )]
    pub legacy_fingerprint: String,
}

impl BackupDocument {
    pub fn new(document: DesiredState) -> Self {
        Self {
            document,
            assets: Vec::new(),
            legacy_sections: Vec::new(),
            legacy_fingerprint: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Command {
    SetDevice(DevicePatch),
    SelectSource {
        output: OutputId,
        source: NdiSourceRef,
    },
    ForgetSource {
        output: OutputId,
    },
    ReconnectSource {
        output: OutputId,
    },
    /// Transient recovery endpoint; never replaces the saved source selection.
    RetrySource {
        output: OutputId,
        source: NdiSourceRef,
    },
    /// Save the source a Discovery Server controller already switched the
    /// output to (None = it disconnected the output). The decoder is on that
    /// source, so this never reconnects it.
    AdoptRemoteSource {
        output: OutputId,
        source: Option<NdiSourceRef>,
    },
    /// Atomic legacy decode setup / restore across shared decoder-file fields.
    PutOutputSettings {
        outputs: std::collections::BTreeMap<OutputId, Output>,
        ndi: Option<Ndi>,
        audio_enabled: std::collections::BTreeMap<OutputId, bool>,
    },
    SetOutputAlias {
        output: OutputId,
        alias: String,
    },
    SetOutputVideo {
        output: OutputId,
        video: OutputVideo,
    },
    SetScreenSaver {
        output: OutputId,
        screensaver: ScreenSaver,
    },
    SetNdi(Ndi),
    RefreshNdi,
    ResetNdiDiscovery,
    CaptureFrame {
        output: OutputId,
    },
    SetAudioEngine {
        engine: AudioEngine,
    },
    PutAudio(Audio),
    PutOutputAudio {
        output: OutputId,
        audio: OutputAudio,
        normalize_matrix: bool,
    },
    PutAudioSource {
        id: String,
        source: AudioSource,
    },
    PutAudioSink {
        id: String,
        sink: AudioSink,
    },
    RemoveAudioEndpoints {
        ids: Vec<String>,
    },
    StageNetwork {
        profiles: NetworkProfiles,
    },
    ConfirmNetwork {
        token: String,
    },
    RevertNetwork {
        token: String,
    },
    RollbackNetwork,
    /// The way out of a network recovery that cannot finish: stop retrying and
    /// keep the network as it is now (NI-17).
    AcceptNetworkRecovery,
    PrepareNetworkHandoff,
    CancelNetworkHandoff {
        id: String,
    },
    ImportNetworkHandoff {
        source: crate::NetworkHandoff,
    },
    CommitNetworkHandoff {
        source: crate::NetworkHandoff,
    },
    SetTessera(Tessera),
    TesseraService {
        action: TesseraServiceAction,
    },
    /// Atomic Tessera backup restore, including its legacy audio projection.
    RestoreTessera {
        tessera: Tessera,
        audio: Audio,
    },
    PutTesseraFlows {
        flows: Vec<TesseraFlow>,
    },
    SetTesseraLabel {
        direction: Direction,
        channel: u16,
        label: Option<String>,
    },
    SetDisplay(Display),
    PreviewSplash {
        source_available: bool,
    },
    SetLogo {
        asset: Option<AssetRef>,
    },
    SavePreset {
        name: PresetName,
        preset: Preset,
    },
    RecallPreset {
        name: PresetName,
        output: OutputId,
    },
    DeletePreset {
        name: PresetName,
    },
    ImportBackup {
        document: Box<BackupDocument>,
        scope: RestoreScope,
    },
    UndoRestore {
        snapshot: String,
    },
    InstallPlugin {
        name: PluginName,
        // Managed download identifier, never a caller-selected path.
        archive: String,
        version: String,
        enable: bool,
        force: bool,
    },
    UpdatePlugin {
        name: PluginName,
        archive: String,
        version: String,
        enable: bool,
        force: bool,
    },
    SetPluginEnabled {
        name: PluginName,
        enabled: bool,
        #[cfg_attr(feature = "serde", serde(default))]
        version: Option<String>,
    },
    RemovePlugin {
        name: PluginName,
        purge: bool,
        force: bool,
    },
    CancelPluginJob {
        job: String,
    },
    ResetPluginState,
    /// A complete plugin-owned subtree, validated against its installed manifest.
    SetPluginData {
        name: PluginName,
        #[cfg_attr(feature = "ts", ts(type = "{ [key: string]: unknown }"))]
        state: BTreeMap<String, serde_json::Value>,
    },
    /// Typed out-of-process plugin action; no paths or executable arguments.
    PluginAction {
        name: PluginName,
        action: String,
        fields: BTreeMap<String, String>,
    },
    ServiceControl {
        service: Service,
        action: ServiceAction,
    },
    Reboot,
    RestartStack,
    InstallUpdate {
        package: Option<AssetRef>,
        #[cfg_attr(
            feature = "serde",
            serde(default, skip_serializing_if = "Option::is_none")
        )]
        version: Option<String>,
        #[cfg_attr(feature = "serde", serde(default))]
        force: bool,
    },
    ReloadConfig,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct CommandRequest {
    pub command: Command,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expect_revision: Option<u64>,
    pub expect_impact: Option<ImpactToken>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub confirm: bool,
    #[cfg_attr(feature = "serde", serde(skip))]
    #[cfg_attr(feature = "ts", ts(skip))]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub network_session: Option<NetworkSessionPath>,
    /// In-process admission fence for background work; never accepted from JSON.
    #[cfg_attr(feature = "serde", serde(skip))]
    #[cfg_attr(feature = "ts", ts(skip))]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub expect_activity: Option<u64>,
}

impl CommandRequest {
    pub fn new(command: Command) -> Self {
        Self {
            command,
            expect_revision: None,
            expect_impact: None,
            confirm: false,
            network_session: None,
            expect_activity: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct CommandReply {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    pub report: ApplyReport,
}

impl CommandReply {
    pub fn new(revision: u64, report: ApplyReport) -> Self {
        Self { revision, report }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct ApplyReport {
    pub impact: Impact,
    pub steps: Vec<ApplyStep>,
    pub pending_restart: Vec<Domain>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct ApplyStep {
    pub step: String,
    pub status: ApplyStatus,
    pub detail: Option<String>,
}

impl ApplyStep {
    pub fn new(step: impl Into<String>, status: ApplyStatus) -> Self {
        Self {
            step: step.into(),
            status,
            detail: None,
        }
    }
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
pub enum ApplyStatus {
    Started,
    Done,
    Failed,
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
pub enum ImpactScope {
    #[default]
    None,
    Output,
    Service,
    Device,
    Network,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ImpactToken(String);

impl ImpactToken {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for ImpactToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Impact {
    pub scope: ImpactScope,
    pub restarts: Vec<Service>,
    pub interruptions: Vec<Interruption>,
    pub lines: Vec<String>,
    pub token: ImpactToken,
    // Complete upstream impact, including its opaque confirmation token.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    #[cfg_attr(feature = "ts", ts(type = "unknown | null"))]
    pub counterpart: Option<Box<serde_json::Value>>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Interruption {
    pub domain: Domain,
    pub target: String,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub expected_duration_ms: Option<u64>,
}

impl Interruption {
    pub fn new(
        domain: Domain,
        target: impl Into<String>,
        expected_duration_ms: Option<u64>,
    ) -> Self {
        Self {
            domain,
            target: target.into(),
            expected_duration_ms,
        }
    }
}
impl Impact {
    pub fn new(scope: ImpactScope) -> Self {
        Self {
            scope,
            ..Self::default()
        }
    }
    pub fn needs_confirmation(&self) -> bool {
        self.scope != ImpactScope::None
    }
    pub fn describe(&self) -> String {
        self.lines.join("\n")
    }

    /// Seal what the operator confirms. Presentation lines and list order do
    /// not matter; the complete command and reconciler context do. No secrets
    /// belong in a command or context passed to this function.
    #[cfg(feature = "serde")]
    pub fn sealed(
        mut self,
        command: &Command,
        context: &[String],
    ) -> Result<Self, serde_json::Error> {
        use sha2::{Digest, Sha256};
        let mut restarts = self.restarts.clone();
        restarts.sort();
        restarts.dedup();
        let mut interruptions: Vec<Vec<u8>> = self
            .interruptions
            .iter()
            .map(serde_json::to_vec)
            .collect::<Result<_, _>>()?;
        interruptions.sort();
        interruptions.dedup();
        let bytes = serde_json::to_vec(&(command, self.scope, restarts, interruptions, context))?;
        self.token = ImpactToken::new(format!("v1-{:x}", Sha256::digest(bytes)));
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct FieldError {
    pub field: String,
    pub reason: String,
}

impl FieldError {
    pub fn new(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }
}
impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.reason)
    }
}
impl std::error::Error for FieldError {}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum ApiError {
    Invalid {
        field: String,
        reason: String,
        errors: Vec<FieldError>,
    },
    Locked {
        field: String,
        #[cfg_attr(
            feature = "serde",
            serde(default, skip_serializing_if = "Option::is_none")
        )]
        by: Option<String>,
    },
    ConfirmationRequired {
        impact: Impact,
    },
    RevisionConflict {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        current_revision: u64,
    },
    Drift {
        path: String,
    },
    NotFound {
        what: String,
    },
    PermissionDenied,
    RateLimited,
    Unsupported {
        what: String,
    },
    ApplyFailed {
        domain: Domain,
        step: String,
        reason: String,
    },
    Internal {
        message: String,
    },
    Unauthorized,
    NotPending,
    Expired,
    StillApplying,
    TokenMismatch,
    WrongAddress,
}

impl ApiError {
    pub fn invalid(errors: Vec<FieldError>) -> Self {
        let first = errors
            .first()
            .cloned()
            .unwrap_or_else(|| FieldError::new("", "invalid value"));
        Self::Invalid {
            field: first.field,
            reason: first.reason,
            errors,
        }
    }
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Invalid { .. } => 400,
            Self::Unauthorized => 401,
            Self::PermissionDenied | Self::TokenMismatch => 403,
            Self::NotFound { .. } => 404,
            Self::Locked { .. }
            | Self::ConfirmationRequired { .. }
            | Self::RevisionConflict { .. }
            | Self::Drift { .. }
            | Self::StillApplying
            | Self::WrongAddress => 409,
            Self::NotPending | Self::Expired => 410,
            Self::RateLimited => 429,
            Self::Unsupported { .. } => 501,
            // Keep or Revert met a recovery that did not verify: the helper keeps
            // its journal and retries. A state of the transaction, not a fault of
            // the request (counterpart contract §3).
            Self::ApplyFailed {
                domain: Domain::Network,
                step,
                ..
            } if step == "recover" => 409,
            Self::ApplyFailed { .. } | Self::Internal { .. } => 500,
        }
    }
    pub fn counts_network_token_failure(&self) -> bool {
        matches!(self, Self::TokenMismatch | Self::Internal { .. })
    }
}
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { field, reason, .. } => write!(f, "{field}: {reason}"),
            Self::Locked { field, by } => write!(
                f,
                "{field} is locked{}",
                by.as_ref().map(|v| format!(" by {v}")).unwrap_or_default()
            ),
            Self::ConfirmationRequired { impact } => {
                write!(f, "Confirmation required: {}", impact.describe())
            }
            Self::RevisionConflict { current_revision } => {
                write!(f, "State changed (revision {current_revision})")
            }
            Self::Drift { path } => write!(f, "Network handoff artifact changed: {path}"),
            Self::NotFound { what } => write!(f, "Not found: {what}"),
            Self::Unsupported { what } => write!(f, "Unsupported: {what}"),
            Self::ApplyFailed {
                domain,
                step,
                reason,
            } => write!(f, "{domain:?} failed at {step}: {reason}"),
            Self::Internal { message } => f.write_str(message),
            Self::PermissionDenied => f.write_str("Permission denied"),
            Self::RateLimited => f.write_str("Too many requests"),
            Self::Unauthorized => f.write_str("Login required"),
            Self::NotPending => f.write_str("No network change is pending"),
            Self::Expired => {
                f.write_str("Network confirmation expired; previous settings restored")
            }
            Self::StillApplying => f.write_str("Network change is still applying"),
            Self::TokenMismatch => f.write_str("Network confirmation token does not match"),
            Self::WrongAddress => f.write_str("Confirm through the new management address"),
        }
    }
}
impl std::error::Error for ApiError {}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct ErrorEnvelope {
    pub error: ApiError,
    pub message: String,
}

impl From<ApiError> for ErrorEnvelope {
    fn from(error: ApiError) -> Self {
        Self {
            message: error.to_string(),
            error,
        }
    }
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
pub enum SyncStatus {
    #[default]
    InSync,
    Applying,
    PendingRestart,
    PendingConfirmation,
    Failed,
    Drift,
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
pub enum NetworkPhase {
    #[cfg_attr(feature = "serde", serde(alias = "scheduled"))]
    Staged,
    Applying,
    Applied,
    Confirmed,
    Reverting,
    Reverted,
    Expired,
    Failed,
}
impl NetworkPhase {
    /// Source compatibility for migration callers; public JSON uses `staged`.
    #[allow(non_upper_case_globals)]
    pub const Scheduled: Self = Self::Staged;
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Observed {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    pub domains: BTreeMap<Domain, DomainObserved>,
    pub drift: Vec<Drift>,
    pub network_pending: Option<PendingNetwork>,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct DomainObserved {
    pub status: SyncStatus,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub applied_revision: Option<u64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct Drift {
    pub domain: Domain,
    pub path: String,
    pub artifact: Option<String>,
    pub foreign: Option<String>,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "String::is_empty")
    )]
    pub selector: String,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "String::is_empty")
    )]
    pub interface: String,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "std::ops::Not::not")
    )]
    pub neutralized: bool,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "std::ops::Not::not")
    )]
    pub blocking: bool,
}

impl Drift {
    pub fn new(domain: Domain, path: impl Into<String>) -> Self {
        Self {
            domain,
            path: path.into(),
            artifact: None,
            foreign: None,
            selector: String::new(),
            interface: String::new(),
            neutralized: false,
            blocking: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct PendingNetwork {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    pub expires_at: String,
    pub phase: NetworkPhase,
    #[cfg_attr(feature = "serde", serde(default))]
    pub job_id: String,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub remaining_seconds: u64,
    #[cfg_attr(feature = "serde", serde(default))]
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub management_address: Option<std::net::Ipv4Addr>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub management_port: u16,
    #[cfg_attr(feature = "serde", serde(default))]
    pub confirm_policy: ConfirmPolicy,
    #[cfg_attr(feature = "serde", serde(default))]
    pub recovery_pending: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub apply_error: Option<ApiError>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub recovery_error: Option<ApiError>,
}

impl PendingNetwork {
    pub fn new(revision: u64, expires_at: impl Into<String>, phase: NetworkPhase) -> Self {
        Self {
            revision,
            expires_at: expires_at.into(),
            phase,
            job_id: String::new(),
            remaining_seconds: 0,
            management_address: None,
            management_port: 0,
            confirm_policy: ConfirmPolicy::default(),
            recovery_pending: false,
            apply_error: None,
            recovery_error: None,
        }
    }
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
pub enum StatusSource {
    Decoder,
    Finder,
    Tessera,
    Network,
    Services,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Event {
    StateChanged {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        revision: u64,
        domains: Vec<Domain>,
        origin: Origin,
    },
    ApplyProgress {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        revision: u64,
        step: String,
        status: ApplyStatus,
        detail: Option<String>,
    },
    Observed {
        domain: Domain,
        delta: DomainObserved,
    },
    Drift {
        domain: Domain,
        path: String,
        artifact: Option<String>,
        foreign: Option<String>,
        #[cfg_attr(
            feature = "serde",
            serde(default, skip_serializing_if = "String::is_empty")
        )]
        selector: String,
        #[cfg_attr(
            feature = "serde",
            serde(default, skip_serializing_if = "String::is_empty")
        )]
        interface: String,
        #[cfg_attr(
            feature = "serde",
            serde(default, skip_serializing_if = "std::ops::Not::not")
        )]
        neutralized: bool,
        #[cfg_attr(
            feature = "serde",
            serde(default, skip_serializing_if = "std::ops::Not::not")
        )]
        blocking: bool,
    },
    NetworkPending {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        revision: u64,
        expires_at: String,
        phase: NetworkPhase,
    },
    Status {
        source: StatusSource,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        data: serde_json::Value,
    },
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
pub enum Catalog {
    AudioEndpoints,
    NdiSources,
    NetworkInterfaces,
    DisplayModes,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum Query {
    Session,
    State,
    StateDomain {
        domain: Domain,
    },
    Observed,
    Catalog {
        name: Catalog,
        output: Option<OutputId>,
    },
    Status,
    Health,
    Services,
    WatchdogStats,
    Version,
    VaapiInfo,
    Hostname,
    Ntp,
    RebootSchedule,
    AboutMe,
    AudioSettings,
    OutputAudio {
        output: OutputId,
    },
    AudioStatus,
    NetworkStatus,
    NetworkProfiles,
    NetworkPending {
        token: String,
    },
    PreviewNetwork {
        profiles: NetworkProfiles,
    },
    CheckNetworkAddress {
        address: String,
    },
    Plugins,
    PluginFeed,
    RefreshPluginFeed,
    /// Resolve/download a legacy feed or uploaded archive before confirmation.
    PreparePluginInstall {
        name: PluginName,
        source: String,
        version: Option<String>,
    },
    PluginJob {
        job: String,
    },
    PluginStatus {
        name: PluginName,
    },
    TesseraStatus,
    TesseraMetrics,
    TesseraDiagnostics,
    TesseraFlows,
    /// The legacy encoder/subscription endpoints have no implementation.
    Unsupported {
        what: String,
    },
    ExportBackup {
        scope: RestoreScope,
    },
    BackupSnapshots,
    InspectBackup {
        document: Box<BackupDocument>,
    },
    Events,
}

/// Authentication has separate handlers; credentials never enter state/commands,
/// backups, impact digests or events. A Debug implementation always redacts.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Password(String);
impl Password {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password([redacted])")
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum SessionCommand {
    Login {
        password: Password,
    },
    Logout,
    ChangePassword {
        current_password: Password,
        new_password: Password,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub enum TesseraServiceAction {
    Start,
    Stop,
    Restart,
    Reload,
}
