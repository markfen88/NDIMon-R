//! Mount legacy_router on the authenticated HTTP listener. It authenticates no
//! public request and supplies no CSRF middleware; the front door does both.
pub mod audio;
pub mod backup;
pub mod common;
pub mod device;
pub mod display;
pub mod ndi;
pub mod network;
mod network_limit;
pub mod network_validation;
pub mod outputs;
pub mod plugins;
pub mod presets;
pub mod status;
pub mod system;
pub mod tessera;
pub mod validation;
pub use audio::audio_router;
use axum::{Router, extract::FromRef};
pub use common::UiSession;
use ndimon_state::StateHandle;

pub fn legacy_router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    Router::new()
        .merge(audio::router())
        .merge(display::router())
        .merge(outputs::router())
        .merge(ndi::router())
        .merge(device::router())
        .merge(system::router())
        .merge(status::router())
        .merge(tessera::router())
        .merge(presets::router())
        .merge(backup::router())
        .merge(network::router())
        .merge(plugins::router())
}

#[cfg(test)]
mod tests;
