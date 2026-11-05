use core::num::NonZeroU64;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
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
            seen_items_subscription_channel_size: default_seen_items_subscription_channel_size(),
            pow_claim_grace_slots: 0,
        }
    }
}

#[must_use]
pub const fn default_seen_items_subscription_channel_size() -> NonZeroU64 {
    NonZeroU64::new(64).unwrap()
}
