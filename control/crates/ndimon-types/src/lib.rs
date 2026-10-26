//! Public NDIMon-R contract. See LEGACY-MAPPING.md for import/render rules.
//! Enums use external snake_case tags. Secrets and runtime state never belong
//! in DesiredState. Validation is independent of serialization and I/O.
mod model;
mod plugins;
pub use plugins::*;
mod tessera;
pub use tessera::*;
mod network;
mod validation;
mod wire;

pub use model::*;
pub use network::*;
pub use validation::{
    MAX_DNS_SEARCH, alias_hostname, valid_dns_search_domain, valid_web_hostname, validate,
};
pub use wire::*;

pub const SCHEMA_VERSION: u32 = 1;

#[cfg(all(test, feature = "serde"))]
mod tests;
