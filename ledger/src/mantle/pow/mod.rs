pub(crate) mod blend_difficulty;
mod difficulty;
pub(crate) mod tx_density;

use lb_core::{
    crypto::Hash,
    mantle::{
        Value,
        ops::pow::{ClaimPoWRewardExecutionContext, PowNullifier, PowReward, PowTarget},
    },
};
use lb_cryptarchia_engine::Slot;
use lb_groth16::serde::serde_fr;
use rpds::HashTrieMapSync;

use crate::{
    EpochState,
    config::RewardPoWConfig,
    mantle::pow::{
        difficulty::compute_new_reward_difficulty,
        tx_density::{ClosedEpochLoad, TxDensity},
    },
};

/// `PoW` state of the mantle ledger.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PowState {
    /// State of the token-reward role: the pool, its per-claim reward, and the
    /// threshold and nullifiers that govern claiming.
    reward: RewardPowState,
    /// State of the Blend-admission role.
    blend: BlendPowState,
}

/// State of the `PoW` role that mints token rewards.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RewardPowState {
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
    /// Values are the **claimed** slot used to trim after the validation window
    /// expires.
    nullifiers: HashTrieMapSync<PowNullifier, Slot>,
    /// Slots of recently seen blocks by hash, retained for the
    /// window-of-acceptance check and pruned as they age out of the
    /// configured acceptance window
    /// ([`slot_window`](crate::config::RewardPoWConfig::slot_window)). Keyed
    /// by the wire-format block hash — the same
    /// value a `ClaimPowRewardOp` anchors to — so consensus state stays
    /// independent of the node's header-id type.
    block_slots: HashTrieMapSync<Hash, Slot>,
}

/// State of the `PoW` role that admits messages to the Blend network.
///
/// `d_blend` itself is not here: it must be frozen at the same moment as the
/// epoch nonce, so it lives in the `EpochState` that snapshot produces. What
/// this holds is the observation that threshold is computed *from*.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlendPowState {
    /// Per-epoch block and transaction counts, closed at each epoch boundary
    /// and read at the next nonce snapshot.
    tx_density: TxDensity,
}

impl PowState {
    /// Create the genesis `PoW` state from `config`: pool and per-claim reward
    /// seeded from the genesis endowment, initial difficulty derived from them,
    /// and no claims or seen blocks yet.
    #[must_use]
    pub fn from_reward_config(config: &RewardPoWConfig) -> Self {
        Self {
            reward: RewardPowState {
                reward_pool: config.reward_pool_genesis,
                epoch_reward: config.epoch_reward_genesis,
                reward_difficulty: compute_new_reward_difficulty(
                    config.initial_difficulty_seed,
                    PowTarget::from(config.epoch_reward_genesis),
                    config,
                ),
                refill_rewards: 0,
                nullifiers: HashTrieMapSync::new_sync(),
                block_slots: HashTrieMapSync::new_sync(),
            },
            blend: BlendPowState::default(),
        }
    }

    /// `R_PoW`: current balance of the `PoW` reward pool.
    #[must_use]
    pub const fn reward_pool(&self) -> Value {
        self.reward.reward_pool
    }

    /// `sigma_e`: reward per claim for the current epoch.
    #[must_use]
    pub const fn epoch_reward(&self) -> Value {
        self.reward.epoch_reward
    }

    /// Nullifiers of already-claimed `PoW` solutions.
    #[must_use]
    pub const fn nullifiers(&self) -> &HashTrieMapSync<PowNullifier, Slot> {
        &self.reward.nullifiers
    }

    /// `d_reward`: the current reward difficulty a puzzle ticket must be
    /// strictly below.
    #[must_use]
    pub const fn reward_difficulty(&self) -> PowTarget {
        self.reward.reward_difficulty
    }

    /// Apply the outcome of a [`ClaimPowRewardOp`] execution to this state.
    ///
    /// [`ClaimPowRewardOp`]: lb_core::mantle::ops::pow::ClaimPowRewardOp
    pub fn update_from_claim_execution_result(&mut self, context: &ClaimPoWRewardExecutionContext) {
        self.reward.nullifiers = context.nullifiers.clone();
        self.reward.reward_pool = context.reward_pool;
    }

    /// Move the epoch's collected `refill_rewards` into the `reward_pool`
    /// and recompute the per-claim `epoch_reward` from it.
    pub(crate) fn add_rewards_to_pool(&mut self, config: &RewardPoWConfig) {
        self.reward.reward_pool = self
            .reward
            .reward_pool
            .saturating_add(self.reward.refill_rewards);
        self.reward.refill_rewards = 0;
        self.reward.epoch_reward = compute_epoch_pow_reward(self.reward.reward_pool, config);
    }

    /// Add `reward` to the current epoch's pending `refill_rewards`.
    pub(crate) const fn add_reward_refill_rewards(&mut self, reward: PowReward) {
        self.reward.refill_rewards = self.reward.refill_rewards.saturating_add(reward);
    }

    pub(crate) fn update_difficulty(&mut self, claims_in_block: u64, config: &RewardPoWConfig) {
        self.reward.reward_difficulty =
            compute_new_reward_difficulty(claims_in_block, self.reward.reward_difficulty, config);
    }

    /// Slots of the recently seen blocks a claim may anchor to, by hash.
    #[must_use]
    pub const fn block_slots(&self) -> &HashTrieMapSync<Hash, Slot> {
        &self.reward.block_slots
    }

    /// Record the slot of a newly applied block.
    pub(crate) fn add_seen_block_slots(&mut self, block_hash: Hash, slot: Slot) {
        self.reward.block_slots.insert_mut(block_hash, slot);
    }

    /// Drop seen blocks that have aged out of the acceptance window: the
    /// window check rejects them regardless, so they no longer need to be
    /// retained (§5.1.1).
    pub(crate) fn prune_seen_block_slots(&mut self, current: Slot, slot_window: u64) {
        let cutoff = current.saturating_sub(Slot::from(slot_window));
        self.reward.block_slots = self
            .reward
            .block_slots
            .into_iter()
            .filter_map(|(&hash, &slot)| (slot >= cutoff).then_some((hash, slot)))
            .collect();
    }

    /// Drop seen nullifiers that have aged out of the acceptance window: the
    /// window check rejects them regardless, so they no longer need to be
    /// retained (§5.1.1).
    pub(crate) fn prune_nullifiers_by_slots(&mut self, current: Slot, slot_window: u64) {
        let cutoff = current.saturating_sub(Slot::from(slot_window));
        self.reward.nullifiers = self
            .reward
            .nullifiers
            .into_iter()
            .filter_map(|(&nullifier, &slot)| (slot >= cutoff).then_some((nullifier, slot)))
            .collect();
    }

    /// Count a block, and the transactions it carried, into the current
    /// epoch's totals.
    pub(crate) const fn record_block_txs(&mut self, txs_in_block: u64) {
        self.blend.tx_density.record_block(txs_in_block);
    }

    /// The load of the last epoch to close — the observation the Blend
    /// difficulty retarget reads — or `None` while no epoch has closed yet.
    pub(crate) const fn last_closed_epoch_load(&self) -> Option<ClosedEpochLoad> {
        self.blend.tx_density.last_closed_epoch_load()
    }

    /// Apply an epoch transition: on epoch change, refill the reward pool from
    /// the rewards collected during `previous_epoch`, and close that epoch's
    /// transaction totals so the Blend retarget can read them.
    pub(crate) fn try_apply_header(
        &self,
        previous_epoch: &EpochState,
        next_epoch: &EpochState,
        config: &RewardPoWConfig,
    ) -> Self {
        if previous_epoch.epoch >= next_epoch.epoch {
            return self.clone();
        }
        let mut new_self = self.clone();
        new_self.add_rewards_to_pool(config);
        // Once per epoch crossed, so epochs skipped entirely close as empty
        // and are read as no load — matching how the other per-epoch
        // rotations treat them.
        for _ in u32::from(previous_epoch.epoch)..u32::from(next_epoch.epoch) {
            new_self.blend.tx_density.close_epoch();
        }
        new_self
    }
}

#[cfg(test)]
impl PowState {
    /// Test-only: seed the reward difficulty directly, standing in for the
    /// genesis initial-difficulty seeding that is not implemented yet.
    pub(crate) const fn set_reward_difficulty(&mut self, difficulty: PowTarget) {
        self.reward.reward_difficulty = difficulty;
    }
}

/// Compute the per-claim `sigma_e` reward for the epoch from the current
/// `PoW` reward pool balance, per the deployment's payout rate
/// (`config.rate_num / config.claim_rate_denominator()`).
///
/// The intermediate product is widened to `u128` so a full pool
/// (`u64::MAX`, reachable through saturation) cannot overflow with a
/// `rate_num` greater than one; a result beyond `u64` saturates.
#[must_use]
pub fn compute_epoch_pow_reward(pow_reward_pool: PowReward, config: &RewardPoWConfig) -> PowReward {
    let denominator = u64::from(config.claim_rate_denominator());
    let reward =
        u128::from(pow_reward_pool) * u128::from(config.rate_num) / u128::from(denominator);
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
            blend_pow_difficulty: PowTarget::ZERO,
            utxos: UtxoTree::default(),
            total_stake: 0,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
            active_declarations: Arc::new(Declarations::default()),
        }
    }

    use std::num::NonZeroU64;

    use lb_core::mantle::ops::pow::SLOT_WINDOW;

    /// Genesis endowment values used by [`reward_config`], pinned here so the
    /// assertions read against a fixed number.
    const POW_REWARD_POOL_GENESIS: PowReward = 1_000_000_000;
    const POW_EPOCH_REWARD_POOL_GENESIS: PowReward = 1_000_000;

    /// A reward config with claiming disabled (`rate_num = 0`), standing in for
    /// a real deployment config in tests that don't exercise the payout rate.
    fn reward_config() -> RewardPoWConfig {
        RewardPoWConfig {
            reward_pool_genesis: POW_REWARD_POOL_GENESIS,
            epoch_reward_genesis: POW_EPOCH_REWARD_POOL_GENESIS,
            initial_difficulty_seed: 1_000,
            ema_smoothing_factor: 9,
            ema_smoothing_precision: NonZeroU64::new(10).expect("10 is non-zero"),
            target_claims_per_block: 100,
            rate_num: 0,
            rate_den: NonZeroU64::MIN,
            target_claim_per_block: NonZeroU64::MIN,
            expected_blocks_per_epoch: NonZeroU64::MIN,
            slot_window: NonZeroU64::new(SLOT_WINDOW).expect("SLOT_WINDOW is non-zero"),
        }
    }

    /// Genesis `PoW` state built from [`reward_config`].
    fn pow_state() -> PowState {
        PowState::from_reward_config(&reward_config())
    }

    /// A payout rate of `1/100`: `sigma_e = pool / 100` (rate `1`, denominator
    /// `1 * 10 * 10`).
    fn test_pool_config() -> RewardPoWConfig {
        RewardPoWConfig {
            rate_num: 1,
            rate_den: NonZeroU64::MIN,
            target_claim_per_block: NonZeroU64::new(10).expect("10 is non-zero"),
            expected_blocks_per_epoch: NonZeroU64::new(10).expect("10 is non-zero"),
            ..reward_config()
        }
    }

    const BLOCK_A: Hash = [1u8; 32];
    const BLOCK_B: Hash = [2u8; 32];

    #[test]
    fn new_state_starts_with_genesis_values() {
        let state = pow_state();
        assert_eq!(state.reward_pool(), POW_REWARD_POOL_GENESIS);
        assert_eq!(state.epoch_reward(), POW_EPOCH_REWARD_POOL_GENESIS);
        // The initial difficulty is seeded too — a zero target would be an
        // absorbing state no claim could ever satisfy.
        assert_ne!(state.reward_difficulty(), PowTarget::default());
        assert!(state.nullifiers().is_empty());
        assert!(state.block_slots().is_empty());
    }

    #[test]
    fn compute_epoch_pow_reward_applies_rate() {
        let config = test_pool_config();
        assert_eq!(compute_epoch_pow_reward(1_000, &config), 10);
        assert_eq!(compute_epoch_pow_reward(0, &config), 0);
        // Rounds down when the pool doesn't divide the rate evenly.
        assert_eq!(compute_epoch_pow_reward(150, &config), 1);
        assert_eq!(compute_epoch_pow_reward(99, &config), 0);
    }

    #[test]
    fn compute_epoch_pow_reward_disabled_is_always_zero() {
        // The default config disables claiming (`rate_num = 0`).
        assert_eq!(compute_epoch_pow_reward(u64::MAX, &reward_config()), 0);
    }

    #[test]
    fn compute_epoch_pow_reward_does_not_overflow_on_full_pool() {
        // A rate with rate_num > 1: `sigma_e = pool * 2 / 4`.
        let high_rate = RewardPoWConfig {
            rate_num: 2,
            rate_den: NonZeroU64::MIN,
            target_claim_per_block: NonZeroU64::MIN,
            expected_blocks_per_epoch: NonZeroU64::new(4).expect("4 is non-zero"),
            ..reward_config()
        };

        // The pool can legitimately reach u64::MAX (it saturates there), so
        // `pool * rate_num` must be widened past u64 or it overflows for any
        // rate_num > 1.
        assert_eq!(compute_epoch_pow_reward(u64::MAX, &high_rate), u64::MAX / 2);
    }

    #[test]
    fn add_rewards_to_pool_moves_refill_and_computes_reward() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool(&test_pool_config());

        assert_eq!(state.reward_pool(), POW_REWARD_POOL_GENESIS + 1_000);
        assert_eq!(
            state.epoch_reward(),
            (POW_REWARD_POOL_GENESIS + 1_000) / 100
        );
    }

    #[test]
    fn add_rewards_to_pool_accumulates_across_multiple_refills() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(400);
        state.add_reward_refill_rewards(600);
        state.add_rewards_to_pool(&test_pool_config());

        assert_eq!(state.reward_pool(), POW_REWARD_POOL_GENESIS + 1_000);
    }

    #[test]
    fn add_rewards_to_pool_is_noop_on_pool_when_no_refill_is_pending() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool(&test_pool_config());
        // Refill was reset by the call above: applying again must not add
        // anything further to the pool.
        state.add_rewards_to_pool(&test_pool_config());

        assert_eq!(state.reward_pool(), POW_REWARD_POOL_GENESIS + 1_000);
        assert_eq!(
            state.epoch_reward(),
            (POW_REWARD_POOL_GENESIS + 1_000) / 100
        );
    }

    #[test]
    fn add_rewards_to_pool_recomputes_reward_from_new_pool_each_time() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool(&test_pool_config());
        assert_eq!(
            state.epoch_reward(),
            (POW_REWARD_POOL_GENESIS + 1_000) / 100
        );

        state.add_reward_refill_rewards(9_000);
        state.add_rewards_to_pool(&test_pool_config());

        assert_eq!(state.reward_pool(), POW_REWARD_POOL_GENESIS + 10_000);
        assert_eq!(
            state.epoch_reward(),
            (POW_REWARD_POOL_GENESIS + 10_000) / 100
        );
    }

    #[test]
    fn refill_rewards_saturate_instead_of_overflowing() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(u64::MAX);
        state.add_reward_refill_rewards(1);
        state.add_rewards_to_pool(&test_pool_config());

        assert_eq!(state.reward_pool(), u64::MAX);
    }

    #[test]
    fn reward_pool_saturates_instead_of_overflowing() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(u64::MAX);
        state.add_rewards_to_pool(&test_pool_config());
        state.add_reward_refill_rewards(u64::MAX);
        state.add_rewards_to_pool(&test_pool_config());

        assert_eq!(state.reward_pool(), u64::MAX);
    }

    #[test]
    fn try_apply_header_is_noop_when_epoch_does_not_advance() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(500);
        let same_epoch = epoch_state(3);

        let unchanged = state.try_apply_header(&same_epoch, &same_epoch, &reward_config());

        assert_eq!(unchanged, state);
        assert_eq!(unchanged.reward_pool(), POW_REWARD_POOL_GENESIS);
    }

    #[test]
    fn try_apply_header_is_noop_when_epoch_goes_backwards() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(500);
        let earlier = epoch_state(1);
        let later = epoch_state(5);

        // `next_epoch` behind `previous_epoch`, e.g. a stale/reorged branch.
        let unchanged = state.try_apply_header(&later, &earlier, &reward_config());

        assert_eq!(unchanged, state);
    }

    #[test]
    fn try_apply_header_does_not_mutate_the_receiver() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(500);
        let original = state.clone();
        let previous = epoch_state(0);
        let next = epoch_state(1);

        drop(state.try_apply_header(&previous, &next, &reward_config()));

        assert_eq!(state, original);
    }

    #[test]
    fn try_apply_header_moves_pending_refill_into_pool_on_advance() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(500);
        let previous = epoch_state(0);
        let next = epoch_state(1);

        let new_state = state.try_apply_header(&previous, &next, &reward_config());

        assert_eq!(new_state.reward_pool(), POW_REWARD_POOL_GENESIS + 500);
    }

    #[test]
    fn try_apply_header_leaves_reward_claiming_disabled() {
        // With the default reward config (claiming disabled, `rate_num = 0`),
        // `epoch_reward` is zeroed at the first transition even though the
        // pool is well funded.
        let mut state = pow_state();
        state.add_reward_refill_rewards(1_000_000);
        let previous = epoch_state(0);
        let next = epoch_state(1);

        let new_state = state.try_apply_header(&previous, &next, &reward_config());

        assert_eq!(new_state.reward_pool(), POW_REWARD_POOL_GENESIS + 1_000_000);
        assert_eq!(new_state.epoch_reward(), 0);
    }

    #[test]
    fn try_apply_header_across_multiple_epoch_jump_applies_once() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(500);
        let previous = epoch_state(0);
        let next = epoch_state(5);

        let new_state = state.try_apply_header(&previous, &next, &reward_config());

        assert_eq!(new_state.reward_pool(), POW_REWARD_POOL_GENESIS + 500);
    }

    #[test]
    fn try_apply_header_preserves_pending_refill_across_noop_transitions() {
        // A no-op transition (same epoch) must not drop a refill that
        // hasn't been credited to the pool yet.
        let mut state = pow_state();
        state.add_reward_refill_rewards(200);
        let same = epoch_state(2);
        let mut state = state.try_apply_header(&same, &same, &reward_config());

        state.add_reward_refill_rewards(300);
        let previous = epoch_state(2);
        let next = epoch_state(3);
        let new_state = state.try_apply_header(&previous, &next, &reward_config());

        assert_eq!(new_state.reward_pool(), POW_REWARD_POOL_GENESIS + 500);
    }

    #[test]
    fn seen_block_slots_are_pruned_once_they_age_out_of_the_window() {
        let mut state = pow_state();

        // Block A at slot 5, then block B exactly SLOT_WINDOW later: A sits
        // right on the cutoff (`current - WINDOW`) and must survive, since
        // the window check still accepts a gap equal to the window.
        state.add_seen_block_slots(BLOCK_A, Slot::from(5u64));
        state.add_seen_block_slots(BLOCK_B, Slot::from(5 + SLOT_WINDOW));
        state.prune_seen_block_slots(Slot::from(5 + SLOT_WINDOW), SLOT_WINDOW);
        assert!(state.block_slots().contains_key(&BLOCK_A));
        assert!(state.block_slots().contains_key(&BLOCK_B));

        // One slot further, A is strictly older than the window and is
        // pruned; B remains.
        state.prune_seen_block_slots(Slot::from(5 + SLOT_WINDOW + 1), SLOT_WINDOW);
        assert!(!state.block_slots().contains_key(&BLOCK_A));
        assert!(state.block_slots().contains_key(&BLOCK_B));
    }

    #[test]
    fn nullifiers_are_pruned_once_they_age_out_of_the_window() {
        // Spent solutions are retained only for `SLOT_WINDOW`: once their
        // claim slot ages out, the window check rejects any reuse anyway, so
        // the nullifier can be dropped (§5.1.1).
        let old_nullifier = PowNullifier::from(Fr::ONE);
        let recent_nullifier = PowNullifier::from(Fr::from(2u64));
        let nullifiers = HashTrieMapSync::new_sync()
            .insert(old_nullifier, Slot::from(5u64))
            .insert(recent_nullifier, Slot::from(5 + SLOT_WINDOW));
        let mut state = pow_state();
        state.update_from_claim_execution_result(&ClaimPoWRewardExecutionContext {
            reward_pool: state.reward_pool(),
            epoch_reward: 0,
            nullifiers,
            tx_hash: TxHash::from([7u8; 32]),
            utxos: Utxos::new(),
            block_slots: HashTrieMapSync::new_sync(),
        });

        // A claim slot exactly `SLOT_WINDOW` back sits on the cutoff and must
        // survive, matching the inclusive window check.
        state.prune_nullifiers_by_slots(Slot::from(5 + SLOT_WINDOW), SLOT_WINDOW);
        assert!(state.nullifiers().contains_key(&old_nullifier));
        assert!(state.nullifiers().contains_key(&recent_nullifier));

        // One slot further, the old nullifier is strictly outside the window
        // and is dropped; the recent one remains.
        state.prune_nullifiers_by_slots(Slot::from(5 + SLOT_WINDOW + 1), SLOT_WINDOW);
        assert!(!state.nullifiers().contains_key(&old_nullifier));
        assert!(state.nullifiers().contains_key(&recent_nullifier));
    }

    #[test]
    fn update_from_claim_execution_result_replaces_pool_and_nullifiers() {
        let mut state = pow_state();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool(&test_pool_config());
        let epoch_reward = (POW_REWARD_POOL_GENESIS + 1_000) / 100;
        assert_eq!(state.reward_pool(), POW_REWARD_POOL_GENESIS + 1_000);
        assert_eq!(state.epoch_reward(), epoch_reward);

        let nullifier = PowNullifier::from(Fr::ONE);
        let nullifiers = HashTrieMapSync::new_sync().insert(nullifier, Slot::from(7u64));
        let context = ClaimPoWRewardExecutionContext {
            reward_pool: 990,
            epoch_reward,
            nullifiers: nullifiers.clone(),
            tx_hash: TxHash::from([7u8; 32]),
            utxos: Utxos::new(),
            block_slots: HashTrieMapSync::new_sync(),
        };

        state.update_from_claim_execution_result(&context);

        assert_eq!(state.reward_pool(), 990);
        assert_eq!(state.nullifiers(), &nullifiers);
        assert!(state.nullifiers().contains_key(&nullifier));
        // Unrelated fields are left untouched by this update.
        assert_eq!(state.epoch_reward(), epoch_reward);
    }

    /// Build a claim execution result that drains the pool to `reward_pool`,
    /// recording `nullifier` as spent.
    fn claim_result(
        reward_pool: PowReward,
        nullifier: PowNullifier,
    ) -> ClaimPoWRewardExecutionContext {
        ClaimPoWRewardExecutionContext {
            reward_pool,
            epoch_reward: 0,
            nullifiers: HashTrieMapSync::new_sync().insert(nullifier, Slot::from(7u64)),
            tx_hash: TxHash::from([7u8; 32]),
            utxos: Utxos::new(),
            block_slots: HashTrieMapSync::new_sync(),
        }
    }

    #[test]
    fn epoch_reward_tapers_as_claims_drain_the_pool() {
        // Spec §5.6: sigma_e is recomputed at each boundary from the pool as
        // claims left it, so a drained pool pays a smaller per-claim reward
        // in the next epoch, tapering to zero (the safety cutoff's input).
        let mut state = pow_state();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool(&test_pool_config());
        assert_eq!(
            state.epoch_reward(),
            (POW_REWARD_POOL_GENESIS + 1_000) / 100
        );

        // Claims drain the pool down to 990.
        state.update_from_claim_execution_result(&claim_result(990, PowNullifier::from(Fr::ONE)));
        state.add_rewards_to_pool(&test_pool_config());
        assert_eq!(state.epoch_reward(), 9);

        // Drained below the payout rate, sigma_e floors to zero and the
        // safety cutoff (§5.6 `pow_reward_enabled`) would disable claiming.
        state.update_from_claim_execution_result(&claim_result(99, PowNullifier::from(Fr::ONE)));
        state.add_rewards_to_pool(&test_pool_config());
        assert_eq!(state.epoch_reward(), 0);
    }

    #[test]
    fn claim_execution_result_does_not_clobber_pending_refill() {
        // Spec §5.8: within an epoch the spendable pool is touched only by
        // claim draws; the refill accrues on the side and lands whole at the
        // boundary. A claim applied after refills have accrued must not
        // discard them.
        let mut state = pow_state();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool(&test_pool_config());

        // Mid-epoch: block rewards accrue, then a claim drains the pool.
        state.add_reward_refill_rewards(500);
        state.update_from_claim_execution_result(&claim_result(990, PowNullifier::from(Fr::ONE)));

        // Boundary: the refill is credited on top of the post-claim pool,
        // and sigma_e is snapshotted from the refilled pool (§5.6 ordering).
        state.add_rewards_to_pool(&test_pool_config());
        assert_eq!(state.reward_pool(), 1_490);
        assert_eq!(state.epoch_reward(), 14);
    }

    #[test]
    fn try_apply_header_carries_nullifiers_forward() {
        // Spec §5.5/§5.1.1: spent solutions must stay rejected while their
        // block_hash is inside the acceptance window, which spans epoch
        // boundaries. Nullifier pruning by window age is not implemented
        // yet, so today the whole set must survive a transition untouched.
        let nullifier = PowNullifier::from(Fr::ONE);
        let mut state = pow_state();
        state.update_from_claim_execution_result(&claim_result(0, nullifier));

        let new_state = state.try_apply_header(&epoch_state(0), &epoch_state(1), &reward_config());

        assert!(new_state.nullifiers().contains_key(&nullifier));
    }

    #[test]
    fn pow_state_serde_round_trip() {
        // PowState is consensus state carried per block; `reward_difficulty`
        // serializes through the custom `serde_fr` codec and the nullifier
        // set through rpds. A round trip must reproduce the state exactly,
        // including a pending (not yet credited) refill.
        let mut state = pow_state();
        state.add_reward_refill_rewards(1_000);
        state.add_rewards_to_pool(&test_pool_config());
        state.update_from_claim_execution_result(&claim_result(990, PowNullifier::from(Fr::ONE)));
        state.add_reward_refill_rewards(123);

        let json = serde_json::to_string(&state).expect("PowState should serialize");
        let restored: PowState = serde_json::from_str(&json).expect("PowState should deserialize");

        assert_eq!(restored, state);
    }
}
