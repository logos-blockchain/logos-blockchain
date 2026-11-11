pub(crate) mod blend_difficulty;
mod difficulty;
pub(crate) mod tx_density;

use lb_core::{
    crypto::Hash,
    mantle::{
        Value,
        ops::pow::{
            ClaimPoWRewardExecutionContext, PowNullifier, PowReward, PowSlotWindows, PowTarget,
        },
    },
};
use lb_cryptarchia_engine::Slot;
use lb_groth16::serde::serde_fr;
use rpds::HashTrieMapSync;

use crate::{
    EpochState,
    block_rewards::MAX_SUPPLY,
    config::{Config, RewardPoWConfig},
    mantle::pow::{
        difficulty::compute_new_reward_difficulty,
        tx_density::{ClosedEpochLoad, TxDensity},
    },
};

/// `POW_REWARD_POOL_GENESIS`: the balance the pool starts with, five
/// thousandths of the maximum supply (Initial Proof of Work Reward Pool in the
/// genesis spec).
pub const POW_REWARD_POOL_GENESIS: PowReward = MAX_SUPPLY / 1000 * 5;

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
    /// `R_PoW`: reserve funding `PoW` rewards. Credited by each block with the
    /// `PoW` share (`beta`) of its collected fees. Drained by `sigma_e` as
    /// rewards are claimed.
    reward_pool: PowReward,
    /// `sigma_e`: reward per claim, fixed for the epoch.
    epoch_reward: PowReward,
    /// `d_reward`: the REWARD threshold, retargeted every block
    #[serde(with = "serde_fr")]
    reward_difficulty: PowTarget,
    /// Spent `PoW` solutions, retained only for the acceptance window.
    /// Values are the slot of the block the claim anchored to, used to trim
    /// them once the window of that block's era expires: from then on the
    /// window check rejects any reuse.
    nullifiers: HashTrieMapSync<PowNullifier, Slot>,
    /// Slots of recently seen blocks by hash, retained for the
    /// window-of-acceptance check and pruned once the acceptance window
    /// ([`slot_window`](crate::config::RewardPoWConfig::slot_window)) of the
    /// era of each block closes. Keyed
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
    /// Create the genesis `PoW` state: the pool seeded with `pow_reward_pool`,
    /// the per-claim reward derived from it as at every epoch boundary, so that
    /// claims pay from the first epoch, difficulty taken straight from the
    /// configured exponent, and no claims or seen blocks yet.
    #[must_use]
    pub fn from_reward_pool_and_config(pow_reward_pool: PowReward, config: &Config) -> Self {
        Self {
            reward: RewardPowState {
                reward_pool: pow_reward_pool,
                epoch_reward: compute_epoch_pow_reward(pow_reward_pool, config),
                reward_difficulty: PowTarget::from(config.pow_config.reward.minimum_difficulty),
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

    /// Recompute the per-claim `epoch_reward` from the `reward_pool`.
    pub(crate) fn update_epoch_reward(&mut self, config: &Config) {
        self.reward.epoch_reward = compute_epoch_pow_reward(self.reward.reward_pool, config);
    }

    /// Credit the `reward_pool` with the `PoW` share of a block's fees. The
    /// `epoch_reward` stays as it is until the next epoch boundary.
    pub(crate) fn credit_reward_pool(
        &mut self,
        reward: PowReward,
    ) -> Result<(), PowRewardPoolOverflow> {
        self.reward.reward_pool = self
            .reward
            .reward_pool
            .checked_add(reward)
            .ok_or(PowRewardPoolOverflow)?;
        Ok(())
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

    /// Drop seen blocks that have aged out of the acceptance window of their
    /// era: the window check rejects them regardless, so they no longer need
    /// to be retained (§5.1.1).
    pub(crate) fn prune_seen_block_slots(&mut self, current: Slot, slot_windows: &PowSlotWindows) {
        self.reward.block_slots = self
            .reward
            .block_slots
            .into_iter()
            .filter_map(|(&hash, &slot)| {
                is_within_slot_window(slot, current, slot_windows).then_some((hash, slot))
            })
            .collect();
    }

    /// Drop seen nullifiers whose anchor block has aged out of the acceptance
    /// window of its era: the window check rejects any reuse regardless, so
    /// they no longer need to be retained (§5.1.1).
    pub(crate) fn prune_nullifiers_by_slots(
        &mut self,
        current: Slot,
        slot_windows: &PowSlotWindows,
    ) {
        self.reward.nullifiers = self
            .reward
            .nullifiers
            .into_iter()
            .filter_map(|(&nullifier, &slot)| {
                is_within_slot_window(slot, current, slot_windows).then_some((nullifier, slot))
            })
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

    /// Apply an epoch transition: on epoch change, recompute the per-claim
    /// reward from the pool, and close `previous_epoch`'s transaction totals
    /// so the Blend retarget can read them.
    pub(crate) fn apply_header(
        &self,
        previous_epoch: &EpochState,
        next_epoch: &EpochState,
        config: &Config,
    ) -> Self {
        if previous_epoch.epoch >= next_epoch.epoch {
            return self.clone();
        }
        let mut new_self = self.clone();
        new_self.update_epoch_reward(config);
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

    /// Test-only: set the pool directly, standing in for the claims that
    /// drained it.
    pub(crate) const fn set_reward_pool(&mut self, reward_pool: PowReward) {
        self.reward.reward_pool = reward_pool;
    }
}

/// Compute the per-claim `sigma_e` reward for the epoch from the current
/// `PoW` reward pool balance, per the deployment's payout rate (`rate_num`
/// over [`Config::claim_rate_denominator`]).
///
/// The whole computation is widened to `u128` so a full pool (`u64::MAX`)
/// cannot overflow with a `rate_num` greater than one; a result beyond `u64`
/// saturates.
#[must_use]
pub fn compute_epoch_pow_reward(pow_reward_pool: PowReward, config: &Config) -> PowReward {
    let reward = u128::from(pow_reward_pool) * u128::from(config.pow_config.reward.rate_num)
        / config.claim_rate_denominator().get();
    PowReward::try_from(reward).unwrap_or(PowReward::MAX)
}

/// Whether a claim anchored to a block at `anchor_slot` may still be judged
/// inside the acceptance window of the block's era at `current`, as the
/// window check counts it: `current - anchor_slot <= slot_window`.
fn is_within_slot_window(anchor_slot: Slot, current: Slot, slot_windows: &PowSlotWindows) -> bool {
    let slot_window = slot_windows.at_slot(anchor_slot).parameters;
    current.saturating_sub(anchor_slot) <= Slot::from(slot_window.get())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("PoW reward pool overflow")]
pub struct PowRewardPoolOverflow;

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, sync::Arc};

    use lb_core::{
        mantle::{
            note_set::{NoteSets, TxNoteState},
            transactions::hash::TxHash,
        },
        sdp::Declarations,
    };
    use lb_cryptarchia_engine::Epoch;
    use lb_groth16::{AdditiveGroup as _, Field as _, Fr, fr_to_bytes};
    use num_bigint::BigUint;

    use super::*;
    use crate::{
        config::{ModulusShift, pow_slot_windows},
        test_utils::schedule,
    };

    const SLOT_WINDOW: NonZeroU64 = NonZeroU64::new(100).expect("100 is not 0");

    /// The windows of a single era, of [`SLOT_WINDOW`].
    fn single_era_slot_windows() -> PowSlotWindows {
        pow_slot_windows(&schedule(ledger_config(), []))
    }

    /// The windows of two eras, `genesis_window` then, from epoch 1 on,
    /// `later_window`, and the first slot of the later era.
    fn two_era_slot_windows(genesis_window: u64, later_window: u64) -> (PowSlotWindows, u64) {
        let with_window = |slot_window| {
            let mut config = ledger_config();
            config.pow_config.reward.slot_window = NonZeroU64::new(slot_window).unwrap();
            config
        };
        let eras = schedule(
            with_window(genesis_window),
            [(1, with_window(later_window))],
        );
        let later_era_start = eras.starting_slot_for_epoch(Epoch::new(1)).into_inner();
        (pow_slot_windows(&eras), later_era_start)
    }

    fn epoch_state(epoch: u32) -> EpochState {
        EpochState {
            epoch: epoch.into(),
            nonce: Fr::ZERO,
            blend_pow_difficulty: PowTarget::ZERO,
            note_sets: NoteSets::new(),
            total_stake: 0,
            lottery_0: Fr::ZERO,
            lottery_1: Fr::ZERO,
            active_declarations: Arc::new(Declarations::default()),
        }
    }

    /// Genesis endowment values used by [`reward_config`], pinned here so the
    /// assertions read against a fixed number.
    const POW_REWARD_POOL_GENESIS: PowReward = 1_000_000_000;

    /// A reward config with claiming disabled (`rate_num = 0`), standing in for
    /// a real deployment config in tests that don't exercise the payout rate.
    fn reward_config() -> RewardPoWConfig {
        RewardPoWConfig {
            minimum_difficulty: ModulusShift::new::<26>(),
            ema_smoothing_factor: 9,
            ema_smoothing_precision: NonZeroU64::new(10).expect("10 is non-zero"),
            target_claims_per_block: 100,
            rate_num: 0,
            rate_den: NonZeroU64::MIN,
            target_claim_per_block: NonZeroU64::MIN,
            pow_share: 0,
            share_den: NonZeroU64::MIN,
            slot_window: NonZeroU64::new(SLOT_WINDOW.get()).expect("SLOT_WINDOW is non-zero"),
        }
    }

    /// Genesis `PoW` state built from [`ledger_config`].
    fn pow_state() -> PowState {
        PowState::from_reward_pool_and_config(POW_REWARD_POOL_GENESIS, &ledger_config())
    }

    /// A ledger config carrying [`reward_config`]. Its consensus schedule
    /// (`k = 1`, `f = 1/10`) works out to ten expected blocks per epoch — the
    /// derived factor of the payout denominator.
    fn ledger_config() -> Config {
        let mut config = crate::cryptarchia::tests::config();
        config.pow_config.reward = reward_config();
        config
    }

    /// A payout rate of `1/100`: `sigma_e = pool / 100` (rate `1`, denominator
    /// `1 * 10 * 10`, the last factor derived from the consensus schedule).
    fn test_pool_config() -> Config {
        let mut config = ledger_config();
        config.pow_config.reward = RewardPoWConfig {
            rate_num: 1,
            rate_den: NonZeroU64::MIN,
            target_claim_per_block: NonZeroU64::new(10).expect("10 is non-zero"),
            ..reward_config()
        };
        config
    }

    const BLOCK_A: Hash = [1u8; 32];
    const BLOCK_B: Hash = [2u8; 32];

    #[test]
    fn new_state_starts_with_genesis_values() {
        let state =
            PowState::from_reward_pool_and_config(POW_REWARD_POOL_GENESIS, &test_pool_config());
        assert_eq!(state.reward_pool(), POW_REWARD_POOL_GENESIS);
        // The per-claim reward the pool pays at its payout rate of `1/100`.
        assert_eq!(state.epoch_reward(), POW_REWARD_POOL_GENESIS / 100);
        // The genesis difficulty is exactly the configured exponent, not a
        // value derived from it. Asserting only that it is non-zero would not
        // notice a target off by orders of magnitude, which is what deriving
        // it from a token amount used to produce.
        assert_eq!(
            state.reward_difficulty(),
            PowTarget::from(ModulusShift::new::<26>())
        );
        assert!(state.nullifiers().is_empty());
        assert!(state.block_slots().is_empty());
    }

    /// The genesis difficulty has to be a sane fraction of the scalar field:
    /// `p / 2^26`, which is a ~68-digit number. A token-scale value such as
    /// the per-claim reward is ~7 digits, so a target derived from one is
    /// unreachable by tens of orders of magnitude and no ticket ever wins.
    /// This pins the scale rather than the exact constant.
    #[test]
    fn genesis_difficulty_is_field_scale_not_token_scale() {
        let difficulty = BigUint::from_bytes_le(&fr_to_bytes(&pow_state().reward_difficulty()));
        let field = BigUint::from_bytes_le(&fr_to_bytes(&-PowTarget::ONE));

        // Within a factor of 2^27 of the whole field, and nowhere near the
        // token amounts in `reward_config`.
        assert!(difficulty > &field >> 27u32);
        assert!(difficulty < field);
        assert!(difficulty > BigUint::from(u64::MAX));
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
        assert_eq!(compute_epoch_pow_reward(u64::MAX, &ledger_config()), 0);
    }

    #[test]
    fn compute_epoch_pow_reward_does_not_overflow_on_full_pool() {
        // A rate with rate_num > 1: `sigma_e = pool * 2 / 10`, the denominator
        // being the ten expected blocks per epoch.
        let mut high_rate = ledger_config();
        high_rate.pow_config.reward.rate_num = 2;

        // The pool can reach u64::MAX, so `pool * rate_num` must be widened
        // past u64 or it overflows for any rate_num > 1.
        assert_eq!(compute_epoch_pow_reward(u64::MAX, &high_rate), u64::MAX / 5);
    }

    #[test]
    fn credit_reward_pool_keeps_the_epoch_reward_until_the_boundary() {
        let config = test_pool_config();
        let mut state = pow_state();
        state.update_epoch_reward(&config);
        let epoch_reward = state.epoch_reward();

        state.credit_reward_pool(1_000).unwrap();
        assert_eq!(state.reward_pool(), POW_REWARD_POOL_GENESIS + 1_000);
        assert_eq!(state.epoch_reward(), epoch_reward);

        let new_state = state.apply_header(&epoch_state(0), &epoch_state(1), &config);
        assert_eq!(new_state.reward_pool(), POW_REWARD_POOL_GENESIS + 1_000);
        assert_eq!(
            new_state.epoch_reward(),
            (POW_REWARD_POOL_GENESIS + 1_000) / 100
        );
    }

    #[test]
    fn reward_pool_overflow_is_an_error() {
        // The pool starts at the genesis endowment, so adding u64::MAX to it
        // overflows.
        let mut state = pow_state();

        assert_eq!(
            state.credit_reward_pool(u64::MAX),
            Err(PowRewardPoolOverflow)
        );
    }

    #[test]
    fn apply_header_is_noop_when_epoch_does_not_advance() {
        let mut state = pow_state();
        state.credit_reward_pool(500).unwrap();
        let same_epoch = epoch_state(3);

        let unchanged = state.apply_header(&same_epoch, &same_epoch, &test_pool_config());

        assert_eq!(unchanged, state);
        assert_eq!(unchanged.reward_pool(), POW_REWARD_POOL_GENESIS + 500);
    }

    #[test]
    fn apply_header_is_noop_when_epoch_goes_backwards() {
        let mut state = pow_state();
        state.credit_reward_pool(500).unwrap();
        let earlier = epoch_state(1);
        let later = epoch_state(5);

        // `next_epoch` behind `previous_epoch`, e.g. a stale/reorged branch.
        let unchanged = state.apply_header(&later, &earlier, &test_pool_config());

        assert_eq!(unchanged, state);
    }

    #[test]
    fn apply_header_does_not_mutate_the_receiver() {
        let mut state = pow_state();
        state.credit_reward_pool(500).unwrap();
        let original = state.clone();

        drop(state.apply_header(&epoch_state(0), &epoch_state(1), &ledger_config()));

        assert_eq!(state, original);
    }

    #[test]
    fn apply_header_leaves_reward_claiming_disabled() {
        // With the default reward config (claiming disabled, `rate_num = 0`),
        // `epoch_reward` is zeroed at the first transition even though the
        // pool is well funded.
        let mut state = pow_state();
        state.credit_reward_pool(1_000_000).unwrap();

        let new_state = state.apply_header(&epoch_state(0), &epoch_state(1), &ledger_config());

        assert_eq!(new_state.reward_pool(), POW_REWARD_POOL_GENESIS + 1_000_000);
        assert_eq!(new_state.epoch_reward(), 0);
    }

    #[test]
    fn seen_block_slots_are_pruned_once_they_age_out_of_the_window() {
        let mut state = pow_state();
        let slot_windows = single_era_slot_windows();

        // Block A at slot 5, then block B exactly SLOT_WINDOW later: A sits
        // right on the cutoff (`current - WINDOW`) and must survive, since
        // the window check still accepts a gap equal to the window.
        state.add_seen_block_slots(BLOCK_A, Slot::from(5u64));
        state.add_seen_block_slots(BLOCK_B, Slot::from(5 + SLOT_WINDOW.get()));
        state.prune_seen_block_slots(Slot::from(5 + SLOT_WINDOW.get()), &slot_windows);
        assert!(state.block_slots().contains_key(&BLOCK_A));
        assert!(state.block_slots().contains_key(&BLOCK_B));

        // One slot further, A is strictly older than the window and is
        // pruned; B remains.
        state.prune_seen_block_slots(Slot::from(5 + SLOT_WINDOW.get() + 1), &slot_windows);
        assert!(!state.block_slots().contains_key(&BLOCK_A));
        assert!(state.block_slots().contains_key(&BLOCK_B));
    }

    #[test]
    fn nullifiers_are_pruned_once_they_age_out_of_the_window() {
        // Spent solutions are retained only for `SLOT_WINDOW`: once their
        // anchor slot ages out, the window check rejects any reuse anyway, so
        // the nullifier can be dropped (§5.1.1).
        let slot_windows = single_era_slot_windows();
        let old_nullifier = PowNullifier::from(Fr::ONE);
        let recent_nullifier = PowNullifier::from(Fr::from(2u64));
        let nullifiers = HashTrieMapSync::new_sync()
            .insert(old_nullifier, Slot::from(5u64))
            .insert(recent_nullifier, Slot::from(5 + SLOT_WINDOW.get()));
        let mut state = pow_state();
        state.update_from_claim_execution_result(&ClaimPoWRewardExecutionContext {
            reward_pool: state.reward_pool(),
            epoch_reward: 0,
            nullifiers,
            tx_hash: TxHash::from([7u8; 32]),
            note_sets: NoteSets::new(),
            tx_notes: &mut TxNoteState::new(),
            block_slots: HashTrieMapSync::new_sync(),
        });

        // An anchor slot exactly `SLOT_WINDOW` back sits on the cutoff and must
        // survive, matching the inclusive window check.
        state.prune_nullifiers_by_slots(Slot::from(5 + SLOT_WINDOW.get()), &slot_windows);
        assert!(state.nullifiers().contains_key(&old_nullifier));
        assert!(state.nullifiers().contains_key(&recent_nullifier));

        // One slot further, the old nullifier is strictly outside the window
        // and is dropped; the recent one remains.
        state.prune_nullifiers_by_slots(Slot::from(5 + SLOT_WINDOW.get() + 1), &slot_windows);
        assert!(!state.nullifiers().contains_key(&old_nullifier));
        assert!(state.nullifiers().contains_key(&recent_nullifier));
    }

    #[test]
    fn seen_block_slots_are_pruned_by_the_window_of_their_era() {
        // Era 0 keeps its blocks for 50 slots, era 1 for 10.
        let (slot_windows, era_1_start) = two_era_slot_windows(50, 10);
        let era_0_block = era_1_start - 1;
        let mut state = pow_state();
        state.add_seen_block_slots(BLOCK_A, Slot::from(era_0_block));
        state.add_seen_block_slots(BLOCK_B, Slot::from(era_1_start));

        // The block of era 1 goes once 10 slots old, while the older block of
        // era 0 stays: the window of a later era does not cut it short.
        state.prune_seen_block_slots(Slot::from(era_1_start + 11), &slot_windows);
        assert!(state.block_slots().contains_key(&BLOCK_A));
        assert!(!state.block_slots().contains_key(&BLOCK_B));

        // It goes once 50 slots old.
        state.prune_seen_block_slots(Slot::from(era_0_block + 50), &slot_windows);
        assert!(state.block_slots().contains_key(&BLOCK_A));
        state.prune_seen_block_slots(Slot::from(era_0_block + 51), &slot_windows);
        assert!(!state.block_slots().contains_key(&BLOCK_A));
    }

    #[test]
    fn nullifiers_are_kept_for_the_window_of_the_era_of_their_anchor_block() {
        // Era 0 has a window of 50, era 1 one of 10. A claim anchored to the
        // last block of era 0 can be made again for 50 slots, even when it was
        // claimed in era 1, so its nullifier must outlive era 1's window.
        let (slot_windows, era_1_start) = two_era_slot_windows(50, 10);
        let anchor = era_1_start - 1;
        let nullifier = PowNullifier::from(Fr::ONE);
        let mut state = pow_state();
        state.update_from_claim_execution_result(&ClaimPoWRewardExecutionContext {
            reward_pool: state.reward_pool(),
            epoch_reward: 0,
            nullifiers: HashTrieMapSync::new_sync().insert(nullifier, Slot::from(anchor)),
            tx_hash: TxHash::from([7u8; 32]),
            note_sets: NoteSets::new(),
            tx_notes: &mut TxNoteState::new(),
            block_slots: HashTrieMapSync::new_sync(),
        });

        state.prune_nullifiers_by_slots(Slot::from(era_1_start + 11), &slot_windows);
        assert!(state.nullifiers().contains_key(&nullifier));
        state.prune_nullifiers_by_slots(Slot::from(anchor + 50), &slot_windows);
        assert!(state.nullifiers().contains_key(&nullifier));
        state.prune_nullifiers_by_slots(Slot::from(anchor + 51), &slot_windows);
        assert!(!state.nullifiers().contains_key(&nullifier));
    }

    #[test]
    fn update_from_claim_execution_result_replaces_pool_and_nullifiers() {
        let mut state = pow_state();
        state.credit_reward_pool(1_000).unwrap();
        state.update_epoch_reward(&test_pool_config());
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
            note_sets: NoteSets::new(),
            tx_notes: &mut TxNoteState::new(),
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
        tx_notes: &mut TxNoteState,
    ) -> ClaimPoWRewardExecutionContext<'_> {
        ClaimPoWRewardExecutionContext {
            reward_pool,
            epoch_reward: 0,
            nullifiers: HashTrieMapSync::new_sync().insert(nullifier, Slot::from(7u64)),
            tx_hash: TxHash::from([7u8; 32]),
            note_sets: NoteSets::new(),
            tx_notes,
            block_slots: HashTrieMapSync::new_sync(),
        }
    }

    #[test]
    fn epoch_reward_tapers_as_claims_drain_the_pool() {
        // Spec §5.6: sigma_e is recomputed at each boundary from the pool as
        // claims and credits left it, so a drained pool pays a smaller per-claim reward
        // in the next epoch, tapering to zero (the safety cutoff's input).
        let mut state = pow_state();
        state.credit_reward_pool(1_000).unwrap();
        state.update_epoch_reward(&test_pool_config());
        assert_eq!(
            state.epoch_reward(),
            (POW_REWARD_POOL_GENESIS + 1_000) / 100
        );

        // Claims drain the pool down to 990.
        state.update_from_claim_execution_result(&claim_result(
            990,
            PowNullifier::from(Fr::ONE),
            &mut TxNoteState::new(),
        ));
        state.update_epoch_reward(&test_pool_config());
        assert_eq!(state.epoch_reward(), 9);

        // Drained below the payout rate, sigma_e floors to zero and the
        // safety cutoff (§5.6 `pow_reward_enabled`) would disable claiming.
        state.update_from_claim_execution_result(&claim_result(
            99,
            PowNullifier::from(Fr::ONE),
            &mut TxNoteState::new(),
        ));
        state.update_epoch_reward(&test_pool_config());
        assert_eq!(state.epoch_reward(), 0);
    }

    #[test]
    fn apply_header_carries_nullifiers_forward() {
        // Spec §5.5/§5.1.1: spent solutions must stay rejected while their
        // block_hash is inside the acceptance window, which spans epoch
        // boundaries. Nullifier pruning by window age is not implemented
        // yet, so today the whole set must survive a transition untouched.
        let nullifier = PowNullifier::from(Fr::ONE);
        let mut state = pow_state();
        state.update_from_claim_execution_result(&claim_result(
            0,
            nullifier,
            &mut TxNoteState::new(),
        ));

        let new_state = state.apply_header(&epoch_state(0), &epoch_state(1), &ledger_config());

        assert!(new_state.nullifiers().contains_key(&nullifier));
    }

    #[test]
    fn pow_state_serde_round_trip() {
        // PowState is consensus state carried per block; `reward_difficulty`
        // serializes through the custom `serde_fr` codec and the nullifier
        // set through rpds. A round trip must reproduce the state exactly.
        let mut state = pow_state();
        state.credit_reward_pool(1_000).unwrap();
        state.update_epoch_reward(&test_pool_config());
        state.update_from_claim_execution_result(&claim_result(
            990,
            PowNullifier::from(Fr::ONE),
            &mut TxNoteState::new(),
        ));

        let json = serde_json::to_string(&state).expect("PowState should serialize");
        let restored: PowState = serde_json::from_str(&json).expect("PowState should deserialize");

        assert_eq!(restored, state);
    }
}
