use std::ops::RangeInclusive;

use lb_cryptarchia_engine::{Epoch, Slot, UncleSlots};
use rpds::HashTrieSetSync;

use crate::Config;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockDensity {
    period_range: RangeInclusive<Slot>,
    /// The distinct slots occupied by the blocks of the chain and the uncles
    /// they reference, within the period.
    occupied_slots: HashTrieSetSync<Slot>,
}

impl BlockDensity {
    pub fn new(epoch: Epoch, config: &Config) -> Self {
        Self {
            period_range: Self::compute_period_range(epoch, config),
            occupied_slots: HashTrieSetSync::new_sync(),
        }
    }

    /// The range of slots used to compute the block density for a given epoch
    ///
    /// If epoch length is 100 slots, and epoch phases are 3/3/4 slots,
    /// the block density for epoch 2 will be computed during [200, 259],
    /// which is the Stake Distribution Snapshot + Buffer phases of epoch 2.
    fn compute_period_range(epoch: Epoch, config: &Config) -> RangeInclusive<Slot> {
        let snapshot_slot_for_next_epoch = config.total_stake_snapshot(epoch.strict_add(1.into()));
        let start = snapshot_slot_for_next_epoch
            .saturating_sub(config.total_stake_inference_period().into());
        let end = snapshot_slot_for_next_epoch.saturating_sub(1.into());
        start..=end
    }

    /// Marks the slots occupied by a block and the uncles it references.
    ///
    /// Skipped entirely if the block itself is outside the period, so that
    /// the blocks after the period cannot alter the density with the uncles
    /// they reference, even if the uncles are in the window.
    ///
    /// A slot occupied more than once is counted once.
    pub fn mark_occupied_slots(&mut self, block_slot: Slot, uncle_slots: &UncleSlots) {
        if !self.period_range.contains(&block_slot) {
            return;
        }
        self.occupied_slots.insert_mut(block_slot);
        for uncle_slot in uncle_slots.iter() {
            if self.period_range.contains(uncle_slot) {
                self.occupied_slots.insert_mut(*uncle_slot);
            }
        }
    }

    pub fn current_block_density(&self) -> u64 {
        self.occupied_slots.size() as u64
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
    fn test_mark_occupied_slots() {
        let mut density = BlockDensity::new(1.into(), &config());
        assert_eq!(density.period_range(), &(100.into()..=159.into()));
        density.mark_occupied_slots(Slot::from(100), &UncleSlots::default());
        assert_eq!(density.current_block_density(), 1);
        density.mark_occupied_slots(Slot::from(159), &UncleSlots::default());
        assert_eq!(density.current_block_density(), 2);
        // slot order doesn't matter
        density.mark_occupied_slots(Slot::from(140), &UncleSlots::default());
        assert_eq!(density.current_block_density(), 3);
        // blocks outside the period are ignored
        density.mark_occupied_slots(Slot::from(95), &UncleSlots::default());
        assert_eq!(density.current_block_density(), 3);
        density.mark_occupied_slots(Slot::from(160), &UncleSlots::default());
        assert_eq!(density.current_block_density(), 3);
    }

    #[test]
    fn test_mark_occupied_slots_with_uncles() {
        let mut density = BlockDensity::new(1.into(), &config());
        assert_eq!(density.period_range(), &(100.into()..=159.into()));

        // A block and its uncles are marked together.
        density.mark_occupied_slots(Slot::from(110), &[Slot::from(105)].into());
        assert_eq!(density.current_block_density(), 2);

        // A slot occupied more than once is counted once:
        // slot 105 by another uncle, and slot 110 by an uncle of another block.
        density.mark_occupied_slots(Slot::from(120), &[Slot::from(105), Slot::from(110)].into());
        assert_eq!(density.current_block_density(), 3);

        // Uncle slots outside the period are ignored.
        density.mark_occupied_slots(Slot::from(130), &[Slot::from(99)].into());
        assert_eq!(density.current_block_density(), 4);

        // A block outside the period is skipped entirely, even if its uncles
        // are within the period (late references).
        density.mark_occupied_slots(Slot::from(160), &[Slot::from(150)].into());
        assert_eq!(density.current_block_density(), 4);
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
                20.try_into().unwrap(), // W = 10 at f = 1/2
            ),
            // not used in the tests
            sdp_config: crate::mantle::sdp::Config {
                service_params: Arc::new(HashMap::new()),
                service_rewards_params: ServiceRewardsParameters {
                    blend: RewardsParameters {
                        rounds_per_epoch: 10.try_into().unwrap(),
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
