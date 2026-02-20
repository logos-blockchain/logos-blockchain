use std::ops::RangeInclusive;

use lb_cryptarchia_engine::{Epoch, Slot};

use crate::Config;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone)]
pub struct BlockDensity {
    period_range: RangeInclusive<Slot>,
    density: u64,
}

impl BlockDensity {
    pub fn new(epoch: Epoch, config: &Config) -> Self {
        Self {
            period_range: Self::compute_period_range(epoch, config),
            density: 0,
        }
    }

    /// The range of slots used to compute the block density for a given epoch
    ///
    /// If epoch length is 100 slots, and epoch phases are 3/3/4 slots,
    /// the block density for epoch 2 will be computed during [200, 259],
    /// which is the Stake Distribution Snapshot + Buffer phases of epoch 2.
    fn compute_period_range(epoch: Epoch, config: &Config) -> RangeInclusive<Slot> {
        let snapshot_slot_for_next_epoch =
            config.total_stake_snapshot(epoch.saturating_add(1.into()));
        let start = snapshot_slot_for_next_epoch
            .saturating_sub(config.total_stake_inference_period().into());
        let end = snapshot_slot_for_next_epoch.saturating_sub(1.into());
        start..=end
    }

    pub fn increment_block_density(&mut self, new_slot: Slot) {
        if self.period_range.contains(&new_slot) {
            self.density += 1;
        }
    }

    pub const fn current_block_density(&self) -> u64 {
        self.density
    }

    #[cfg(test)]
    pub const fn period_range(&self) -> &RangeInclusive<Slot> {
        &self.period_range
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use lb_core::sdp::MinStake;
    use lb_utils::math::NonNegativeRatio;

    use super::*;
    use crate::mantle::sdp::{ServiceRewardsParameters, rewards::blend::RewardsParameters};

    #[test]
    fn test_initial_block_density_is_zero() {
        let density = BlockDensity::new(0.into(), &config());
        assert_eq!(density.period_range(), &(0.into()..=59.into()));
        assert_eq!(density.current_block_density(), 0);
    }

    #[test]
    fn test_increment_block_density() {
        let mut density = BlockDensity::new(1.into(), &config());
        assert_eq!(density.period_range(), &(100.into()..=159.into()));
        density.increment_block_density(Slot::from(100));
        assert_eq!(density.current_block_density(), 1);
        density.increment_block_density(Slot::from(159));
        assert_eq!(density.current_block_density(), 2);
        density.increment_block_density(Slot::from(140)); // slot order doesn't matter
        assert_eq!(density.current_block_density(), 3);
        density.increment_block_density(Slot::from(95)); // ignored
        assert_eq!(density.current_block_density(), 3); // not changed
        density.increment_block_density(Slot::from(160)); // ignored
        assert_eq!(density.current_block_density(), 3); // not changed
    }

    fn config() -> Config {
        Config {
            epoch_config: lb_cryptarchia_engine::EpochConfig {
                epoch_stake_distribution_stabilization: 3.try_into().unwrap(),
                epoch_period_nonce_buffer: 3.try_into().unwrap(),
                epoch_period_nonce_stabilization: 4.try_into().unwrap(),
            },
            consensus_config: lb_cryptarchia_engine::Config::new(
                5.try_into().unwrap(),
                NonNegativeRatio::new(1, 2.try_into().unwrap()),
                1f64.try_into().unwrap(),
            ),
            // not used in the tests
            sdp_config: crate::mantle::sdp::Config {
                service_params: Arc::new(HashMap::new()),
                service_rewards_params: ServiceRewardsParameters {
                    blend: RewardsParameters {
                        rounds_per_session: 10.try_into().unwrap(),
                        message_frequency_per_round: 1.0.try_into().unwrap(),
                        num_blend_layers: 3.try_into().unwrap(),
                        minimum_network_size: 1.try_into().unwrap(),
                        data_replication_factor: 0,
                        activity_threshold_sensitivity: 1,
                    },
                },
                min_stake: MinStake {
                    threshold: 1,
                    timestamp: 0,
                },
            },
            faucet_pk: None,
        }
    }
}
