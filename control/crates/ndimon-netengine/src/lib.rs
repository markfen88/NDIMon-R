//! Privileged, single-writer network transactions. See README.md for the privd
//! job ABI and the mandatory backend certification gate.
pub mod backend;
pub mod engine;
pub mod handoff;
#[cfg(feature = "harness")]
pub mod harness;
pub mod host;
pub mod migration;
pub mod store;
pub mod validation;
mod yaml;

pub use engine::{
    Accepted, Authority, Commit, Engine, EngineConfig, Pending, Phase, Preview, Refusal, Stage,
    Status, Unrestored,
};
pub use host::{
    Clock, CommandOutput, CommandRunner, CommandSpec, Host, LinuxClock, Processes, Tool,
};
pub use ndimon_types::{ApiError, NetworkProfiles};
pub type Result<T> = std::result::Result<T, ApiError>;

pub const MAX_MESSAGE: usize = 65_536;
pub const MAX_PRIVATE: usize = 2 * 1024 * 1024;
pub const STATE_DIRECTORY: &str = "/var/lib/ndimon-network";
pub const BINDING: &str = "/etc/ndimon-network-interfaces.json";
pub const TESSERA_BINDING: &str = "/etc/tessera/config.d/62-ndimon-network.toml";
pub const BANNER: &str = "/run/ndimon/net-pending";
pub const PRIVD: &str = "/usr/libexec/ndimon/ndimon-privd";
pub const CONFIRM_MS: u64 = 60_000;
pub const APPLY_MS: u64 = 90_000;
pub const GUARD_MS: u64 = 180_000;

pub(crate) fn invalid(field: &str, reason: &str) -> ApiError {
    ApiError::invalid(vec![ndimon_types::FieldError::new(field, reason)])
}
pub(crate) fn unsupported(what: &str) -> ApiError {
    ApiError::Unsupported { what: what.into() }
}
pub(crate) fn failed(step: &str, reason: &str) -> ApiError {
    ApiError::ApplyFailed {
        domain: ndimon_types::Domain::Network,
        step: step.into(),
        reason: reason.into(),
    }
}
pub(crate) fn locked(by: &str) -> ApiError {
    ApiError::Locked {
        field: "network".into(),
        by: Some(by.into()),
    }
}

#[cfg(test)]
mod tests;
