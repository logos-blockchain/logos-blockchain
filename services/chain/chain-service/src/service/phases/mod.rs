mod awaiting_genesis_time;
mod following;
mod ibd;
mod pbp;

use core::fmt::Debug;

pub use following::Following;
pub use ibd::InitialBlockDownload;
pub use pbp::ProlongedBootstrapPeriod;
use serde::{Deserialize, Serialize};

/// A phase of the chain service.
pub trait Phase: Send + Sync + Debug {
    /// The tag reported by [`Query::Info`](crate::Query::Info) while this
    /// phase is running.
    const TAG: PhaseTag;
}

/// Phase tags
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum PhaseTag {
    /// The genesis time is in the future. Only read-only queries are served.
    AwaitingGenesisTime,
    /// Applying blocks while waiting for Initial Block Download to complete.
    InitialBlockDownload,
    /// The Prolonged Bootstrap Period is running.
    ProlongedBootstrapPeriod,
    /// Following the chain in real time
    Following,
}
