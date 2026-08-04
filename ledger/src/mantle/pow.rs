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
    const RATE_DEN: u64 = 1;
    const TARGET_CLAIM_PER_BLOCK: u64 = 1;
    const EXPECTED_BLOCKS_PER_EPOCH: u64 = 1;
}

/// Compute the per-claim `sigma_e` reward for the epoch from the current
/// `PoW` reward pool balance, per `Constants`' payout rate.
///
/// The intermediate product is widened to `u128` so a full pool
/// (`u64::MAX`, reachable through saturation) cannot overflow with a
/// `RATE_NUM` greater than one; a result beyond `u64` saturates.
pub fn compute_epoch_pow_reward<Constants: ClaimPoWConstants>(
    pow_reward_pool: PowReward,
) -> PowReward {
    let reward = u128::from(pow_reward_pool) * u128::from(Constants::RATE_NUM)
        / u128::from(Constants::denominator());
    PowReward::try_from(reward).unwrap_or(PowReward::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lb_core::{
        mantle::{ledger::Utxos, transactions::hash::TxHash},
        sdp::Declarations,
    };
    use lb_groth16::{AdditiveGroup as _, Field as _, Fr};

    use super::*;
    use crate::UtxoTree;

    fn epoch_state(epoch: u32) -> EpochState {
        EpochState {
            epoch: epoch.into(),
            nonce: Fr::ZERO,
            utxos: UtxoTree::default(),
            total_stake: 0,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
            active_declarations: Arc::new(Declarations::default()),
        }
    }

    /// A payout rate of `1/100`: `sigma_e = pool / 100`.
    struct TestConstants;
    impl ClaimPoWConstants for TestConstants {
        const RATE_NUM: u64 = 1;
        const RATE_DEN: u64 = 1;
        const TARGET_CLAIM_PER_BLOCK: u64 = 10;
        const EXPECTED_BLOCKS_PER_EPOCH: u64 = 10;
    }

    /// The trait's own defaults, with nothing overridden.
    struct DefaultTraitConstants;
    impl ClaimPoWConstants for DefaultTraitConstants {}

    #[test]
    fn new_state_starts_empty() {
        let state = PowState::new();
        assert_eq!(state.reward_pool(), 0);
        assert_eq!(state.epoch_reward(), 0);
        assert!(state.nullifiers().is_empty());
    }

    #[test]
    fn claim_pow_disabled_constants_denominator_is_never_zero() {
        // `try_apply_header` always refills through `ClaimPoWDisabledConstants`.
        // If its denominator were ever zero, every epoch transition would
        // panic inside `compute_epoch_pow_reward` (division by zero) - as it
        // used to before RATE_DEN/TARGET_CLAIM_PER_BLOCK/
        // EXPECTED_BLOCKS_PER_EPOCH were fixed to `1`.
        assert_ne!(ClaimPoWDisabledConstants::denominator(), 0);
    }

    #[test]
    #[should_panic(expected = "attempt to divide by zero")]
    fn trait_default_constants_denominator_is_unsafe() {
        // The trait's bare defaults (RATE_DEN=100, TARGET_CLAIM_PER_BLOCK=0,
        // EXPECTED_BLOCKS_PER_EPOCH=0) yield a zero denominator. Any new
        // `ClaimPoWConstants` impl MUST override TARGET_CLAIM_PER_BLOCK and
        // EXPECTED_BLOCKS_PER_EPOCH to a nonzero value, or claiming panics.
        // Pinned here so the footgun is caught by a test, not a crash.
        let _ = compute_epoch_pow_reward::<DefaultTraitConstants>(1_000);
    }

    #[test]
    fn compute_epoch_pow_reward_applies_rate() {
        assert_eq!(compute_epoch_pow_reward::<TestConstants>(1_000), 10);
        assert_eq!(compute_epoch_pow_reward::<TestConstants>(0), 0);
        // Rounds down when the pool doesn't divide the rate evenly.
        assert_eq!(compute_epoch_pow_reward::<TestConstants>(150), 1);
        assert_eq!(compute_epoch_pow_reward::<TestConstants>(99), 0);
    }

    #[test]
    fn compute_epoch_pow_reward_disabled_is_always_zero() {
        assert_eq!(
            compute_epoch_pow_reward::<ClaimPoWDisabledConstants>(u64::MAX),
            0
        );
    }

    #[test]
    fn compute_epoch_pow_reward_does_not_overflow_on_full_pool() {
        // A rate with RATE_NUM > 1: `sigma_e = pool * 2 / 4`.
        struct HighRateConstants;
        impl ClaimPoWConstants for HighRateConstants {
            const RATE_NUM: u64 = 2;
            const RATE_DEN: u64 = 1;
            const TARGET_CLAIM_PER_BLOCK: u64 = 1;
            const EXPECTED_BLOCKS_PER_EPOCH: u64 = 4;
        }

        // The pool can legitimately reach u64::MAX (it saturates there), so
        // `pool * RATE_NUM` must be widened past u64 or it overflows for any
        // RATE_NUM > 1.
        assert_eq!(
            compute_epoch_pow_reward::<HighRateConstants>(u64::MAX),
            u64::MAX / 2
        );
    }

    #[test]
    fn add_rewards_to_pool_moves_refill_and_computes_reward() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool::<TestConstants>();

        assert_eq!(state.reward_pool(), 1_000);
        assert_eq!(state.epoch_reward(), 10);
    }

    #[test]
    fn add_rewards_to_pool_accumulates_across_multiple_refills() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(400);
        state.add_reward_refill_rewards(600);
        state.add_rewards_to_pool::<TestConstants>();

        assert_eq!(state.reward_pool(), 1_000);
    }

    #[test]
    fn add_rewards_to_pool_is_noop_on_pool_when_no_refill_is_pending() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool::<TestConstants>();
        // Refill was reset by the call above: applying again must not add
        // anything further to the pool.
        state.add_rewards_to_pool::<TestConstants>();

        assert_eq!(state.reward_pool(), 1_000);
        assert_eq!(state.epoch_reward(), 10);
    }

    #[test]
    fn add_rewards_to_pool_recomputes_reward_from_new_pool_each_time() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool::<TestConstants>();
        assert_eq!(state.epoch_reward(), 10);

        state.add_reward_refill_rewards(9_000);
        state.add_rewards_to_pool::<TestConstants>();

        assert_eq!(state.reward_pool(), 10_000);
        assert_eq!(state.epoch_reward(), 100);
    }

    #[test]
    fn refill_rewards_saturate_instead_of_overflowing() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(u64::MAX);
        state.add_reward_refill_rewards(1);
        state.add_rewards_to_pool::<TestConstants>();

        assert_eq!(state.reward_pool(), u64::MAX);
    }

    #[test]
    fn reward_pool_saturates_instead_of_overflowing() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(u64::MAX);
        state.add_rewards_to_pool::<TestConstants>();
        state.add_reward_refill_rewards(u64::MAX);
        state.add_rewards_to_pool::<TestConstants>();

        assert_eq!(state.reward_pool(), u64::MAX);
    }

    #[test]
    fn try_apply_header_is_noop_when_epoch_does_not_advance() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(500);
        let same_epoch = epoch_state(3);

        let unchanged = state.try_apply_header((), &same_epoch, &same_epoch);

        assert_eq!(unchanged, state);
        assert_eq!(unchanged.reward_pool(), 0);
    }

    #[test]
    fn try_apply_header_is_noop_when_epoch_goes_backwards() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(500);
        let earlier = epoch_state(1);
        let later = epoch_state(5);

        // `next_epoch` behind `previous_epoch`, e.g. a stale/reorged branch.
        let unchanged = state.try_apply_header((), &later, &earlier);

        assert_eq!(unchanged, state);
    }

    #[test]
    fn try_apply_header_does_not_mutate_the_receiver() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(500);
        let original = state.clone();
        let previous = epoch_state(0);
        let next = epoch_state(1);

        drop(state.try_apply_header((), &previous, &next));

        assert_eq!(state, original);
    }

    #[test]
    fn try_apply_header_moves_pending_refill_into_pool_on_advance() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(500);
        let previous = epoch_state(0);
        let next = epoch_state(1);

        let new_state = state.try_apply_header((), &previous, &next);

        assert_eq!(new_state.reward_pool(), 500);
    }

    #[test]
    fn try_apply_header_leaves_reward_claiming_disabled() {
        // `try_apply_header` currently always refills through
        // `ClaimPoWDisabledConstants` (claiming isn't activated yet), so
        // `epoch_reward` stays zero even though the pool is well funded.
        let mut state = PowState::new();
        state.add_reward_refill_rewards(1_000_000);
        let previous = epoch_state(0);
        let next = epoch_state(1);

        let new_state = state.try_apply_header((), &previous, &next);

        assert_eq!(new_state.reward_pool(), 1_000_000);
        assert_eq!(new_state.epoch_reward(), 0);
    }

    #[test]
    fn try_apply_header_across_multiple_epoch_jump_applies_once() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(500);
        let previous = epoch_state(0);
        let next = epoch_state(5);

        let new_state = state.try_apply_header((), &previous, &next);

        assert_eq!(new_state.reward_pool(), 500);
    }

    #[test]
    fn try_apply_header_preserves_pending_refill_across_noop_transitions() {
        // A no-op transition (same epoch) must not drop a refill that
        // hasn't been credited to the pool yet.
        let mut state = PowState::new();
        state.add_reward_refill_rewards(200);
        let same = epoch_state(2);
        let mut state = state.try_apply_header((), &same, &same);

        state.add_reward_refill_rewards(300);
        let previous = epoch_state(2);
        let next = epoch_state(3);
        let new_state = state.try_apply_header((), &previous, &next);

        assert_eq!(new_state.reward_pool(), 500);
    }

    #[test]
    fn update_from_claim_execution_result_replaces_pool_and_nullifiers() {
        let mut state = PowState::new();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool::<TestConstants>();
        assert_eq!(state.reward_pool(), 1_000);
        assert_eq!(state.epoch_reward(), 10);

        let nullifier = PowNullifier::from(Fr::ONE);
        let nullifiers = HashTrieSetSync::new_sync().insert(nullifier);
        let context = ClaimPoWRewardExecutionContext {
            reward_pool: 990,
            epoch_reward: 10,
            nullifiers: nullifiers.clone(),
            tx_hash: TxHash::from([7u8; 32]),
            utxos: Utxos::new(),
        };

        state.update_from_claim_execution_result(&context);

        assert_eq!(state.reward_pool(), 990);
        assert_eq!(state.nullifiers(), &nullifiers);
        assert!(state.nullifiers().contains(&nullifier));
        // Unrelated fields are left untouched by this update.
        assert_eq!(state.epoch_reward(), 10);
    }

    /// Build a claim execution result that drains the pool to `reward_pool`,
    /// recording `nullifier` as spent.
    fn claim_result(reward_pool: PowReward, nullifier: PowNullifier) -> ClaimPoWRewardExecutionContext {
        ClaimPoWRewardExecutionContext {
            reward_pool,
            epoch_reward: 0,
            nullifiers: HashTrieSetSync::new_sync().insert(nullifier),
            tx_hash: TxHash::from([7u8; 32]),
            utxos: Utxos::new(),
        }
    }

    #[test]
    fn epoch_reward_tapers_as_claims_drain_the_pool() {
        // Spec §5.6: sigma_e is recomputed at each boundary from the pool as
        // claims left it, so a drained pool pays a smaller per-claim reward
        // in the next epoch, tapering to zero (the safety cutoff's input).
        let mut state = PowState::new();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool::<TestConstants>();
        assert_eq!(state.epoch_reward(), 10);

        // One claim of sigma_e=10 drains the pool to 990.
        state.update_from_claim_execution_result(&claim_result(990, PowNullifier::from(Fr::ONE)));
        state.add_rewards_to_pool::<TestConstants>();
        assert_eq!(state.epoch_reward(), 9);

        // Drained below the payout rate, sigma_e floors to zero and the
        // safety cutoff (§5.6 `pow_reward_enabled`) would disable claiming.
        state.update_from_claim_execution_result(&claim_result(99, PowNullifier::from(Fr::ONE)));
        state.add_rewards_to_pool::<TestConstants>();
        assert_eq!(state.epoch_reward(), 0);
    }

    #[test]
    fn claim_execution_result_does_not_clobber_pending_refill() {
        // Spec §5.8: within an epoch the spendable pool is touched only by
        // claim draws; the refill accrues on the side and lands whole at the
        // boundary. A claim applied after refills have accrued must not
        // discard them.
        let mut state = PowState::new();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool::<TestConstants>();

        // Mid-epoch: block rewards accrue, then a claim drains the pool.
        state.add_reward_refill_rewards(500);
        state.update_from_claim_execution_result(&claim_result(990, PowNullifier::from(Fr::ONE)));

        // Boundary: the refill is credited on top of the post-claim pool,
        // and sigma_e is snapshotted from the refilled pool (§5.6 ordering).
        state.add_rewards_to_pool::<TestConstants>();
        assert_eq!(state.reward_pool(), 1_490);
        assert_eq!(state.epoch_reward(), 14);
    }

    #[test]
    fn try_apply_header_carries_nullifiers_forward() {
        // Spec §5.5/§5.1.1: spent solutions must stay rejected while their
        // block_hash is inside the acceptance window, which spans epoch
        // boundaries. Pruning by window age is not implemented yet, so today
        // the whole set must survive a transition untouched.
        let nullifier = PowNullifier::from(Fr::ONE);
        let mut state = PowState::new();
        state.update_from_claim_execution_result(&claim_result(0, nullifier));

        let new_state = state.try_apply_header((), &epoch_state(0), &epoch_state(1));

        assert!(new_state.nullifiers().contains(&nullifier));
    }

    #[test]
    fn pow_state_serde_round_trip() {
        // PowState is consensus state carried per block; `reward_difficulty`
        // serializes through the custom `serde_fr` codec and the nullifier
        // set through rpds. A round trip must reproduce the state exactly,
        // including a pending (not yet credited) refill.
        let mut state = PowState::new();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool::<TestConstants>();
        state.update_from_claim_execution_result(&claim_result(990, PowNullifier::from(Fr::ONE)));
        state.add_reward_refill_rewards(123);

        let json = serde_json::to_string(&state).expect("PowState should serialize");
        let restored: PowState = serde_json::from_str(&json).expect("PowState should deserialize");

        assert_eq!(restored, state);
    }
}
