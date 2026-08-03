use lb_core::mantle::{
    Value,
    ops::pow::{ClaimPoWRewardExecutionContext, PowNullifier, PowReward, PowTarget},
};
use lb_groth16::serde::serde_fr;
use rpds::HashTrieSetSync;

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
    refill_rewards: PowReward,
    /// Spent `PoW` solutions, retained only for the acceptance
    nullifiers: HashTrieSetSync<PowNullifier>,
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum Error {}

impl PowState {
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

    pub const fn reward_pool(&self) -> Value {
        self.reward_pool
    }

    pub const fn epoch_reward(&self) -> Value {
        self.epoch_reward
    }

    pub const fn nullifiers(&self) -> &HashTrieSetSync<PowNullifier> {
        &self.nullifiers
    }

    pub fn update_from_claim_execution_result(&mut self, context: &ClaimPoWRewardExecutionContext) {
        self.nullifiers = context.nullifiers.clone();
        self.reward_pool = context.reward_pool;
    }

    pub(crate) fn add_rewards_to_pool<Constants: ClaimPoWConstants>(&mut self) {
        self.reward_pool = self.reward_pool.saturating_add(self.refill_rewards);
        self.refill_rewards = 0;
        self.epoch_reward = compute_epoch_pow_reward::<Constants>(self.reward_pool);
    }

    pub(crate) const fn add_reward_refill_rewards(&mut self, reward: PowReward) {
        self.refill_rewards = self.refill_rewards.saturating_add(reward);
    }
}

pub trait ClaimPoWConstants {
    const RATE_NUM: u64 = 1;
    const RATE_DEN: u64 = 100;
    const TARGET_CLAIM_PER_BLOCK: u64;
    const EXPECTED_BLOCKS_PER_EPOCH: u64;

    fn denominator() -> u64 {
        Self::RATE_DEN * Self::TARGET_CLAIM_PER_BLOCK * Self::EXPECTED_BLOCKS_PER_EPOCH
    }
}

pub fn compute_epoch_pow_reward<Constants: ClaimPoWConstants>(
    pow_reward_pool: PowReward,
) -> PowReward {
    pow_reward_pool * Constants::RATE_NUM / Constants::denominator()
}
