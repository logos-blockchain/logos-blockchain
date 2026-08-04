use lb_core::mantle::{
    Value,
    ops::pow::{ClaimPoWRewardExecutionContext, PowNullifier, PowReward, PowTarget},
};
use lb_groth16::serde::serde_fr;
use rpds::HashTrieSetSync;

use crate::EpochState;

/// `PoW` reward-claiming state of the mantle ledger.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PowState {
    /// `R_PoW`: reserve funding `PoW` rewards. Credited at each epoch boundary
    /// with the `PoW` share (`beta_PoW`) of every block's reward, summed
    /// over the epoch's blocks. Drained by `sigma_e` as rewards are
    /// claimed.
    reward_pool: PowReward,
    /// `sigma_e`: reward per claim, fixed for the epoch.
    epoch_reward: PowReward,
    /// `d_reward`: the REWARD threshold, retargeted every block
    #[serde(with = "serde_fr")]
    reward_difficulty: PowTarget,
    /// Rewards collected during the current epoch, added to the
    /// `reward_pool` at the next epoch boundary.
    refill_rewards: PowReward,
    /// Spent `PoW` solutions, retained only for the acceptance
    nullifiers: HashTrieSetSync<PowNullifier>,
}

/// Errors that can occur while applying `PoW` state transitions.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum Error {}

impl PowState {
    /// Create an empty `PoW` state with no rewards, no claims and default
    /// difficulty.
    pub fn new() -> Self {
        Self {
            // TODO: Use genesis value instead
            reward_pool: 0,
            epoch_reward: 0,
            // TODO: Set initial difficulty
            reward_difficulty: PowTarget::default(),
            refill_rewards: 0,
            nullifiers: HashTrieSetSync::new_sync(),
        }
    }

    /// `R_PoW`: current balance of the `PoW` reward pool.
    pub const fn reward_pool(&self) -> Value {
        self.reward_pool
    }

    /// `sigma_e`: reward per claim for the current epoch.
    pub const fn epoch_reward(&self) -> Value {
        self.epoch_reward
    }

    /// Nullifiers of already-claimed `PoW` solutions.
    pub const fn nullifiers(&self) -> &HashTrieSetSync<PowNullifier> {
        &self.nullifiers
    }

    /// Apply the outcome of a [`ClaimPowRewardOp`] execution to this state.
    ///
    /// [`ClaimPowRewardOp`]: lb_core::mantle::ops::pow::ClaimPowRewardOp
    pub fn update_from_claim_execution_result(&mut self, context: &ClaimPoWRewardExecutionContext) {
        self.nullifiers = context.nullifiers.clone();
        self.reward_pool = context.reward_pool;
    }

    /// Move the epoch's collected `refill_rewards` into the `reward_pool`
    /// and recompute the per-claim `epoch_reward` from it.
    pub(crate) fn add_rewards_to_pool<Constants: ClaimPoWConstants>(&mut self) {
        self.reward_pool = self.reward_pool.saturating_add(self.refill_rewards);
        self.refill_rewards = 0;
        self.epoch_reward = compute_epoch_pow_reward::<Constants>(self.reward_pool);
    }

    /// Add `reward` to the current epoch's pending `refill_rewards`.
    pub(crate) const fn add_reward_refill_rewards(&mut self, reward: PowReward) {
        self.refill_rewards = self.refill_rewards.saturating_add(reward);
    }

    /// Apply an epoch transition: on epoch change, refill the reward pool
    /// from the rewards collected during `previous_epoch`.
    pub(crate) fn try_apply_header(
        &self,
        _config: (),
        previous_epoch: &EpochState,
        next_epoch: &EpochState,
    ) -> Self {
        if previous_epoch.epoch >= next_epoch.epoch {
            return self.clone();
        }
        let mut new_self = self.clone();
        new_self.add_rewards_to_pool::<ClaimPoWDisabledConstants>();
        new_self
    }
}

/// Network parameters controlling how much of the `PoW` reward pool is paid
/// out per epoch, expressed as the rate `RATE_NUM / denominator()`.
pub trait ClaimPoWConstants {
    /// Numerator of the per-epoch payout rate.
    const RATE_NUM: u64 = 0;
    /// Denominator scale of the per-epoch payout rate.
    const RATE_DEN: u64 = 100;
    /// Expected number of reward claims per block.
    const TARGET_CLAIM_PER_BLOCK: u64 = 0;
    /// Expected number of blocks per epoch.
    const EXPECTED_BLOCKS_PER_EPOCH: u64 = 0;

    /// Full denominator of the per-epoch payout rate.
    fn denominator() -> u64 {
        Self::RATE_DEN * Self::TARGET_CLAIM_PER_BLOCK * Self::EXPECTED_BLOCKS_PER_EPOCH
    }
}

/// [`ClaimPoWConstants`] with `PoW` claiming disabled: all rates are zero, so
/// no reward is ever paid out.
struct ClaimPoWDisabledConstants;

impl ClaimPoWConstants for ClaimPoWDisabledConstants {
    const RATE_NUM: u64 = 0;
    const RATE_DEN: u64 = 0;
    const TARGET_CLAIM_PER_BLOCK: u64 = 0;
    const EXPECTED_BLOCKS_PER_EPOCH: u64 = 0;
}

/// Compute the per-claim `sigma_e` reward for the epoch from the current
/// `PoW` reward pool balance, per `Constants`' payout rate.
pub fn compute_epoch_pow_reward<Constants: ClaimPoWConstants>(
    pow_reward_pool: PowReward,
) -> PowReward {
    pow_reward_pool * Constants::RATE_NUM / Constants::denominator()
}
