//! Private native bridge. Existing version-1 network requests retain their
//! exact encoding; this additive variant carries the contracts they cannot.
use ndimon_types::{ImpactToken, NetworkHandoff, NetworkRequest, NetworkSessionPath};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSocketContext {
    pub peer: IpAddr,
    pub local: IpAddr,
}
impl From<NetworkSessionPath> for NetworkSocketContext {
    fn from(value: NetworkSessionPath) -> Self {
        Self {
            peer: value.peer,
            local: value.local,
        }
    }
}
impl From<NetworkSocketContext> for NetworkSessionPath {
    fn from(value: NetworkSocketContext) -> Self {
        Self::new(value.peer, value.local)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkCall {
    Status,
    /// `Status` for polling: a network nothing has been committed for is not
    /// adopted, so no backend tool runs for it (see `Engine::observe`).
    Observe,
    CheckAddress {
        address: String,
    },
    Preview {
        request: NetworkRequest,
    },
    Stage {
        request: NetworkRequest,
        session: Option<NetworkSocketContext>,
    },
    Pending {
        token: String,
    },
    Confirm {
        token: String,
        local_address: Option<IpAddr>,
    },
    Revert {
        token: String,
    },
    Rollback,
    /// Give up a failed recovery and keep the network as it is (NI-17).
    AcceptCurrent,
    HandoffPrepare,
    HandoffCancel {
        id: String,
    },
    HandoffImport {
        source: NetworkHandoff,
    },
    HandoffCommit {
        source: NetworkHandoff,
        expect_impact: ImpactToken,
        session: Option<NetworkSocketContext>,
    },
}

impl NetworkCall {
    /// A call that reads and never changes the network: whoever polls with it
    /// need not drop what it has learned, and its request is routine in a log.
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::Status
                | Self::Observe
                | Self::CheckAddress { .. }
                | Self::Preview { .. }
                | Self::Pending { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Request;

    #[test]
    fn only_reads_are_read_only() {
        for (call, read_only) in [
            (serde_json::json!("status"), true),
            (serde_json::json!("observe"), true),
            (serde_json::json!("rollback"), false),
            (serde_json::json!("handoff_prepare"), false),
            (serde_json::json!({"pending":{"token":"a"}}), true),
            (serde_json::json!({"revert":{"token":"a"}}), false),
            (
                serde_json::json!({"confirm":{"token":"a","local_address":null}}),
                false,
            ),
        ] {
            let call: NetworkCall = serde_json::from_value(call.clone()).unwrap();
            assert_eq!(call.is_read_only(), read_only, "{call:?}");
        }
    }

    #[test]
    fn existing_network_request_encoding_is_unchanged() {
        assert_eq!(
            serde_json::to_string(&Request::NetworkProfilesStatus).unwrap(),
            "\"network_profiles_status\""
        );
        assert_eq!(
            serde_json::to_string(&Request::NetworkProfilesRollback).unwrap(),
            "\"network_profiles_rollback\""
        );
        let token = "a".repeat(64);
        let old = serde_json::json!({"network_profiles_confirm":{"token":token,"local_address":"192.0.2.1"}});
        let request: Request = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), old);
    }

    #[test]
    fn native_extension_is_closed_and_keeps_classified_calls() {
        let request = Request::NetworkNative {
            call: NetworkCall::Pending {
                token: "a".repeat(64),
            },
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(serde_json::from_value::<Request>(value).unwrap(), request);
        assert!(
            serde_json::from_value::<NetworkCall>(
                serde_json::json!({"pending":{"token":"a","local_address":"192.0.2.1"}})
            )
            .is_err()
        );
    }
}
