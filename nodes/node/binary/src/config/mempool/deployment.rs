use core::time::Duration;

use lb_tx_service::backend::DEFAULT_TX_TTL;
use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub pubsub_topic: String,
    /// How long a pending transaction may stay in the mempool before it is
    /// evicted. `None` disables expiry-based eviction.
    #[serde_as(as = "Option<MinimalBoundedDuration<1, SECOND>>")]
    #[serde(default = "default_tx_ttl")]
    pub tx_ttl: Option<Duration>,
}

#[must_use]
pub const fn default_tx_ttl() -> Option<Duration> {
    Some(DEFAULT_TX_TTL)
}
