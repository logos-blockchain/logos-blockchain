mod broadcast;
mod core;
mod edge;
mod ondemand;

use std::fmt::Debug;

use lb_log_targets::blend;
use overwatch::services::relay::RelayError;

#[cfg(test)]
pub use crate::modes::broadcast::tests as broadcast_tests;
pub use crate::modes::{broadcast::BroadcastMode, core::CoreMode, edge::EdgeMode};

const LOG_TARGET: &str = blend::service::MODES;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Overwatch error: {0}")]
    Overwatch(#[from] overwatch::DynError),
    #[error("Overwatch relay error: {0}")]
    OverwatchRelay(#[from] RelayError),
}
