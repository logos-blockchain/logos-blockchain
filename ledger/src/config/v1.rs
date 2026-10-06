//! The ledger's config in version 1.

use core::num::NonZero;
use std::num::{NonZeroU64, NonZeroU128};

use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_pol::LotteryConstants;

use crate::config::PoWConfig;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub epoch_config: lb_cryptarchia_engine::EpochConfig,
    pub consensus_config: lb_cryptarchia_engine::Config,
    pub sdp_config: crate::mantle::sdp::Config,
    #[serde(default)]
    pub faucet_pk: Option<ZkPublicKey>,
    pub pow_config: PoWConfig,
}

impl Config {
    #[must_use]
    pub const fn lottery_constants(&self) -> &LotteryConstants {
        self.consensus_config.lottery_constants()
    }

    #[must_use]
    pub const fn base_period_length(&self) -> NonZero<u64> {
        self.consensus_config.base_period_length()
    }

    #[must_use]
    pub const fn epoch_length(&self) -> u64 {
        self.epoch_config
            .epoch_length(self.consensus_config.base_period_length())
    }

    /// `N_b`: the number of blocks a full epoch is expected to produce.
    ///
    /// Deliberately derived rather than configured. It is fixed by the epoch
    /// schedule and the slot activation coefficient — it works out to `10k`
    /// for the standard 3/3/4 phase split — so a hand-set value that disagreed
    /// with them would silently mis-scale the per-claim `PoW` reward.
    #[must_use]
    pub const fn expected_blocks_per_epoch(&self) -> NonZeroU64 {
        lb_cryptarchia_engine::expected_blocks_per_epoch(
            self.epoch_length(),
            self.consensus_config.slot_activation_coeff(),
        )
    }

    /// Full denominator of the per-epoch payout rate:
    /// `rate_den * target_claim_per_block * expected_blocks_per_epoch`.
    ///
    /// Widened to `u128`: the first two factors are bounded to `u64` by
    /// [`RewardPoWConfig::validate`](crate::config::RewardPoWConfig::validate)
    /// at config-load time, and the third is a `u64`, so the product fits
    /// but need not fit in a `u64`.
    #[must_use]
    pub fn claim_rate_denominator(&self) -> NonZeroU128 {
        let configured = self.pow_config.reward.claim_rate_scale();
        let denominator =
            u128::from(configured.get()) * u128::from(self.expected_blocks_per_epoch().get());
        NonZeroU128::new(denominator).expect("product of non-zero values is non-zero")
    }

    /// The number of slots in Stake Distribution Snapshot + Buffer phases
    #[must_use]
    pub fn nonce_contribution_period(&self) -> u64 {
        self.base_period_length().get().strict_mul(
            u64::from(NonZeroU64::from(
                self.epoch_config.epoch_period_nonce_buffer,
            ))
            .strict_add(u64::from(NonZeroU64::from(
                self.epoch_config.epoch_stake_distribution_stabilization,
            ))),
        )
    }

    /// The number of slots in Stake Distribution Snapshot + Buffer phases
    #[must_use]
    pub fn total_stake_inference_period(&self) -> u64 {
        self.nonce_contribution_period()
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU64, NonZeroU128};

    use crate::config::tests::epoch_zero_test_config;

    #[test]
    fn expected_blocks_per_epoch_is_ten_k() {
        // k = 5 and f = 1/2, so the epoch spans 100 slots and half of them are
        // expected to carry a block — the `10k` the payout rate assumes.
        let config = epoch_zero_test_config();
        assert_eq!(config.epoch_length(), 100);
        assert_eq!(
            config.expected_blocks_per_epoch(),
            NonZeroU64::new(50).unwrap()
        );
        assert_eq!(
            u64::from(config.expected_blocks_per_epoch()),
            10 * u64::from(config.consensus_config.security_param().get())
        );
    }

    #[test]
    fn claim_rate_denominator_folds_in_the_derived_block_count() {
        let mut config = epoch_zero_test_config();
        config.pow_config.reward.rate_den = NonZeroU64::new(10).unwrap();
        config.pow_config.reward.target_claim_per_block = NonZeroU64::new(3).unwrap();
        // rate_den * target_claim_per_block * expected_blocks_per_epoch.
        assert_eq!(
            config.claim_rate_denominator(),
            NonZeroU128::new(10 * 3 * 50).unwrap()
        );
    }
}
