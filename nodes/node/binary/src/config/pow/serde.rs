use core::num::{NonZeroU64, NonZeroUsize};

use lb_core::mantle::Value;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_pow_service::{AutoClaimSettings, PoWMiningSettings};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Config {
    /// Tuning for the CPU-heavy ticket search (thread pool and per-block
    /// concurrency). Optional: omitting it keeps the defaults.
    #[serde(default)]
    pub mining: MiningConfig,
    /// Unattended claiming: the keys to pay, the balance each should reach,
    /// and how often to try. Optional: omitting it leaves auto-claim off, so
    /// rewards are only claimed through the `PoW` claim endpoint.
    #[serde(default)]
    pub auto_claim: AutoClaimConfig,
}

/// What the `PoW` ticket search runs with, and whether it starts by itself.
///
/// The service's `PoWMiningSettings` is also what a runtime settings update
/// carries, so it keeps its own derives; this is the node configuration's view
/// of it, with the defaults defined here.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct MiningConfig {
    /// Worker threads in the ticket-search pool. One thread when omitted; an
    /// explicit `null` lets rayon pick its default (one per logical CPU).
    pub max_threads: Option<NonZeroUsize>,
    /// Ticket-search attempts kept in flight concurrently per block.
    pub max_tickets_per_block: NonZeroUsize,
    /// Start mining as soon as the chain reaches `Online`, as if `StartMining`
    /// had been received. Off when omitted. `PUT /pow/mining/stop` and `start`
    /// keep working at runtime.
    pub start_on_boot: bool,
}

impl Default for MiningConfig {
    fn default() -> Self {
        let PoWMiningSettings {
            max_threads,
            max_tickets_per_block,
        } = PoWMiningSettings::default();
        Self {
            max_threads,
            max_tickets_per_block,
            start_on_boot: false,
        }
    }
}

impl MiningConfig {
    /// The service's tuning, and the start-on-boot flag it takes separately.
    #[must_use]
    pub const fn into_service_parts(self) -> (PoWMiningSettings, bool) {
        (
            PoWMiningSettings {
                max_threads: self.max_threads,
                max_tickets_per_block: self.max_tickets_per_block,
            },
            self.start_on_boot,
        )
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> Config {
        serde_yaml::from_str(yaml).expect("valid pow config")
    }

    #[test]
    fn mining_stays_off_and_keeps_its_tuning_when_the_section_is_omitted() {
        let mining = config("{}").mining;
        assert!(!mining.start_on_boot);
        assert_eq!(mining, MiningConfig::default());
        let (settings, start_on_boot) = mining.into_service_parts();
        assert_eq!(settings, PoWMiningSettings::default());
        assert!(!start_on_boot);
    }

    #[test]
    fn start_on_boot_is_read_from_the_mining_section() {
        let mining = config("mining:\n  start_on_boot: true\n").mining;
        let defaults = MiningConfig::default();
        assert!(mining.start_on_boot);
        assert_eq!(mining.max_threads, defaults.max_threads);
        assert_eq!(mining.max_tickets_per_block, defaults.max_tickets_per_block);
        assert!(mining.into_service_parts().1);
    }

    #[test]
    fn the_existing_tuning_keys_still_mean_what_they_did() {
        let mining = config("mining:\n  max_threads: 3\n  max_tickets_per_block: 2\n").mining;
        assert_eq!(mining.max_threads, NonZeroUsize::new(3));
        assert_eq!(mining.max_tickets_per_block, NonZeroUsize::new(2).unwrap());
        assert!(!mining.start_on_boot);
    }

    #[test]
    fn an_explicit_null_for_max_threads_still_means_every_core() {
        assert_eq!(
            config("mining:\n  max_threads: null\n").mining.max_threads,
            None
        );
    }

    #[test]
    fn a_zero_thread_count_is_still_rejected() {
        assert!(serde_yaml::from_str::<Config>("mining:\n  max_threads: 0\n").is_err());
    }
}
