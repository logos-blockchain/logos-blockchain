use core::{num::NonZeroU64, time::Duration};

use lb_tx_service::backend::DEFAULT_TX_TTL;
use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    /// How long a pending transaction may stay in the mempool before it is
    /// evicted. `None` disables expiry-based eviction.
    #[serde_as(as = "Option<MinimalBoundedDuration<1, SECOND>>")]
    #[serde(default = "default_tx_ttl")]
    pub tx_ttl: Option<Duration>,
    #[serde(default = "default_seen_items_subscription_channel_size")]
    pub seen_items_subscription_channel_size: NonZeroU64,
    /// How many slots past its acceptance window a transaction carrying a
    /// `PoW` reward claim stays in the mempool before it is evicted. `0`
    /// evicts it in the first slot the ledger would reject it.
    #[serde(default)]
    pub pow_claim_grace_slots: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tx_ttl: default_tx_ttl(),
            seen_items_subscription_channel_size: default_seen_items_subscription_channel_size(),
            pow_claim_grace_slots: 0,
        }
    }
}

#[must_use]
pub const fn default_tx_ttl() -> Option<Duration> {
    Some(DEFAULT_TX_TTL)
}

#[must_use]
pub const fn default_seen_items_subscription_channel_size() -> NonZeroU64 {
    NonZeroU64::new(64).unwrap()
}
