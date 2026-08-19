use core::num::NonZeroU32;
use std::num::{NonZero, NonZeroU64};

use lb_cryptarchia_engine::{Epoch, Slot};
pub use lb_groth16::ModulusShift;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_pol::LotteryConstants;

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

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
// TODO: Add reward difficulty parameters here. For now only the ones used for
// Blend are included.
pub struct PoWConfig {
    pub blend: BlendPoWConfig,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BlendPoWConfig {
    /// `BLEND_DIFFICULTY_BASE`: the threshold in effect at exactly the
    /// reference load, and the threshold the chain starts from.
    pub base_difficulty: ModulusShift,
    pub target_transactions_per_block: NonZeroU64,
    pub max_step: NonZeroU64,
    pub damping_num: NonZeroU32,
    // The offset from the denominator from the numerator.
    // E.g. for a fraction of 1/2, `blend_damping_num` would be 1 and `blend_damping_den_offset`
    // would be 1.
    // For an integer number of steps, `blend_damping_den_offset` would be 0, so the fraction would
    // be `blend_damping_num`/`blend_damping_num`.
    pub damping_den_offset: u32,
}

impl BlendPoWConfig {
    /// The damping exponent `alpha = a / b <= 1`, as the pair `(a, b)`.
    ///
    /// Both are exponents applied to big integers, so they must stay small —
    /// `alpha` is a simple fraction such as `1/2`, not a high-precision ratio.
    #[must_use]
    pub const fn damping_exponent(&self) -> (u32, NonZeroU32) {
        let numerator = self.damping_num.get();
        let denominator = NonZeroU32::new(numerator.strict_add(self.damping_den_offset))
            .expect("Numerator is non-zero, so denominator must be non-zero as well since it's a non-negative offset from the numerator.");
        (numerator, denominator)
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
    pub use lb_groth16::ModulusShift;
    use lb_utils::math::{NonNegativeF64, NonNegativeRatio};

    use crate::{
        config::{BlendPoWConfig, PoWConfig},
        mantle::sdp::{ServiceRewardsParameters, rewards::blend::RewardsParameters},
    };

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
            NonZero::new(12).unwrap(),
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
            pow_config: PoWConfig {
                blend: BlendPoWConfig {
                    base_difficulty: ModulusShift::new::<19>(),
                    damping_den_offset: 0,
                    damping_num: 1.try_into().unwrap(),
                    max_step: 1.try_into().unwrap(),
                    target_transactions_per_block: 1.try_into().unwrap(),
                },
            },
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
            NonZero::new(12).unwrap(),
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
            pow_config: PoWConfig {
                blend: BlendPoWConfig {
                    base_difficulty: ModulusShift::new::<19>(),
                    damping_den_offset: 0,
                    damping_num: 1.try_into().unwrap(),
                    max_step: 1.try_into().unwrap(),
                    target_transactions_per_block: 1.try_into().unwrap(),
                },
            },
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
            NonZero::new(12).unwrap(),
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
            pow_config: PoWConfig {
                blend: BlendPoWConfig {
                    base_difficulty: ModulusShift::new::<19>(),
                    damping_den_offset: 0,
                    damping_num: 1.try_into().unwrap(),
                    max_step: 1.try_into().unwrap(),
                    target_transactions_per_block: 1.try_into().unwrap(),
                },
            },
        };
        assert_eq!(config.epoch(1.into()), 0);
        assert_eq!(config.epoch(100.into()), 1);
        assert_eq!(config.epoch(101.into()), 1);
        assert_eq!(config.epoch(200.into()), 2);
    }
}
