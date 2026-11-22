//! The subset of tessera-control-types (0.2.2, routing from 0.5.0) used by NDIMon-R. No Tessera
//! crate is linked. Unknown observation fields are retained for legacy clients.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const WIRE_VERSION: u32 = 1;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Rx,
    Tx,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockRole {
    Follower,
    Leader,
    Auto,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    SetDeviceName {
        device: String,
        name: String,
    },
    SetSampleRate {
        device: String,
        rate: u32,
    },
    SetEncoding {
        device: String,
        encoding: u8,
    },
    SetLatency {
        device: String,
        latency: u32,
    },
    SetDeviceChannels {
        device: String,
        rx: u16,
        tx: u16,
        #[serde(default)]
        dry_run: bool,
    },
    SetChannelLabel {
        device: String,
        direction: Direction,
        channel: std::num::NonZeroU16,
        label: Option<String>,
    },
    SetClockRole {
        role: ClockRole,
    },
    SetClockPriority {
        priority: u8,
    },
    ReloadConfig,
    /// Tessera 0.5: one observed receiver change, checked again by the daemon.
    RouteReceiver {
        expectation: String,
        change: ReceiverChange,
    },
    /// Accept a freshly inspected local receiver baseline after an uncertain
    /// operation; never retries the mutation.
    ReconcileReceiverOperation {
        review: String,
    },
}
/// Tessera 0.5 routing references, as the daemon issues them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointRef {
    pub id: String,
    pub generation: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiverSelection {
    pub tx_device: String,
    pub tx_channel: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingSourceRef {
    pub endpoint: EndpointRef,
    pub channel: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverChange {
    Connect {
        source: RoutingSourceRef,
    },
    Replace {
        previous: ReceiverSelection,
        source: RoutingSourceRef,
    },
    Disconnect {
        previous: ReceiverSelection,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingDevicesQuery {
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingChannelsQuery {
    pub endpoint: EndpointRef,
    pub direction: Direction,
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingReceiverQuery {
    pub endpoint: EndpointRef,
    pub first: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Query {
    Status,
    Metrics,
    RoutingDevices(RoutingDevicesQuery),
    RoutingChannels(RoutingChannelsQuery),
    RoutingReceiver(RoutingReceiverQuery),
    /// Complete local receiver review (also while execution is fenced).
    InspectReceiverOperation {
        operation: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestBody {
    Command(Command),
    ConfirmedCommand {
        command: Command,
        #[serde(default)]
        expect: Option<String>,
    },
    Query(Query),
    SubscribeEvents,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub v: u32,
    pub id: u64,
    pub body: RequestBody,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    #[serde(default)]
    pub devices: Vec<DeviceStatus>,
    #[serde(default)]
    pub clocks: Vec<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
impl Status {
    pub fn supports_flow_lock(&self) -> bool {
        let mut parts = self.version.trim_start_matches('v').split('.');
        let major = parts.next().and_then(|v| v.parse::<u32>().ok());
        let minor = parts.next().and_then(|v| v.parse::<u32>().ok());
        let patch = parts
            .next()
            .and_then(|v| v.split('-').next())
            .and_then(|v| v.parse::<u32>().ok());
        matches!((major,minor,patch), (Some(a),Some(b),Some(c)) if (a,b,c) >= (0,2,2))
    }
    /// Receiver routing, discovery browsing and `routing_sources` (0.5.0).
    pub fn supports_routing(&self) -> bool {
        let mut parts = self.version.trim_start_matches('v').split('.');
        let major = parts.next().and_then(|v| v.parse::<u32>().ok());
        let minor = parts.next().and_then(|v| v.parse::<u32>().ok());
        matches!((major, minor), (Some(a), Some(b)) if (a, b) >= (0, 5))
    }
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub key: String,
    #[serde(default)]
    pub sample_rate: u32,
    #[serde(default)]
    pub rx_channels: Vec<Value>,
    #[serde(default)]
    pub tx_channels: Vec<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

/// Keep the daemon's complete impact and opaque token. NDIMon never computes
/// or substitutes a Tessera confirmation token.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Impact {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandError {
    /// Tessera 0.5 routing: refresh the inventory and choose again.
    RoutingSnapshotExpired,
    /// Tessera 0.5 routing: the receiver did not answer; nothing was retried.
    ReceiverReadTimeout,
    ReceiverReadUnavailable {
        reason: String,
    },
    /// Tessera 0.5 device access lock (PIN).
    AccessLocked {
        device: String,
    },
    IncorrectPin,
    Invalid {
        field: String,
        reason: String,
    },
    Locked {
        field: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
    },
    NotFound {
        what: String,
    },
    PermissionDenied,
    RateLimited,
    Unsupported {
        what: String,
    },
    Internal {
        message: String,
    },
    ConfirmationRequired {
        impact: Impact,
    },
    ResponseTooLarge {
        size: u64,
        limit: u64,
        key: String,
    },
    #[serde(other)]
    Unknown,
}
impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid { reason, .. } => f.write_str(reason),
            Self::Locked { field, .. } => write!(f, "{field} is locked by the configuration"),
            Self::NotFound { what } | Self::Unsupported { what } => f.write_str(what),
            Self::Internal { message } => f.write_str(message),
            Self::ConfirmationRequired { .. } => f.write_str("confirmation_required"),
            Self::RoutingSnapshotExpired => {
                f.write_str("routing inventory changed; refresh and choose again")
            }
            Self::ReceiverReadTimeout => {
                f.write_str("receiver did not answer in time; no retry was sent")
            }
            Self::ReceiverReadUnavailable { reason } => {
                write!(f, "receiver read unavailable: {reason}")
            }
            Self::AccessLocked { device } => {
                write!(f, "device {device} is locked with a PIN; unlock it first")
            }
            Self::IncorrectPin => f.write_str("incorrect PIN"),
            Self::Unknown => {
                f.write_str("Tessera refused the request with an error this NDIMon-R does not know")
            }
            _ => write!(f, "{self:?}"),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefusedValue {
    pub field: String,
    pub origin: Value,
    #[serde(default)]
    pub attempted: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeValue {
    pub field: String,
    pub origin: Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolFlows {
    pub flows: Vec<String>,
    pub origin: Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Event {
    LockedValueRefused {
        locked_value_refused: RefusedValue,
    },
    RuntimeValueChanged {
        runtime_value_changed: RuntimeValue,
    },
    ProtocolFlowsChanged {
        protocol_flows_changed: ProtocolFlows,
    },
    Other(Value),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Done,
    Status(Status),
    Metrics(Metrics),
    Impact(Impact),
    Event(Event),
    RoutingDevices(Value),
    RoutingChannels(Value),
    RoutingReceiver(Value),
    ReceiverOperation(Value),
    ReceiverRecovery(Value),
    Lagged {
        dropped: u64,
    },
    #[serde(other)]
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultBody {
    Ok(Reply),
    Err(CommandError),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub v: u32,
    pub id: u64,
    pub result: ResultBody,
}
