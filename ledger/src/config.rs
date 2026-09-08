use core::num::NonZeroU32;
use std::num::{NonZero, NonZeroU64, NonZeroU128};

use lb_core::mantle::ops::pow::PowReward;
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
    /// [`RewardPoWConfig::validate`] at config-load time, and the third is a
    /// `u64`, so the product fits but need not fit in a `u64`.
    #[must_use]
    pub fn claim_rate_denominator(&self) -> NonZeroU128 {
        let configured = self.pow_config.reward.claim_rate_scale();
        let denominator =
            u128::from(configured.get()) * u128::from(self.expected_blocks_per_epoch().get());
        NonZeroU128::new(denominator).expect("product of non-zero values is non-zero")
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
pub struct PoWConfig {
    pub blend: BlendPoWConfig,
    pub reward: RewardPoWConfig,
}

/// Deployment-configurable parameters for the token-reward `PoW` role.
///
/// Covers the genesis endowment, the reward-difficulty (`d_reward`) EMA
/// controller, the per-epoch payout rate, and the claim acceptance window.
/// There is deliberately no `Default`: every value must be supplied by the
/// deployment configuration. A `rate_num` of `0` disables claiming entirely;
/// the shipped deployments enable it.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
// Validate the deployment-controlled invariants on deserialization, so an
// invalid config is rejected at load time (see [`RewardPoWConfig::validate`])
// instead of panicking later while processing consensus state.
#[serde(try_from = "RewardPoWConfigFields")]
pub struct RewardPoWConfig {
    /// `R_PoW` genesis: initial balance of the reward pool.
    pub reward_pool_genesis: PowReward,
    /// `sigma_e` genesis: initial per-claim reward.
    pub epoch_reward_genesis: PowReward,
    /// `d_reward` at genesis, as the exponent `n` in `p / 2^n` — the same way
    /// [`BlendPoWConfig::base_difficulty`] states the Blend threshold.
    ///
    /// Stated directly rather than derived: a difficulty is a fraction of the
    /// scalar field, and the retarget controller can only ever scale a target
    /// it is already given. Seeding it from a token amount cannot express a
    /// field-scale value, so the chain would start ~60 orders of magnitude
    /// too hard. Spec: 26.
    pub initial_difficulty: ModulusShift,
    /// EMA smoothing factor `F` (weight of the prior estimate). Must not
    /// exceed [`Self::ema_smoothing_precision`].
    pub ema_smoothing_factor: u64,
    /// EMA smoothing precision `P`; the smoothing fraction is `F / P`.
    pub ema_smoothing_precision: NonZeroU64,
    /// Target reward claims per block the controller aims for.
    pub target_claims_per_block: u64,
    /// Numerator of the per-epoch payout rate. `0` disables claiming.
    pub rate_num: u64,
    /// Denominator scale of the per-epoch payout rate.
    pub rate_den: NonZeroU64,
    /// Expected number of reward claims per block, a factor of the payout-rate
    /// denominator. The remaining factor, the expected number of blocks per
    /// epoch, is derived from the consensus schedule — see
    /// [`Config::expected_blocks_per_epoch`].
    pub target_claim_per_block: NonZeroU64,
    /// Acceptance window, in slots: how far back a claim's anchor block (and
    /// its nullifier) may be from the current block.
    pub slot_window: NonZeroU64,
}

/// Wire representation of [`RewardPoWConfig`], deserialized first so its
/// invariants can be checked before a validated [`RewardPoWConfig`] is built.
#[derive(serde::Deserialize)]
struct RewardPoWConfigFields {
    reward_pool_genesis: PowReward,
    epoch_reward_genesis: PowReward,
    initial_difficulty: ModulusShift,
    ema_smoothing_factor: u64,
    ema_smoothing_precision: NonZeroU64,
    target_claims_per_block: u64,
    rate_num: u64,
    rate_den: NonZeroU64,
    target_claim_per_block: NonZeroU64,
    slot_window: NonZeroU64,
}

impl TryFrom<RewardPoWConfigFields> for RewardPoWConfig {
    type Error = RewardPoWConfigError;

    fn try_from(fields: RewardPoWConfigFields) -> Result<Self, Self::Error> {
        let config = Self {
            reward_pool_genesis: fields.reward_pool_genesis,
            epoch_reward_genesis: fields.epoch_reward_genesis,
            initial_difficulty: fields.initial_difficulty,
            ema_smoothing_factor: fields.ema_smoothing_factor,
            ema_smoothing_precision: fields.ema_smoothing_precision,
            target_claims_per_block: fields.target_claims_per_block,
            rate_num: fields.rate_num,
            rate_den: fields.rate_den,
            target_claim_per_block: fields.target_claim_per_block,
            slot_window: fields.slot_window,
        };
        config.validate()?;
        Ok(config)
    }
}

/// Invariant violations in a [`RewardPoWConfig`], surfaced at config-load time.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum RewardPoWConfigError {
    #[error(
        "EMA smoothing factor ({factor}) must not exceed EMA smoothing precision ({precision})"
    )]
    EmaSmoothingFactorExceedsPrecision { factor: u64, precision: NonZeroU64 },
    #[error(
        "claim rate scale overflows u64: rate_den ({rate_den}) * \
         target_claim_per_block ({target_claim_per_block})"
    )]
    ClaimRateScaleOverflow {
        rate_den: NonZeroU64,
        target_claim_per_block: NonZeroU64,
    },
}

impl RewardPoWConfig {
    /// Check the invariants the reward-difficulty controller and payout-rate
    /// arithmetic rely on. Run on deserialization (see the `try_from` on this
    /// type) so a bad deployment config is rejected when it is loaded rather
    /// than panicking later while initializing or processing consensus state.
    ///
    /// # Errors
    ///
    /// Returns [`RewardPoWConfigError`] if the EMA smoothing factor exceeds the
    /// precision, or the configured part of the payout-rate denominator
    /// overflows `u64`.
    pub fn validate(&self) -> Result<(), RewardPoWConfigError> {
        if self.ema_smoothing_factor > self.ema_smoothing_precision.get() {
            return Err(RewardPoWConfigError::EmaSmoothingFactorExceedsPrecision {
                factor: self.ema_smoothing_factor,
                precision: self.ema_smoothing_precision,
            });
        }
        // Discard the value; this call only checks that it does not overflow.
        self.checked_claim_rate_scale()?;
        Ok(())
    }

    /// The configured factors of the payout-rate denominator,
    /// `rate_den * target_claim_per_block`, or
    /// [`RewardPoWConfigError::ClaimRateScaleOverflow`] if they overflow
    /// `u64`. The remaining factor, `expected_blocks_per_epoch`, is derived
    /// from the consensus schedule and applied by
    /// [`Config::claim_rate_denominator`].
    fn checked_claim_rate_scale(&self) -> Result<NonZeroU64, RewardPoWConfigError> {
        let product = self
            .rate_den
            .get()
            .checked_mul(self.target_claim_per_block.get())
            .ok_or(RewardPoWConfigError::ClaimRateScaleOverflow {
                rate_den: self.rate_den,
                target_claim_per_block: self.target_claim_per_block,
            })?;
        Ok(NonZeroU64::new(product).expect("product of non-zero values is non-zero"))
    }

    /// The configured factors of the payout-rate denominator:
    /// `rate_den * target_claim_per_block`.
    ///
    /// The product is guaranteed not to overflow by [`Self::validate`], which
    /// runs on deserialization.
    #[must_use]
    pub fn claim_rate_scale(&self) -> NonZeroU64 {
        self.checked_claim_rate_scale()
            .expect("claim rate scale overflow is rejected at config-load time")
    }
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
        num::{NonZero, NonZeroU64, NonZeroU128},
        sync::Arc,
    };

    use lb_core::sdp::{MinStake, ServiceParameters, ServiceType};
    use lb_cryptarchia_engine::EpochConfig;
    pub use lb_groth16::ModulusShift;
    use lb_utils::math::{NonNegativeRatio, PositiveF64};

    use crate::{
        config::{BlendPoWConfig, PoWConfig, RewardPoWConfig, RewardPoWConfigError},
        mantle::sdp::{ServiceRewardsParameters, rewards::blend::RewardsParameters},
    };

    /// A reward config with claiming disabled, standing in for a real
    /// deployment config in tests that don't exercise the reward parameters.
    fn disabled_reward_config() -> RewardPoWConfig {
        RewardPoWConfig {
            reward_pool_genesis: 1_000_000_000,
            epoch_reward_genesis: 1_000_000,
            initial_difficulty: ModulusShift::new::<26>(),
            ema_smoothing_factor: 9,
            ema_smoothing_precision: NonZeroU64::new(10).unwrap(),
            target_claims_per_block: 100,
            rate_num: 0,
            rate_den: NonZeroU64::MIN,
            target_claim_per_block: NonZeroU64::MIN,
            slot_window: NonZeroU64::new(100).expect("100 is non-zero"),
        }
    }

    #[test]
    fn valid_reward_config_passes_validation() {
        assert_eq!(disabled_reward_config().validate(), Ok(()));
    }

    #[test]
    fn reward_config_rejects_ema_factor_above_precision() {
        let mut config = disabled_reward_config();
        config.ema_smoothing_factor = config.ema_smoothing_precision.get() + 1;
        assert_eq!(
            config.validate(),
            Err(RewardPoWConfigError::EmaSmoothingFactorExceedsPrecision {
                factor: config.ema_smoothing_factor,
                precision: config.ema_smoothing_precision,
            })
        );
    }

    #[test]
    fn reward_config_accepts_ema_factor_equal_to_precision() {
        // F == P is q = 1 (full smoothing): a valid boundary, not a rejection.
        let mut config = disabled_reward_config();
        config.ema_smoothing_factor = config.ema_smoothing_precision.get();
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn reward_config_rejects_claim_rate_scale_overflow() {
        // rate_den * target_claim_per_block would exceed u64::MAX.
        let mut config = disabled_reward_config();
        config.rate_den = NonZeroU64::MAX;
        config.target_claim_per_block = NonZeroU64::new(2).unwrap();
        assert_eq!(
            config.validate(),
            Err(RewardPoWConfigError::ClaimRateScaleOverflow {
                rate_den: config.rate_den,
                target_claim_per_block: config.target_claim_per_block,
            })
        );
    }

    #[test]
    fn deserialize_rejects_invalid_reward_config() {
        // The invariant check runs on deserialization, so an invalid config is
        // rejected at config-load time instead of panicking later. The values
        // are serialized from a plain struct (bypassing validation) to emulate
        // a hand-written deployment config.
        let mut invalid = disabled_reward_config();
        invalid.ema_smoothing_factor = invalid.ema_smoothing_precision.get() + 1;
        let json = serde_json::to_string(&invalid).expect("serialize");
        let error = serde_json::from_str::<RewardPoWConfig>(&json)
            .expect_err("invalid reward config must be rejected on deserialization");
        assert!(
            error.to_string().contains("EMA smoothing factor"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deserialize_accepts_valid_reward_config() {
        let valid = disabled_reward_config();
        let json = serde_json::to_string(&valid).expect("serialize");
        let restored: RewardPoWConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, valid);
    }

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
                        message_frequency_per_round: PositiveF64::try_from(1.0).unwrap(),
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
                reward: disabled_reward_config(),
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
                        message_frequency_per_round: PositiveF64::try_from(1.0).unwrap(),
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
                reward: disabled_reward_config(),
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
                        message_frequency_per_round: PositiveF64::try_from(1.0).unwrap(),
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
                reward: disabled_reward_config(),
            },
        };
        assert_eq!(config.epoch(1.into()), 0);
        assert_eq!(config.epoch(100.into()), 1);
        assert_eq!(config.epoch(101.into()), 1);
        assert_eq!(config.epoch(200.into()), 2);
    }
}
