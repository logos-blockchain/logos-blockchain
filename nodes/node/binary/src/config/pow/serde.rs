use core::num::NonZeroU64;

use lb_core::mantle::Value;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_pow_service::{AutoClaimSettings, PoWMiningSettings};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Config {
    /// Tuning for the CPU-heavy ticket search (thread pool and per-block
    /// concurrency). Optional: omitting it keeps the defaults.
    #[serde(default)]
    pub mining: PoWMiningSettings,
    /// Unattended claiming: the keys to pay, the balance each should reach,
    /// and how often to try. Optional: omitting it leaves auto-claim off, so
    /// rewards are only claimed through the `PoW` claim endpoint.
    #[serde(default)]
    pub auto_claim: AutoClaimConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct AutoClaimConfig {
    /// Keys to pay, each with the balance it should reach. Empty disables
    /// auto-claim.
    pub targets: Vec<ClaimTarget>,
    /// Period between claim attempts.
    pub tick: AutoClaimTick,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct ClaimTarget {
    pub public_key: ZkPublicKey,
    /// Balance, in tokens, this key should reach.
    #[serde(default = "default_claim_threshold")]
    pub threshold: Value,
}

#[must_use]
pub const fn default_claim_threshold() -> Value {
    2_000_000_000
}

/// Reads as `{ unit: seconds, value: 300 }`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
#[serde(default)]
pub struct AutoClaimTick {
    pub unit: TickUnit,
    pub value: NonZeroU64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum TickUnit {
    Seconds,
    Slots,
}

impl Default for AutoClaimTick {
    fn default() -> Self {
        Self {
            unit: TickUnit::Seconds,
            value: NonZeroU64::new(10).expect("10 is non-zero"),
        }
    }
}

impl From<AutoClaimConfig> for AutoClaimSettings {
    fn from(AutoClaimConfig { targets, tick }: AutoClaimConfig) -> Self {
        Self {
            targets: targets
                .into_iter()
                .map(
                    |ClaimTarget {
                         public_key,
                         threshold,
                     }| lb_pow_service::ClaimTarget {
                        public_key,
                        threshold,
                    },
                )
                .collect(),
            tick: match tick.unit {
                TickUnit::Seconds => lb_pow_service::AutoClaimTick::Seconds(tick.value),
                TickUnit::Slots => lb_pow_service::AutoClaimTick::Slots(tick.value),
            },
        }
    }
}
