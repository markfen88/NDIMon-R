//! Endpoint identity shared by native audio and Tessera projections.
use crate::Direction;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TesseraEndpoint {
    pub direction: Direction,
    pub start: u32,
    pub channels: u16,
}
impl TesseraEndpoint {
    pub fn new(direction: Direction, start: u32, channels: u16) -> Self {
        Self {
            direction,
            start,
            channels,
        }
    }
    pub fn id(&self) -> String {
        format!(
            "tessera-{}-{}-{}",
            self.direction_name(),
            self.start,
            self.channels
        )
    }
    pub fn pcm(&self) -> String {
        format!(
            "ndimon_tessera_{}_{}_{}",
            self.direction_name(),
            self.start,
            self.channels
        )
    }
    pub fn direction_name(&self) -> &'static str {
        match self.direction {
            Direction::Rx => "rx",
            Direction::Tx => "tx",
        }
    }
    pub fn resource(&self) -> String {
        format!("tessera:main:{}", self.direction_name())
    }
    pub fn legacy(&self) -> &'static str {
        match self.direction {
            Direction::Rx => "net-in",
            Direction::Tx => "net-out",
        }
    }
    pub fn is_rx(id: &str, resource: &str) -> bool {
        id == "net-in" || id.starts_with("tessera-rx-") || resource == "tessera:main:rx"
    }
    pub fn is_tx(id: &str, resource: &str) -> bool {
        id == "net-out" || id.starts_with("tessera-tx-") || resource == "tessera:main:tx"
    }
}
