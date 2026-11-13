use std::num::{NonZero, NonZeroU32, NonZeroU64, NonZeroU128};

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};
use lb_core::{
    era::AcceptedForkDigests,
    mantle::{
        Value,
        gas::{Gas, GasProfile},
        ops::pow::{PowSlotWindows, PowTarget},
    },
};
use lb_cryptarchia_engine::{Epoch, EraSchedule, Slot};
pub use lb_groth16::ModulusShift;
use lb_pol::LotteryConstants;
use lb_time::era::EpochLength;
use lb_utils::math::{NonNegativeRatio, ProperFraction};

/// The config of every era: each block is applied under the config of the era
/// of its slot, and each epoch is settled under the config of its era.
pub type EraScheduledConfig = EraSchedule<Config>;

pub use lb_core::mantle::ConsensusExcludedKeys;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub epoch_config: lb_cryptarchia_engine::EpochConfig,
    pub consensus_config: lb_cryptarchia_engine::Config,
    /// The most transactions a block may hold.
    pub max_transactions: u16,
    /// The most bytes a block's transactions may take in their canonical
    /// encoding.
    pub max_transactions_size: u32,
    /// The most bytes of data an Operation may carry: an inscription, or a
    /// deposit's metadata.
    pub max_op_data_size: u32,
    pub sdp_config: crate::mantle::sdp::Config,
    pub pow_config: PoWConfig,
    pub accepted_fork_digests: AcceptedForkDigests,
    pub execution_market: ExecutionMarketConfig,
    pub storage_market: StorageMarketConfig,
    pub block_rewards: BlockRewardsConfig,
    pub gas_profile: GasProfile,
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

    /// The slot at which an epoch that starts at `first_slot` under this config
    /// snapshots the nonce and the total stake of the next epoch: the first
    /// slot of its last phase.
    ///
    /// If epoch length is 100 slots, and epoch phases are 3/3/4 slots, the
    /// epoch that starts at slot 100 snapshots them at slot 160.
    #[must_use]
    pub fn next_epoch_snapshot(&self, first_slot: Slot) -> Slot {
        Slot::new(
            first_slot
                .into_inner()
                .strict_add(self.nonce_contribution_period()),
        )
    }
}

impl EpochLength for Config {
    fn epoch_length(&self) -> NonZero<u64> {
        NonZero::new(self.epoch_config.epoch_length(self.base_period_length()))
            .expect("an epoch holds at least one slot")
    }
}

/// The config of the era `slot` belongs to.
#[must_use]
pub fn config_for_slot(eras: &EraScheduledConfig, slot: Slot) -> &Config {
    eras.at_slot(slot).parameters
}

/// The config of the era `epoch` belongs to.
#[must_use]
pub fn config_for_epoch(eras: &EraScheduledConfig, epoch: Epoch) -> &Config {
    eras.at_epoch(epoch).parameters
}

/// The acceptance window, in slots, of the `PoW` reward claims anchored to the
/// blocks of each era.
#[must_use]
pub fn pow_slot_windows(eras: &EraScheduledConfig) -> PowSlotWindows {
    eras.map(|era| era.parameters.pow_config.reward.slot_window)
}

/// The slot at which the nonce for a given epoch is snapshotted
///
/// If epoch length is 100 slots, and epoch phases are 3/3/4 slots,
/// the nonce for epoch 1 will be snapshotted at slot 60, which is the 1st
/// slot of the last phase of epoch 0.
#[must_use]
pub fn nonce_snapshot(eras: &EraScheduledConfig, non_genesis_epoch: NonZeroU32) -> Slot {
    let previous_epoch = Epoch::new(non_genesis_epoch.get() - 1);
    config_for_epoch(eras, previous_epoch)
        .next_epoch_snapshot(eras.starting_slot_for_epoch(previous_epoch))
}

/// The slot at which the stake distribution for a given epoch is
/// snapshotted, i.e., the first slot of the previous epoch.
#[must_use]
pub fn stake_distribution_snapshot(
    eras: &EraScheduledConfig,
    non_genesis_epoch: NonZeroU32,
) -> Slot {
    eras.starting_slot_for_epoch(Epoch::new(non_genesis_epoch.get() - 1))
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PoWConfig {
    pub blend: BlendPoWConfig,
    pub reward: RewardPoWConfig,
}

/// Deployment-configurable parameters for the token-reward `PoW` role.
///
/// Covers the reward-difficulty (`d_reward`) EMA controller, the per-epoch
/// payout rate, and the claim acceptance window. The pool the payout draws on
/// starts with the genesis parameters' `pow_reward_pool`.
/// There is deliberately no `Default`: every value must be supplied by the
/// deployment configuration. A `rate_num` of `0` disables claiming entirely;
/// the shipped deployments enable it.
///
/// A node's era digest commits to these fields through their derived encoding,
/// in declaration order.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, BinaryEncode)]
// Validate the deployment-controlled invariants on deserialization, so an
// invalid config is rejected at load time (see [`RewardPoWConfig::validate`])
// instead of panicking later while processing consensus state.
#[serde(try_from = "RewardPoWConfigFields")]
pub struct RewardPoWConfig {
    /// `d_reward` floor, as the exponent `n` in `p / 2^n` — the same way
    /// [`BlendPoWConfig::base_difficulty`] states the Blend threshold. It is
    /// both the genesis difficulty and the minimum: the retarget controller
    /// never eases the target past it.
    ///
    /// Stated directly rather than derived: a difficulty is a fraction of the
    /// scalar field, and the retarget controller can only ever scale a target
    /// it is already given. Seeding it from a token amount cannot express a
    /// field-scale value, so the chain would start ~60 orders of magnitude
    /// too hard. Spec: 19.
    pub minimum_difficulty: ModulusShift,
    /// EMA smoothing factor `F` (weight of the prior estimate). Must be below
    /// [`Self::ema_smoothing_precision`]: `P - F` is a divisor in
    /// [`Self::reward_target_floor`].
    pub ema_smoothing_factor: u64,
    /// EMA smoothing precision `P`; the smoothing fraction is `F / P`.
    /// Must be above [`Self::ema_smoothing_factor`]: `P - F` is a divisor in
    /// [`Self::reward_target_floor`].
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
    /// `POW_SHARE`: numerator of the share of each block's collected fees
    /// diverted to the `PoW` reward pool. `0` disables refilling.
    /// Must not exceed [`Self::share_den`].
    pub pow_share: u64,
    /// `SHARE_DEN`: denominator of the share of each block's collected fees
    /// diverted to the `PoW` reward pool.
    pub share_den: NonZeroU64,
    /// Acceptance window, in slots: how far back a claim's anchor block (and
    /// its nullifier) may be from the current block.
    pub slot_window: NonZeroU64,
}

/// Wire representation of [`RewardPoWConfig`], deserialized first so its
/// invariants can be checked before a validated [`RewardPoWConfig`] is built.
#[derive(serde::Deserialize)]
struct RewardPoWConfigFields {
    minimum_difficulty: ModulusShift,
    ema_smoothing_factor: u64,
    ema_smoothing_precision: NonZeroU64,
    target_claims_per_block: u64,
    rate_num: u64,
    rate_den: NonZeroU64,
    target_claim_per_block: NonZeroU64,
    pow_share: u64,
    share_den: NonZeroU64,
    slot_window: NonZeroU64,
}

impl TryFrom<RewardPoWConfigFields> for RewardPoWConfig {
    type Error = RewardPoWConfigError;

    fn try_from(fields: RewardPoWConfigFields) -> Result<Self, Self::Error> {
        let config = Self {
            minimum_difficulty: fields.minimum_difficulty,
            ema_smoothing_factor: fields.ema_smoothing_factor,
            ema_smoothing_precision: fields.ema_smoothing_precision,
            target_claims_per_block: fields.target_claims_per_block,
            rate_num: fields.rate_num,
            rate_den: fields.rate_den,
            target_claim_per_block: fields.target_claim_per_block,
            pow_share: fields.pow_share,
            share_den: fields.share_den,
            slot_window: fields.slot_window,
        };
        config.validate()?;
        Ok(config)
    }
}

/// Invariant violations in a [`RewardPoWConfig`], surfaced at config-load time.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum RewardPoWConfigError {
    #[error("EMA smoothing factor ({factor}) must be below EMA smoothing precision ({precision})")]
    EmaSmoothingFactorNotBelowPrecision { factor: u64, precision: NonZeroU64 },
    #[error(
        "claim rate scale overflows u64: rate_den ({rate_den}) * \
         target_claim_per_block ({target_claim_per_block})"
    )]
    ClaimRateScaleOverflow {
        rate_den: NonZeroU64,
        target_claim_per_block: NonZeroU64,
    },
    #[error("PoW fee share ({pow_share}) must not exceed its denominator ({share_den})")]
    PowShareExceedsDenominator {
        pow_share: u64,
        share_den: NonZeroU64,
    },
    #[error(
        "reward target cap ({minimum_difficulty}) must be above the reward target floor \
         ({floor}), or the target can never move"
    )]
    TargetCapNotAboveFloor {
        minimum_difficulty: ModulusShift,
        floor: NonZeroU64,
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
    /// Returns [`RewardPoWConfigError`] if the EMA smoothing factor is not
    /// below the precision, the fee share exceeds its denominator, the reward
    /// target cap is not above the floor, or the configured part of the
    /// payout-rate denominator overflows `u64`.
    pub fn validate(&self) -> Result<(), RewardPoWConfigError> {
        if self.ema_smoothing_factor >= self.ema_smoothing_precision.get() {
            return Err(RewardPoWConfigError::EmaSmoothingFactorNotBelowPrecision {
                factor: self.ema_smoothing_factor,
                precision: self.ema_smoothing_precision,
            });
        }

        if self.pow_share > self.share_den.get() {
            return Err(RewardPoWConfigError::PowShareExceedsDenominator {
                pow_share: self.pow_share,
                share_den: self.share_den,
            });
        }

        // The retarget clamps between the floor and the cap, the genesis
        // target: at or below the floor, it would always return the cap.
        let floor = self.checked_reward_target_floor()?;
        if PowTarget::from(self.minimum_difficulty) <= PowTarget::from(floor.get()) {
            return Err(RewardPoWConfigError::TargetCapNotAboveFloor {
                minimum_difficulty: self.minimum_difficulty,
                floor,
            });
        }

        self.checked_claim_rate_scale()?;
        Ok(())
    }

    /// `REWARD_TARGET_FLOOR`: the retarget never returns a target below this to
    /// prevent it from falling to a value it can never recover from.
    ///
    /// It is computed from `ceil(F / (P - F))`.
    /// The formula is designed to prevent the floor from being too small.
    /// If it is too small, the target stays the same value.
    ///
    /// With `F = 0` the formula gives 0, which defeats its purpose,
    /// so the function sets a minimum of 1.
    fn checked_reward_target_floor(&self) -> Result<NonZeroU64, RewardPoWConfigError> {
        Ok(self
            .ema_smoothing_factor
            .div_ceil(
                self.ema_smoothing_precision
                    .get()
                    .checked_sub(self.ema_smoothing_factor)
                    .and_then(NonZeroU64::new)
                    .ok_or(RewardPoWConfigError::EmaSmoothingFactorNotBelowPrecision {
                        factor: self.ema_smoothing_factor,
                        precision: self.ema_smoothing_precision,
                    })?
                    .get(),
            )
            .max(1)
            .try_into()
            .expect("floor is at least one"))
    }

    /// `REWARD_TARGET_FLOOR`: the retarget never returns a target below this to
    /// prevent it from falling to a value it can never recover from.
    ///
    /// It is computed from `ceil(F / (P - F))` and at least 1.
    #[must_use]
    pub fn reward_target_floor(&self) -> NonZeroU64 {
        self.checked_reward_target_floor()
            .expect("reward_target_floor must be computed successfully")
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

    /// The share of a block's collected fees diverted to the `PoW` reward
    /// pool: `collected_fees * POW_SHARE / SHARE_DEN`, rounded down.
    ///
    /// Never exceeds `collected_fees`, since `pow_share <= share_den` is
    /// guaranteed by [`Self::validate`].
    #[must_use]
    pub(crate) fn pow_fee_share(&self, collected_fees: Value) -> Value {
        assert!(
            self.pow_share <= self.share_den.get(),
            "PoW share must not exceed its denominator; guaranteed by RewardPoWConfig::validate"
        );

        // Convert u64 values to u128 to avoid overflow. Safe to use
        // `strict_mul`.
        let share = u128::from(collected_fees).strict_mul(u128::from(self.pow_share))
            / NonZeroU128::from(self.share_den);
        Value::try_from(share).expect("share cannot exceed collected_fees")
    }
}

/// The encoding of the fixture below, one field per group.
const REWARD_POW_CONFIG_HEX: &str = "20000000 2100000000000000 2200000000000000 2300000000000000 2400000000000000 2500000000000000 2600000000000000 2700000000000000 2800000000000000 2900000000000000";

codec_fixtures!(
    RewardPoWConfig,
    encode_only,
    RewardPoWConfig {
        minimum_difficulty: ModulusShift::new::<32>(),
        ema_smoothing_factor: 33,
        ema_smoothing_precision: NonZeroU64::new(34).unwrap(),
        target_claims_per_block: 35,
        rate_num: 36,
        rate_den: NonZeroU64::new(37).unwrap(),
        target_claim_per_block: NonZeroU64::new(38).unwrap(),
        pow_share: 39,
        share_den: NonZeroU64::new(40).unwrap(),
        slot_window: NonZeroU64::new(41).unwrap(),
    } => REWARD_POW_CONFIG_HEX
);

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

/// `G_max`: the most execution gas a block may consume. At least 2, so that its
/// half, `G_target`, the usage the base fee steers blocks towards, is never
/// zero.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(try_from = "u64", into = "u64")]
pub struct MaxGasPerBlock(u64);

impl MaxGasPerBlock {
    /// `gas`, if it is at least 2.
    #[must_use]
    pub const fn new(gas: u64) -> Option<Self> {
        if gas >= 2 { Some(Self(gas)) } else { None }
    }

    /// `G_target`: half of the limit.
    #[must_use]
    pub(crate) const fn target(self) -> Gas {
        Gas::new(self.0 / 2)
    }
}

impl TryFrom<u64> for MaxGasPerBlock {
    type Error = String;

    fn try_from(gas: u64) -> Result<Self, Self::Error> {
        Self::new(gas).ok_or_else(|| {
            format!(
                "The max execution gas per block, {gas}, must be at least 2, so that its half, \
                 the target, is not zero."
            )
        })
    }
}

impl From<MaxGasPerBlock> for u64 {
    fn from(max_gas_per_block: MaxGasPerBlock) -> Self {
        max_gas_per_block.0
    }
}

impl From<MaxGasPerBlock> for Gas {
    fn from(max_gas_per_block: MaxGasPerBlock) -> Self {
        Self::new(max_gas_per_block.0)
    }
}

// A block's gas limit: the `u64` it counts.
impl BinaryEncode for MaxGasPerBlock {
    fn encoded_length(&self) -> usize {
        self.0.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.0.encode_into(out);
    }
}

codec_fixtures!(MaxGasPerBlock, encode_only, MaxGasPerBlock::new(43).unwrap() => "2b00000000000000");

/// The execution market of an era: how the execution base fee follows the
/// execution gas blocks consume, per the
/// [execution market](https://lip.logos.co/blockchain/raw/execution-market.html).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionMarketConfig {
    /// `G_max`: the most execution gas a block may consume.
    pub max_gas_per_block: MaxGasPerBlock,
    /// `φ`: the fraction by which the base fee rises in a block whose average
    /// usage is twice the target, and falls in one whose average usage is
    /// zero.
    pub fee_adjustment_rate: ProperFraction,
    /// `q`: the weight of the previous average in the moving average of the
    /// execution gas blocks consume.
    pub ema_smoothing_factor: ProperFraction,
}

/// The storage market of an era: how the price of storage gas follows the
/// storage gas each epoch consumes, per the
/// [storage markets](https://lip.logos.co/blockchain/raw/storage-markets.html).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageMarketConfig {
    /// `α`: the largest fraction by which the price moves in an epoch.
    pub max_adjustment_factor: ProperFraction,
    /// `β`: the weight of the latest epoch in the moving average of the storage
    /// gas epochs consume.
    pub ema_smoothing_factor: ProperFraction,
}

// The execution market's parameters: every field in declaration order. A
// node's era digest commits to them through this encoding.
impl BinaryEncode for ExecutionMarketConfig {
    fn encoded_length(&self) -> usize {
        let Self {
            max_gas_per_block,
            fee_adjustment_rate,
            ema_smoothing_factor,
        } = self;
        max_gas_per_block.encoded_length()
            + fee_adjustment_rate.encoded_length()
            + ema_smoothing_factor.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            max_gas_per_block,
            fee_adjustment_rate,
            ema_smoothing_factor,
        } = self;
        max_gas_per_block.encode_into(out);
        fee_adjustment_rate.encode_into(out);
        ema_smoothing_factor.encode_into(out);
    }
}

/// The encoding of the fixture below, one field per group.
const EXECUTION_MARKET_CONFIG_HEX: &str = "2b00000000000000 2c000000 2d000000 2e000000 2f000000";

codec_fixtures!(
    ExecutionMarketConfig,
    encode_only,
    ExecutionMarketConfig {
        max_gas_per_block: MaxGasPerBlock::new(43).unwrap(),
        fee_adjustment_rate: ProperFraction::new(44, 45.try_into().unwrap()).unwrap(),
        ema_smoothing_factor: ProperFraction::new(46, 47.try_into().unwrap()).unwrap(),
    } => EXECUTION_MARKET_CONFIG_HEX
);

// The storage market's parameters: every field in declaration order. A node's
// era digest commits to them through this encoding.
impl BinaryEncode for StorageMarketConfig {
    fn encoded_length(&self) -> usize {
        let Self {
            max_adjustment_factor,
            ema_smoothing_factor,
        } = self;
        max_adjustment_factor.encoded_length() + ema_smoothing_factor.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            max_adjustment_factor,
            ema_smoothing_factor,
        } = self;
        max_adjustment_factor.encode_into(out);
        ema_smoothing_factor.encode_into(out);
    }
}

/// The encoding of the fixture below, one field per group.
const STORAGE_MARKET_CONFIG_HEX: &str = "30000000 31000000 32000000 33000000";

codec_fixtures!(
    StorageMarketConfig,
    encode_only,
    StorageMarketConfig {
        max_adjustment_factor: ProperFraction::new(48, 49.try_into().unwrap()).unwrap(),
        ema_smoothing_factor: ProperFraction::new(50, 51.try_into().unwrap()).unwrap(),
    } => STORAGE_MARKET_CONFIG_HEX
);

/// How each block is rewarded, per the
/// [block rewards](https://lip.logos.co/blockchain/raw/block-rewards.html).
///
/// A block's reward mixes an emission from the reserve with the fee the block
/// pooled: the further the stake falls short of its target, and the more fees
/// the window pooled, the more of it is emission. The reward is split between
/// the Blend nodes and the block's leader.
///
/// Its values keep every step of a reward within a `u128`, whatever the fees.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(try_from = "BlockRewardsFields", into = "BlockRewardsFields")]
pub struct BlockRewardsConfig {
    /// `STAKE_TARGET`: the total stake the emission steers towards.
    stake_target: Value,
    /// `A_SCALE`: the shortfall from the stake target at which a block's
    /// emission is in full, and the scale of the emission's weight in a
    /// reward.
    emission_factor_scale: NonZeroU64,
    /// `FEE_AVG_NUMERATOR`: how much each unit of the fees pooled over the
    /// window adds to the emission's weight.
    fee_average_numerator: u64,
    /// `INFLATION_NUMERATOR / INFLATION_DENOMINATOR`, `I_max · S_cap · Δt / f`:
    /// a block's emission in full.
    max_emission_per_block: NonNegativeRatio,
    /// The Blend nodes' share of a reward, the leader taking the rest with the
    /// tips.
    blend_share: ProperFraction,
}

/// The values of a [`BlockRewardsConfig`], as a deployment states them.
#[derive(serde::Serialize, serde::Deserialize)]
struct BlockRewardsFields {
    stake_target: Value,
    emission_factor_scale: NonZeroU64,
    fee_average_numerator: u64,
    max_emission_per_block: NonNegativeRatio,
    blend_share: ProperFraction,
}

impl BlockRewardsConfig {
    /// The block rewards of these values, unless a reward could overflow a
    /// `u128` at some fee: the emission's denominator, the scale and the
    /// share's denominator must multiply to at most `u128::MAX / Value::MAX`.
    #[must_use]
    pub const fn new(
        stake_target: Value,
        emission_factor_scale: NonZeroU64,
        fee_average_numerator: u64,
        max_emission_per_block: NonNegativeRatio,
        blend_share: ProperFraction,
    ) -> Option<Self> {
        // A reward's numerator reaches the emission's denominator times the
        // scale times a block's fee, at most a `Value`, and the split then
        // multiplies it by less than the share's denominator.
        let bound = match (max_emission_per_block.denominator.get() as u128)
            .checked_mul(emission_factor_scale.get() as u128)
        {
            Some(bound) => bound.checked_mul(blend_share.get().denominator.get() as u128),
            None => None,
        };
        match bound {
            Some(bound) if bound <= u128::MAX / Value::MAX as u128 => Some(Self {
                stake_target,
                emission_factor_scale,
                fee_average_numerator,
                max_emission_per_block,
                blend_share,
            }),
            _ => None,
        }
    }

    /// A block's rewards, to the Blend nodes then to its leader, before its
    /// tips: `pooled_fees` sums the fees pooled over the window, `block_fee`,
    /// the block's own, included, and `total_stake` is the inferred total
    /// stake.
    pub(crate) fn rewards(
        &self,
        pooled_fees: u128,
        block_fee: Value,
        total_stake: Value,
    ) -> (Value, Value) {
        let Self {
            stake_target,
            emission_factor_scale,
            fee_average_numerator,
            max_emission_per_block,
            blend_share,
        } = *self;
        let scale = NonZeroU128::from(emission_factor_scale);
        // `A_t'`: the emission's weight in the reward, out of `scale`.
        let emission_weight = u128::from(stake_target)
            .saturating_add(u128::from(fee_average_numerator).saturating_mul(pooled_fees))
            .saturating_sub(u128::from(total_stake))
            .min(scale.get());
        let emission_denominator = NonZeroU128::from(max_emission_per_block.denominator);
        let reward_numerator = u128::from(max_emission_per_block.numerator) * emission_weight
            + emission_denominator.get() * (scale.get() - emission_weight) * u128::from(block_fee);
        let NonNegativeRatio {
            numerator: blend_numerator,
            denominator: share_denominator,
        } = blend_share.get();
        let share_denominator = NonZeroU128::from(share_denominator);
        // The reward's denominator, split: within a `u128`, as `Self::new`
        // checks, so the product never saturates.
        let denominator = emission_denominator
            .saturating_mul(scale)
            .saturating_mul(share_denominator);
        let share = |numerator: u128| {
            Value::try_from(reward_numerator * numerator / denominator).expect(
                "A reward is at most the larger of a block's emission in full and its fee, both \
                 within a Value.",
            )
        };
        let blend_numerator = u128::from(blend_numerator);
        (
            share(blend_numerator),
            share(share_denominator.get() - blend_numerator),
        )
    }
}

impl TryFrom<BlockRewardsFields> for BlockRewardsConfig {
    type Error = String;

    fn try_from(
        BlockRewardsFields {
            stake_target,
            emission_factor_scale,
            fee_average_numerator,
            max_emission_per_block,
            blend_share,
        }: BlockRewardsFields,
    ) -> Result<Self, Self::Error> {
        Self::new(
            stake_target,
            emission_factor_scale,
            fee_average_numerator,
            max_emission_per_block,
            blend_share,
        )
        .ok_or_else(|| {
            format!(
                "The block rewards' emission denominator, {}, scale, {emission_factor_scale}, and \
                 Blend share denominator, {}, must multiply to at most {}, or a reward could \
                 overflow.",
                max_emission_per_block.denominator,
                blend_share.get().denominator,
                u128::MAX / u128::from(Value::MAX)
            )
        })
    }
}

impl From<BlockRewardsConfig> for BlockRewardsFields {
    fn from(
        BlockRewardsConfig {
            stake_target,
            emission_factor_scale,
            fee_average_numerator,
            max_emission_per_block,
            blend_share,
        }: BlockRewardsConfig,
    ) -> Self {
        Self {
            stake_target,
            emission_factor_scale,
            fee_average_numerator,
            max_emission_per_block,
            blend_share,
        }
    }
}

// The block rewards' parameters: every field in declaration order. A node's era
// digest commits to them through this encoding.
impl BinaryEncode for BlockRewardsConfig {
    fn encoded_length(&self) -> usize {
        let Self {
            stake_target,
            emission_factor_scale,
            fee_average_numerator,
            max_emission_per_block,
            blend_share,
        } = self;
        stake_target.encoded_length()
            + emission_factor_scale.encoded_length()
            + fee_average_numerator.encoded_length()
            + max_emission_per_block.encoded_length()
            + blend_share.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            stake_target,
            emission_factor_scale,
            fee_average_numerator,
            max_emission_per_block,
            blend_share,
        } = self;
        stake_target.encode_into(out);
        emission_factor_scale.encode_into(out);
        fee_average_numerator.encode_into(out);
        max_emission_per_block.encode_into(out);
        blend_share.encode_into(out);
    }
}

/// The encoding of the fixture below, one field per group.
const BLOCK_REWARDS_CONFIG_HEX: &str =
    "3500000000000000 3600000000000000 3700000000000000 38000000 39000000 3a000000 3b000000";

codec_fixtures!(
    BlockRewardsConfig,
    encode_only,
    BlockRewardsConfig::new(
        53,
        NonZeroU64::new(54).unwrap(),
        55,
        NonNegativeRatio::new(56, NonZeroU32::new(57).unwrap()),
        ProperFraction::new(58, NonZeroU32::new(59).unwrap()).unwrap(),
    )
    .unwrap() => BLOCK_REWARDS_CONFIG_HEX
);

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZero, NonZeroU32, NonZeroU64, NonZeroU128},
        sync::Arc,
    };

    use lb_core::{
        era::AcceptedForkDigests,
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::EpochConfig;
    pub use lb_groth16::ModulusShift;
    use lb_utils::math::{NonNegativeRatio, PositiveF64};

    use crate::{
        config::{
            BlendPoWConfig, PoWConfig, RewardPoWConfig, RewardPoWConfigError, nonce_snapshot,
            stake_distribution_snapshot,
        },
        mantle::sdp::{ServiceRewardsParameters, rewards::blend::RewardsParameters},
        test_utils::{FORK_DIGEST, single_era},
    };

    /// A reward config with claiming disabled, standing in for a real
    /// deployment config in tests that don't exercise the reward parameters.
    fn disabled_reward_config() -> RewardPoWConfig {
        RewardPoWConfig {
            minimum_difficulty: ModulusShift::new::<26>(),
            ema_smoothing_factor: 9,
            ema_smoothing_precision: NonZeroU64::new(10).unwrap(),
            target_claims_per_block: 100,
            rate_num: 0,
            rate_den: NonZeroU64::MIN,
            target_claim_per_block: NonZeroU64::MIN,
            pow_share: 0,
            share_den: NonZeroU64::MIN,
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
            Err(RewardPoWConfigError::EmaSmoothingFactorNotBelowPrecision {
                factor: config.ema_smoothing_factor,
                precision: config.ema_smoothing_precision,
            })
        );
    }

    #[test]
    fn reward_config_rejects_ema_factor_equal_to_precision() {
        // F == P leaves P - F at zero, so the reward target floor is undefined.
        let mut config = disabled_reward_config();
        config.ema_smoothing_factor = config.ema_smoothing_precision.get();
        assert_eq!(
            config.validate(),
            Err(RewardPoWConfigError::EmaSmoothingFactorNotBelowPrecision {
                factor: config.ema_smoothing_factor,
                precision: config.ema_smoothing_precision,
            })
        );
    }

    #[test]
    fn reward_target_floor() {
        let reward_target_floor = |factor: u64, precision: u64| {
            let mut config = disabled_reward_config();
            config.ema_smoothing_factor = factor;
            config.ema_smoothing_precision = NonZeroU64::new(precision).unwrap();
            config.reward_target_floor().get()
        };
        // As per spec: ceil(9 / (10-9)).
        assert_eq!(reward_target_floor(9, 10), 9);
        // ceil(7 / (10-7)).
        assert_eq!(reward_target_floor(7, 10), 3);
        // ceil(0 / (10-0)) is 0, so the floor is raised to 1.
        assert_eq!(reward_target_floor(0, 10), 1);
    }

    #[test]
    fn reward_config_rejects_pow_share_above_denominator() {
        let mut config = disabled_reward_config();
        config.pow_share = 11;
        config.share_den = NonZeroU64::new(10).unwrap();
        assert_eq!(
            config.validate(),
            Err(RewardPoWConfigError::PowShareExceedsDenominator {
                pow_share: config.pow_share,
                share_den: config.share_den,
            })
        );
    }

    #[test]
    fn reward_config_accepts_pow_share_equal_to_denominator() {
        let mut config = disabled_reward_config();
        config.pow_share = 10;
        config.share_den = NonZeroU64::new(10).unwrap();
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn reward_config_rejects_target_cap_not_above_floor() {
        // p / 2^253 is 1, below the floor of 9 that F/P = 9/10 sets.
        let mut config = disabled_reward_config();
        config.minimum_difficulty = ModulusShift::new::<253>();
        assert_eq!(
            config.validate(),
            Err(RewardPoWConfigError::TargetCapNotAboveFloor {
                minimum_difficulty: config.minimum_difficulty,
                floor: NonZeroU64::new(9).unwrap(),
            })
        );
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
            12,
            4,
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
                        cover_replication_factor: 0,
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
            max_transactions: 1024,
            max_transactions_size: 2 * 1024 * 1024,
            max_op_data_size: 2 * 1024 * 1024 * 7 / 8,
            execution_market: crate::config::ExecutionMarketConfig {
                max_gas_per_block: crate::config::MaxGasPerBlock::new(3_193_460).unwrap(),
                fee_adjustment_rate: lb_utils::math::ProperFraction::new(1, 8.try_into().unwrap())
                    .unwrap(),
                ema_smoothing_factor: lb_utils::math::ProperFraction::new(
                    9,
                    10.try_into().unwrap(),
                )
                .unwrap(),
            },
            storage_market: crate::config::StorageMarketConfig {
                max_adjustment_factor: lb_utils::math::ProperFraction::new(
                    1,
                    8.try_into().unwrap(),
                )
                .unwrap(),
                ema_smoothing_factor: lb_utils::math::ProperFraction::new(1, 2.try_into().unwrap())
                    .unwrap(),
            },
            block_rewards: crate::config::BlockRewardsConfig::new(
                3_000_000_000,
                120_000_000.try_into().unwrap(),
                10_512,
                NonNegativeRatio::new(62_500, 657.try_into().unwrap()),
                lb_utils::math::ProperFraction::new(6, 10.try_into().unwrap()).unwrap(),
            )
            .unwrap(),
            gas_profile: crate::cryptarchia::tests::GAS_PROFILE,
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
            accepted_fork_digests: AcceptedForkDigests::new(vec![FORK_DIGEST]),
        };
        assert_eq!(config.epoch_length(), 100);
        let eras = single_era(config);
        let epoch_1 = NonZeroU32::new(1).unwrap();
        let epoch_2 = NonZeroU32::new(2).unwrap();
        assert_eq!(nonce_snapshot(&eras, epoch_1), 60.into());
        assert_eq!(nonce_snapshot(&eras, epoch_2), 160.into());
        assert_eq!(
            stake_distribution_snapshot(&eras, 1.try_into().unwrap()),
            0.into()
        );
        assert_eq!(
            stake_distribution_snapshot(&eras, 2.try_into().unwrap()),
            100.into()
        );
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
            12,
            4,
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
                        cover_replication_factor: 0,
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
            max_transactions: 1024,
            max_transactions_size: 2 * 1024 * 1024,
            max_op_data_size: 2 * 1024 * 1024 * 7 / 8,
            execution_market: crate::config::ExecutionMarketConfig {
                max_gas_per_block: crate::config::MaxGasPerBlock::new(3_193_460).unwrap(),
                fee_adjustment_rate: lb_utils::math::ProperFraction::new(1, 8.try_into().unwrap())
                    .unwrap(),
                ema_smoothing_factor: lb_utils::math::ProperFraction::new(
                    9,
                    10.try_into().unwrap(),
                )
                .unwrap(),
            },
            storage_market: crate::config::StorageMarketConfig {
                max_adjustment_factor: lb_utils::math::ProperFraction::new(
                    1,
                    8.try_into().unwrap(),
                )
                .unwrap(),
                ema_smoothing_factor: lb_utils::math::ProperFraction::new(1, 2.try_into().unwrap())
                    .unwrap(),
            },
            block_rewards: crate::config::BlockRewardsConfig::new(
                3_000_000_000,
                120_000_000.try_into().unwrap(),
                10_512,
                NonNegativeRatio::new(62_500, 657.try_into().unwrap()),
                lb_utils::math::ProperFraction::new(6, 10.try_into().unwrap()).unwrap(),
            )
            .unwrap(),
            gas_profile: crate::cryptarchia::tests::GAS_PROFILE,
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
            accepted_fork_digests: AcceptedForkDigests::new(vec![FORK_DIGEST]),
        }
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
            12,
            4,
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
                        cover_replication_factor: 0,
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
            max_transactions: 1024,
            max_transactions_size: 2 * 1024 * 1024,
            max_op_data_size: 2 * 1024 * 1024 * 7 / 8,
            execution_market: crate::config::ExecutionMarketConfig {
                max_gas_per_block: crate::config::MaxGasPerBlock::new(3_193_460).unwrap(),
                fee_adjustment_rate: lb_utils::math::ProperFraction::new(1, 8.try_into().unwrap())
                    .unwrap(),
                ema_smoothing_factor: lb_utils::math::ProperFraction::new(
                    9,
                    10.try_into().unwrap(),
                )
                .unwrap(),
            },
            storage_market: crate::config::StorageMarketConfig {
                max_adjustment_factor: lb_utils::math::ProperFraction::new(
                    1,
                    8.try_into().unwrap(),
                )
                .unwrap(),
                ema_smoothing_factor: lb_utils::math::ProperFraction::new(1, 2.try_into().unwrap())
                    .unwrap(),
            },
            block_rewards: crate::config::BlockRewardsConfig::new(
                3_000_000_000,
                120_000_000.try_into().unwrap(),
                10_512,
                NonNegativeRatio::new(62_500, 657.try_into().unwrap()),
                lb_utils::math::ProperFraction::new(6, 10.try_into().unwrap()).unwrap(),
            )
            .unwrap(),
            gas_profile: crate::cryptarchia::tests::GAS_PROFILE,
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
            accepted_fork_digests: AcceptedForkDigests::new(vec![FORK_DIGEST]),
        };
        let eras = single_era(config);
        assert_eq!(eras.epoch_for_slot(1.into()), 0);
        assert_eq!(eras.epoch_for_slot(100.into()), 1);
        assert_eq!(eras.epoch_for_slot(101.into()), 1);
        assert_eq!(eras.epoch_for_slot(200.into()), 2);
    }
}
