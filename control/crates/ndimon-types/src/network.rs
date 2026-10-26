//! Additive network envelopes and runtime/persistence metadata. Profiles keep
//! the existing schema-1 IPv4 serialization; legacy shapes require an adapter.
use crate::{AddressMode, NetworkProfile, NetworkProfiles};
use std::{collections::BTreeMap, net::IpAddr};

/// NE-9: peer locations are fixed by product, never supplied filesystem paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum NetworkProduct {
    Ndimon,
    Tessera,
}
impl NetworkProduct {
    pub fn state_directory(self) -> &'static str {
        match self {
            Self::Ndimon => "/var/lib/ndimon-network",
            Self::Tessera => "/var/lib/tessera-network",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct NetworkHandoff {
    pub product: NetworkProduct,
    pub id: String,
}
impl NetworkHandoff {
    pub fn new(product: NetworkProduct, id: impl Into<String>) -> Self {
        Self {
            product,
            id: id.into(),
        }
    }
}

/// Proof retained by the single authoritative network decision after Keep.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[non_exhaustive]
pub struct NetworkHandoffReceipt {
    pub source: NetworkHandoff,
    pub document_sha256: String,
}
impl NetworkHandoffReceipt {
    pub fn new(source: NetworkHandoff, document_sha256: String) -> Self {
        Self {
            source,
            document_sha256,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[non_exhaustive]
pub struct NetworkRequest {
    pub version: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub expect_revision: u64,
    pub profiles: NetworkProfiles,
}
impl NetworkRequest {
    pub fn new(expect_revision: u64, profiles: NetworkProfiles) -> Self {
        Self {
            version: 1,
            expect_revision,
            profiles,
        }
    }
}

/// Server-only context from the accepted socket after authentication. It has no
/// request-body deserializer; forwarded headers cannot supply these addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkSessionPath {
    pub peer: IpAddr,
    pub local: IpAddr,
}
impl NetworkSessionPath {
    pub fn new(peer: IpAddr, local: IpAddr) -> Self {
        Self { peer, local }
    }
}

/// Private adoption evidence stored in the authority's state envelope, never
/// DesiredState, public observations, backups, impact digests or events.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[non_exhaustive]
pub struct NetworkBackendMetadata {
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub handoff: Option<NetworkHandoffReceipt>,
    pub parent_source: String,
    pub parent_definition: String,
    pub parent_non_ip: serde_json::Value,
    pub sources: Vec<String>,
    pub dhcp_metric: u32,
    pub static_metric: u32,
    pub nm_parent_uuid: Option<String>,
    pub nm_owned: BTreeMap<String, String>,
}
impl Default for NetworkBackendMetadata {
    fn default() -> Self {
        Self {
            handoff: None,
            parent_source: String::new(),
            parent_definition: String::new(),
            parent_non_ip: serde_json::json!({}),
            sources: Vec::new(),
            dhcp_metric: 1024,
            static_metric: 0,
            nm_parent_uuid: None,
            nm_owned: BTreeMap::new(),
        }
    }
}
impl NetworkBackendMetadata {
    pub fn metric(&self, profile: &NetworkProfile) -> u32 {
        if profile.mode == AddressMode::Dhcp {
            self.dhcp_metric
        } else {
            self.static_metric
        }
    }
}
