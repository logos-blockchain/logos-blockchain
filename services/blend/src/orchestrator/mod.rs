//! Starting and stopping the mode the membership calls for.
//!
//! Nothing here blends. [`Instance`] tracks which mode's service is running —
//! and what is still draining behind it — while [`OnDemandServiceMode`] is the
//! Overwatch plumbing for bringing one up and taking it down.
//!
//! This used to be `modes/`, which was a misleading name once the modes became
//! services of their own: what lives here is the machinery *around* them.

mod instance;
mod on_demand;

use lb_log_targets::blend;
use overwatch::services::relay::RelayError;

pub use crate::orchestrator::{instance::Instance, on_demand::OnDemandServiceMode};

const LOG_TARGET: &str = blend::service::ORCHESTRATOR;

/// A mode could not be started or stopped.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Overwatch error: {0}")]
    Overwatch(#[from] overwatch::DynError),
    #[error("Overwatch relay error: {0}")]
    OverwatchRelay(#[from] RelayError),
}
