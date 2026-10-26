//! Transitional network model independent of ndimon-types and the N4 engine.
use crate::{Error, InterfaceName, NetworkId, VlanId};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

// Empty strings on the legacy wire mean no IPv4 address. Reject every other
// malformed string at decode time, before constructing helper argv or stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OptionalIpv4(pub Option<Ipv4Addr>);
impl TryFrom<String> for OptionalIpv4 {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        if s.is_empty() {
            Ok(Self(None))
        } else {
            s.parse()
                .map(|v| Self(Some(v)))
                .map_err(|_| "invalid IPv4 address".into())
        }
    }
}
impl From<OptionalIpv4> for String {
    fn from(v: OptionalIpv4) -> Self {
        v.0.map(|v| v.to_string()).unwrap_or_default()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressMode {
    Dhcp,
    Static,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSettings {
    pub mode: AddressMode,
    pub address: OptionalIpv4,
    pub prefix: u8,
    pub gateway: OptionalIpv4,
    pub dns: Vec<Ipv4Addr>,
}
impl NetworkSettings {
    pub fn validate(&self) -> Result<(), Error> {
        if self.dns.len() > 3 {
            return Err(Error::invalid("dns", "at most three addresses"));
        }
        if self.mode == AddressMode::Dhcp {
            return Ok(());
        }
        let address = self
            .address
            .0
            .ok_or_else(|| Error::invalid("address", "static IPv4 required"))?;
        if !(8..=30).contains(&self.prefix) {
            return Err(Error::invalid("prefix", "must be 8..30"));
        }
        let mask = u32::MAX << (32 - self.prefix);
        let host = u32::from(address) & !mask;
        if host == 0
            || host == !mask
            || address.is_loopback()
            || address.is_link_local()
            || address.is_multicast()
            || address.is_unspecified()
        {
            return Err(Error::invalid("address", "not a usable unicast address"));
        }
        if self
            .gateway
            .0
            .is_some_and(|g| u32::from(g) & mask != u32::from(address) & mask)
        {
            return Err(Error::invalid("gateway", "outside subnet"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VlanSettings {
    pub id: VlanId,
    pub mode: AddressMode,
    pub address: OptionalIpv4,
    pub prefix: u8,
    pub gateway: OptionalIpv4,
    pub allow_gateway: bool,
}
impl VlanSettings {
    pub fn validate(&self) -> Result<(), Error> {
        if self.gateway.0.is_some() && !self.allow_gateway {
            return Err(Error::invalid("gateway", "requires allow_gateway"));
        }
        NetworkSettings {
            mode: self.mode,
            address: self.address.clone(),
            prefix: self.prefix,
            gateway: self.gateway.clone(),
            dns: vec![],
        }
        .validate()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkBackend {
    NetworkManager,
    Networkd,
    Netplan,
    Ifupdown,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkProfile {
    pub id: NetworkId,
    pub name: String,
    pub vlan_id: Option<VlanId>,
    pub mode: AddressMode,
    pub address: OptionalIpv4,
    pub prefix: u8,
    pub gateway: OptionalIpv4,
    pub dns: Vec<Ipv4Addr>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkProfiles {
    pub version: u32,
    pub revision: u64,
    pub interface: InterfaceName,
    pub backend: NetworkBackend,
    pub networks: Vec<NetworkProfile>,
    pub management: NetworkId,
    pub ndi: NetworkId,
    pub tessera: NetworkId,
}
impl NetworkProfiles {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 || self.networks.is_empty() || self.networks.len() > 9 {
            return Err(Error::invalid(
                "networks",
                "version 1; one to nine networks required",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut tags = std::collections::BTreeSet::new();
        let mut subnets: Vec<(u32, u32)> = vec![];
        for n in &self.networks {
            if n.name.trim().is_empty()
                || n.name.chars().count() > 64
                || n.name.chars().any(char::is_control)
            {
                return Err(Error::invalid("networks.name", "invalid name"));
            }
            if !ids.insert(&n.id) || !tags.insert(n.vlan_id) {
                return Err(Error::invalid("networks", "duplicate id or VLAN"));
            }
            if n.vlan_id
                .is_some_and(|v| self.interface.as_str().len() + 1 + v.get().to_string().len() > 15)
            {
                return Err(Error::invalid("interface", "VLAN interface name too long"));
            }
            NetworkSettings {
                mode: n.mode,
                address: n.address.clone(),
                prefix: n.prefix,
                gateway: n.gateway.clone(),
                dns: n.dns.clone(),
            }
            .validate()?;
            if n.id != self.management && (n.gateway.0.is_some() || !n.dns.is_empty()) {
                return Err(Error::invalid(
                    "networks.gateway",
                    "gateway and DNS belong to management only",
                ));
            }
            if n.mode == AddressMode::Static {
                let a = u32::from(n.address.0.expect("validated"));
                let mask = u32::MAX << (32 - n.prefix);
                let lo = a & mask;
                let hi = lo | !mask;
                if subnets.iter().any(|(l, h)| lo <= *h && *l <= hi) {
                    return Err(Error::invalid("networks", "subnets overlap"));
                }
                subnets.push((lo, hi));
            }
        }
        if tags.iter().filter(|v| v.is_some()).count() > 8 {
            return Err(Error::invalid("networks", "at most eight tagged VLANs"));
        }
        if [&self.management, &self.ndi, &self.tessera]
            .iter()
            .any(|id| !ids.contains(id))
        {
            return Err(Error::invalid(
                "roles",
                "removed network requires replacement",
            ));
        }
        if serde_json::to_vec(self)
            .map_err(|e| Error::invalid("profiles", e.to_string()))?
            .len()
            > 65536
        {
            return Err(Error::invalid("profiles", "exceeds 64KiB"));
        }
        Ok(())
    }
}
