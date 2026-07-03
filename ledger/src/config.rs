use std::num::{NonZero, NonZeroU64};

use lb_cryptarchia_engine::{Epoch, Slot};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_pol::LotteryConstants;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub epoch_config: lb_cryptarchia_engine::EpochConfig,
    pub consensus_config: lb_cryptarchia_engine::Config,
    pub sdp_config: crate::mantle::sdp::Config,
    #[serde(default)]
    pub faucet_pk: Option<ZkPublicKey>,
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

    /// The slot at which the nonce for a given epoch is snapshotted
    ///
    /// If epoch length is 100 slots, and epoch phases are 3/3/4 slots,
    /// the nonce for epoch 1 will be snapshotted at slot 60, which is the 1st
    /// slot of the last phase of epoch 0.
    #[must_use]
    pub fn nonce_snapshot(&self, epoch: Epoch) -> Slot {
        let offset = self.nonce_contribution_period();
        let base =
            u64::from(epoch.strict_sub(1.into()).into_inner()).strict_mul(self.epoch_length());
        base.strict_add(offset).into()
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

    /// The slot at which the total stake for a given epoch is snapshotted
    ///
    /// If epoch length is 100 slots, and epoch phases are 3/3/4 slots,
    /// the total stake for epoch 1 will be snapshotted at slot 60, which is the
    /// 1st slot of the last phase of epoch 0.
    #[must_use]
    pub fn total_stake_snapshot(&self, epoch: Epoch) -> Slot {
        self.nonce_snapshot(epoch)
    }

    /// The number of slots in Stake Distribution Snapshot + Buffer phases
    #[must_use]
    pub fn total_stake_inference_period(&self) -> u64 {
        self.nonce_contribution_period()
    }

    /// The slot at which the stake distribution for a given epoch is
    /// snapshotted, i.e., the first slot of the previous epoch.
    #[must_use]
    pub fn stake_distribution_snapshot(&self, epoch: Epoch) -> Slot {
        (u64::from(epoch.strict_sub(1.into()).into_inner()) * self.epoch_length()).into()
    }

    #[must_use]
    pub fn epoch(&self, slot: Slot) -> Epoch {
        self.epoch_config
            .epoch(slot, self.consensus_config.base_period_length())
    }

    #[must_use]
    pub fn last_slot(&self, epoch: Epoch) -> Slot {
        self.epoch_config
            .last_slot(epoch, self.consensus_config.base_period_length())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZero, NonZeroU64},
        sync::Arc,
    };

    use lb_core::sdp::{MinStake, ServiceParameters, ServiceType};
    use lb_cryptarchia_engine::EpochConfig;
    use lb_utils::math::{NonNegativeF64, NonNegativeRatio};

    use crate::mantle::sdp::{ServiceRewardsParameters, rewards::blend::RewardsParameters};

    #[test]
    fn epoch_snapshots() {
        let epoch_config = EpochConfig {
            epoch_stake_distribution_stabilization: NonZero::new(3u8).unwrap(),
            epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
            epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
        };
        let consensus_config = lb_cryptarchia_engine::Config::new(
            NonZero::new(5).unwrap(),
            NonNegativeRatio::new(1, 2.try_into().unwrap()),
            1f64.try_into().expect("1 > 0"),
        );
        let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

        let config = super::Config {
            epoch_config,
            consensus_config,
            sdp_config: crate::mantle::sdp::Config {
                service_params: Arc::new(
                    [(
                        ServiceType::BlendNetwork,
                        ServiceParameters {
                            inactivity_period: 2.try_into().unwrap(),
                            epoch: 0.into(),
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: RewardsParameters {
                        rounds_per_epoch: epoch_length.try_into().unwrap(),
                        message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                        num_blend_layers: NonZeroU64::new(3).unwrap(),
                        minimum_network_size: NonZeroU64::new(1).unwrap(),
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
        };
        assert_eq!(config.epoch_length(), 100);
        assert_eq!(config.nonce_snapshot(1.into()), 60.into());
        assert_eq!(config.nonce_snapshot(2.into()), 160.into());
        assert_eq!(config.total_stake_snapshot(1.into()), 60.into());
        assert_eq!(config.total_stake_snapshot(2.into()), 160.into());
        assert_eq!(config.stake_distribution_snapshot(1.into()), 0.into());
        assert_eq!(config.stake_distribution_snapshot(2.into()), 100.into());
    }

    fn epoch_zero_test_config() -> super::Config {
        let epoch_config = EpochConfig {
            epoch_stake_distribution_stabilization: NonZero::new(3u8).unwrap(),
            epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
            epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
        };
        let consensus_config = lb_cryptarchia_engine::Config::new(
            NonZero::new(5).unwrap(),
            NonNegativeRatio::new(1, 2.try_into().unwrap()),
            1f64.try_into().expect("1 > 0"),
        );
        let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());
        super::Config {
            epoch_config,
            consensus_config,
            sdp_config: crate::mantle::sdp::Config {
                service_params: Arc::new(
                    [(
                        ServiceType::BlendNetwork,
                        ServiceParameters {
                            inactivity_period: 2.try_into().unwrap(),
                            epoch: 0.into(),
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: RewardsParameters {
                        rounds_per_epoch: epoch_length.try_into().unwrap(),
                        message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                        num_blend_layers: NonZeroU64::new(3).unwrap(),
                        minimum_network_size: NonZeroU64::new(1).unwrap(),
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

    #[test]
    #[should_panic(expected = "attempt to subtract with overflow")]
    fn stake_distribution_snapshot_panics_at_epoch_zero() {
        let config = epoch_zero_test_config();
        let _ = config.stake_distribution_snapshot(0.into());
    }

    #[test]
    #[should_panic(expected = "attempt to subtract with overflow")]
    fn nonce_snapshot_panics_at_epoch_zero() {
        let config = epoch_zero_test_config();
        let _ = config.nonce_snapshot(0.into());
    }

    #[test]
    fn slot_to_epoch() {
        let epoch_config = EpochConfig {
            epoch_stake_distribution_stabilization: NonZero::new(3u8).unwrap(),
            epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
            epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
        };
        let consensus_config = lb_cryptarchia_engine::Config::new(
            NonZero::new(5).unwrap(),
            NonNegativeRatio::new(1, 2.try_into().unwrap()),
            1f64.try_into().expect("1 > 0"),
        );
        let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

        let config = super::Config {
            epoch_config,
            consensus_config,
            sdp_config: crate::mantle::sdp::Config {
                service_params: Arc::new(
                    [(
                        ServiceType::BlendNetwork,
                        ServiceParameters {
                            inactivity_period: 2.try_into().unwrap(),
                            epoch: 0.into(),
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: RewardsParameters {
                        rounds_per_epoch: epoch_length.try_into().unwrap(),
                        message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                        num_blend_layers: NonZeroU64::new(3).unwrap(),
                        minimum_network_size: NonZeroU64::new(1).unwrap(),
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
        };
        assert_eq!(config.epoch(1.into()), 0);
        assert_eq!(config.epoch(100.into()), 1);
        assert_eq!(config.epoch(101.into()), 1);
        assert_eq!(config.epoch(200.into()), 2);
    }
}
