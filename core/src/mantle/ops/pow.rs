use std::num::NonZeroU64;

use ark_ff::Zero as _;
use lb_binary_codec::canonical::{BinaryCodec, BinaryEncode as _};
use lb_cryptarchia_engine::{EraSchedule, Slot};
use lb_groth16::{Fr, fr_from_mod_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use rpds::HashTrieMapSync;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    crypto::{Hash, ZkDigest as _, ZkHash, ZkHasher},
    events::{TxEvent, TxEventPayload},
    mantle::{
        Note, TxHash, Utxo, Value,
        gas::{Gas, MainnetGasProfile, OpGasCalculator, OperationGas},
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, Utxos,
            VerifiableOperation, verification_mode::StandardMode,
        },
        ops::{NoOpProof, OpId, SignedOperation},
        transactions::states::{Preverified, Unverified, Verified},
    },
};

/// `d_reward`: the difficulty threshold a puzzle ticket must be strictly
/// below to qualify for a `PoW` reward claim.
pub type PowTarget = Fr;
/// A `PoW` reward amount, denominated like any other note [`Value`].
pub type PowReward = Value;
/// The acceptance window, in slots, of the `PoW` reward claims of each era.
pub type PowSlotWindows = EraSchedule<NonZeroU64>;

/// Nullifier of a spent `PoW` solution, recorded on claim to prevent the same
/// solution from being claimed twice.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct PowNullifier(#[serde(with = "serde_fr")] ZkHash);

impl PowNullifier {
    #[must_use]
    pub const fn as_fr(&self) -> &Fr {
        &self.0
    }

    /// The puzzle ticket must be strictly below the current reward
    /// difficulty (§5.3: `puzzle_ticket < difficulty_reward`).
    pub fn validate_difficulty_reward(
        &self,
        reward_difficulty: &Fr,
    ) -> Result<(), ClaimPowRewardError> {
        let ticket_as_fr = self.as_fr();
        if ticket_as_fr >= reward_difficulty {
            return Err(ClaimPowRewardError::InvalidPoWRewardTicket);
        }
        Ok(())
    }
}

/// The ticket derived from a claim's inputs, checked against the reward
/// [`PowTarget`] and, once accepted, recorded as a [`PowNullifier`].
pub type PuzzleTicket = PowNullifier;

impl From<ZkHash> for PowNullifier {
    fn from(value: ZkHash) -> Self {
        Self(value)
    }
}

impl From<PowNullifier> for ZkHash {
    fn from(value: PowNullifier) -> Self {
        value.0
    }
}

/// Operation claiming the `PoW` reward for a solved puzzle.
///
/// The puzzle solution is not carried directly on the op: it is proven by
/// deriving a [`PuzzleTicket`] from `epoch_nonce`, `block_hash` and
/// `public_key` (see [`Self::get_puzzle_ticket`]) and checking that ticket
/// against the current reward difficulty during validation.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ClaimPowRewardOp {
    /// Epoch nonce the puzzle was solved against; must match the current or
    /// previous epoch nonce.
    #[serde(with = "serde_fr")]
    pub epoch_nonce: ZkHash,
    /// Hash of the block the puzzle solution is anchored to.
    pub block_hash: Hash,
    /// Public key of the reward beneficiary.
    pub public_key: ZkPublicKey,
}

impl ClaimPowRewardOp {
    /// Derive this claim's [`PuzzleTicket`] from `epoch_nonce`, `block_hash`
    /// and `public_key`.
    #[must_use]
    pub fn get_puzzle_ticket(&self) -> PuzzleTicket {
        PowNullifier(ZkHasher::digest(&[
            *self.public_key.as_fr(),
            fr_from_mod_bytes(&self.block_hash),
            self.epoch_nonce,
        ]))
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            epoch_nonce: ZkHash::from(Fr::from(35u64)),
            block_hash: Hash::from([36u8; 32]),
            public_key: ZkPublicKey::from(Fr::from(37u64)),
        }
    }
}

/// Errors returned while validating a [`ClaimPowRewardOp`].
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ClaimPowRewardError {
    #[error("Insufficient pool ({pool}) for reward ({reward})")]
    InsufficientPoolBalance { pool: PowReward, reward: PowReward },
    #[error("Current PoW rewards are Zero (`0`)")]
    EmptyRewards,
    #[error("Mismatch epoch nonce ({claim:?}), accepted {accepted:?}")]
    MismatchEpochNonce {
        claim: ZkHash,
        /// The (previous, current) epoch nonces a claim may match.
        accepted: (ZkHash, ZkHash),
    },
    #[error("Invalid PoW reward ticket")]
    InvalidPoWRewardTicket,
    #[error("Ticket was already claimed")]
    DoubleClaimed,
    #[error("Out of window slot ({slot:?}) vs block slot ({current_slot:?})")]
    OutOfWindowSlot { slot: Slot, current_slot: Slot },
    #[error("Missing block ({block_id:?})")]
    MissingBlock { block_id: Hash },
}

/// Ledger context needed to validate a [`ClaimPowRewardOp`].
pub struct ClaimPoWRewardVerificationContext<'a> {
    // As per spec
    /// Slot of the block the claim is being validated in.
    pub current_block_slot: Slot,
    /// `d_reward`: current reward difficulty a puzzle ticket must meet.
    pub reward_difficulty: PowTarget,
    /// Nullifiers of already-claimed `PoW` solutions, mapped to the slot
    /// they were claimed at.
    pub pow_nullifiers: &'a HashTrieMapSync<PowNullifier, Slot>,
    // needed not in spec yet
    /// `sigma_e`: reward amount per claim for the current epoch.
    pub epoch_pow_reward: PowReward,
    /// `R_PoW`: current balance of the `PoW` reward pool.
    pub epoch_reward_pool: PowReward,
    /// Randomness nonce of the current epoch (the same value
    /// proof-of-leadership uses), against which a claim's `epoch_nonce` is
    /// matched.
    pub current_epoch_nonce: ZkHash,
    /// Randomness nonce of the previous epoch, also accepted for claims mined
    /// just before an epoch boundary and claimed just after it.
    pub previous_epoch_nonce: ZkHash,
    /// Slots of known blocks, used to check the claim's block is within
    /// the acceptance window.
    pub blocks_slot: HashTrieMapSync<Hash, Slot>,
    /// Acceptance window, in slots, of each era: how far back a claim's
    /// anchor block may be from the current block. A claim is judged under
    /// the window of the era of its anchor block.
    pub slot_windows: PowSlotWindows,
}

impl ClaimPoWRewardVerificationContext<'_> {
    /// Claiming must be enabled for this block context (pool can cover the
    /// reward)
    fn are_pow_reward_enabled(&self) -> Result<(), ClaimPowRewardError> {
        if self.epoch_pow_reward.is_zero() {
            return Err(ClaimPowRewardError::EmptyRewards);
        }
        self.validate_enough_funds_in_pool()?;
        Ok(())
    }

    /// On-chain `block_hash` window check, measured in slots, against the
    /// acceptance window of the era of the anchor block.
    pub fn accept_claim(&self, block_id: Hash) -> Result<(), ClaimPowRewardError> {
        let Some(&block_slot) = self.blocks_slot.get(&block_id) else {
            return Err(ClaimPowRewardError::MissingBlock { block_id });
        };
        let slot_window = self.slot_window_at(block_slot);

        let Some(slot_gap) = self.current_block_slot.checked_sub(block_slot) else {
            return Err(ClaimPowRewardError::OutOfWindowSlot {
                slot: block_slot,
                current_slot: self.current_block_slot,
            });
        };
        if slot_gap > Slot::from(slot_window.get()) {
            return Err(ClaimPowRewardError::OutOfWindowSlot {
                slot: block_slot,
                current_slot: self.current_block_slot,
            });
        }
        Ok(())
    }

    /// The acceptance window of a claim anchored to a block at
    /// `block_slot`: that of the era of the block.
    fn slot_window_at(&self, block_slot: Slot) -> NonZeroU64 {
        *self.slot_windows.at_slot(block_slot).parameters
    }

    /// The claim's epoch nonce must be the current or the previous one.
    fn validate_epoch_nonce(&self, claim_epoch_nonce: ZkHash) -> Result<(), ClaimPowRewardError> {
        if claim_epoch_nonce != self.current_epoch_nonce
            && claim_epoch_nonce != self.previous_epoch_nonce
        {
            return Err(ClaimPowRewardError::MismatchEpochNonce {
                claim: claim_epoch_nonce,
                accepted: (self.previous_epoch_nonce, self.current_epoch_nonce),
            });
        }
        Ok(())
    }

    /// The puzzle ticket must be strictly below the current reward
    /// difficulty (§5.3: `puzzle_ticket < difficulty_reward`).
    fn validate_difficulty_reward(
        &self,
        puzzle_ticket: PuzzleTicket,
    ) -> Result<(), ClaimPowRewardError> {
        puzzle_ticket.validate_difficulty_reward(&self.reward_difficulty)
    }

    /// The puzzle ticket must not already have been claimed.
    fn validate_double_claiming(
        &self,
        puzzle_ticket: PuzzleTicket,
    ) -> Result<(), ClaimPowRewardError> {
        if self.pow_nullifiers.contains_key(&puzzle_ticket) {
            return Err(ClaimPowRewardError::DoubleClaimed);
        }
        Ok(())
    }

    /// Validate enough funds available
    const fn validate_enough_funds_in_pool(&self) -> Result<(), ClaimPowRewardError> {
        if self.epoch_reward_pool < self.epoch_pow_reward {
            return Err(ClaimPowRewardError::InsufficientPoolBalance {
                pool: self.epoch_reward_pool,
                reward: self.epoch_pow_reward,
            });
        }
        Ok(())
    }
}

impl OpId for ClaimPowRewardOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

/// Ledger context needed to execute a [`ClaimPowRewardOp`], and the outcome
/// carried back out to update the ledger's `PoW` state.
pub struct ClaimPoWRewardExecutionContext {
    /// `R_PoW`: current balance of the `PoW` reward pool.
    pub reward_pool: PowReward,
    /// `sigma_e`: reward amount paid out by this claim.
    pub epoch_reward: PowReward,
    /// Nullifiers of already-claimed `PoW` solutions.
    pub nullifiers: HashTrieMapSync<PowNullifier, Slot>,
    /// Hash of the transaction carrying this claim.
    pub tx_hash: TxHash,
    /// Unspent transaction outputs, extended with the reward note.
    pub utxos: Utxos,
    /// Recorded block slots
    pub block_slots: HashTrieMapSync<Hash, Slot>,
}

impl ClaimPoWRewardExecutionContext {
    /// Deduct the paid-out `epoch_reward` from the `reward_pool`.
    /// This should always pass if the verification does it job, but
    /// double-checking is no problem
    const fn decrement_reward_pool(&mut self) {
        self.reward_pool = self.reward_pool.checked_sub(self.epoch_reward).expect(
            "Pool funding is check in validation so this computation should always be valid",
        );
    }
}

impl ProvableOperation for ClaimPowRewardOp {
    type Proof = NoOpProof;
    const CODE: u8 = 0x40;
}

impl OperationGas<MainnetGasProfile> for ClaimPowRewardOp {
    const GAS_COST: Gas = Gas::new(1);
}

impl OpGasCalculator<MainnetGasProfile> for ClaimPowRewardOp {}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<ClaimPowRewardOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = ClaimPowRewardError;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<ClaimPowRewardOp, Preverified, StandardMode>
{
    type DeferredProof = ();
    type Context<'a> = ClaimPoWRewardVerificationContext<'a>;
    type Error = ClaimPowRewardError;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        let operation = self.operation();

        context.are_pow_reward_enabled()?;
        context.validate_epoch_nonce(operation.epoch_nonce)?;
        context.accept_claim(operation.block_hash)?;
        let puzzle_ticket = operation.get_puzzle_ticket();
        context.validate_difficulty_reward(puzzle_ticket)?;
        context.validate_double_claiming(puzzle_ticket)?;
        Ok(())
    }
}

impl ExecutableOperation for SignedOperation<ClaimPowRewardOp, Verified, StandardMode> {
    type Context<'a> = ClaimPoWRewardExecutionContext;
    type Error = ClaimPowRewardError;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        let slot = context
            .block_slots
            .get(&operation.block_hash)
            .expect("Existence should be check in verification");
        // add the nullifier to the set
        let nullifier = operation.get_puzzle_ticket();
        context.nullifiers.insert_mut(nullifier, *slot);
        // create output note
        let note = Note::new(context.epoch_reward, operation.public_key);
        let op_id = operation.op_id();
        let utxo = Utxo {
            op_id,
            output_index: 0,
            note,
        };
        context.utxos = context.utxos.insert(utxo.id(), utxo).0;
        // decrement current pool
        context.decrement_reward_pool();
        // output event
        let tx_hash = context.tx_hash;
        Ok((
            context,
            vec![TxEvent::new(
                tx_hash,
                op_id,
                TxEventPayload::PoWRewardClaimed {
                    pow_nullifier: nullifier,
                    utxo,
                },
            )],
        ))
    }
}

#[cfg(test)]
pub(crate) mod test_utils {
    use std::{iter, num::NonZero, time::Duration};

    use lb_time::era::{EraEntriesAfterGenesis, EraTiming};
    use time::OffsetDateTime;

    use super::{EraSchedule, PowSlotWindows};

    /// Acceptance windows over epochs of 10 slots: `genesis` from epoch 0,
    /// then each `(first_epoch, window)` of `after_genesis`.
    pub fn slot_windows<const AFTER_GENESIS: usize>(
        genesis: u64,
        after_genesis: [(u32, u64); AFTER_GENESIS],
    ) -> PowSlotWindows {
        let timing = EraTiming {
            slot_duration: Duration::from_secs(1),
            epoch_length: NonZero::new(10).expect("10 is not 0"),
        };
        let windows: Vec<_> = iter::once(genesis)
            .chain(after_genesis.iter().map(|&(_, window)| window))
            .collect();
        let after_genesis = EraEntriesAfterGenesis::try_from_iter(
            after_genesis.map(|(first_epoch, _)| (NonZero::new(first_epoch).unwrap(), timing)),
        )
        .unwrap();
        EraSchedule::new(OffsetDateTime::UNIX_EPOCH, timing, after_genesis)
            .unwrap()
            .map(|era| NonZero::new(windows[usize::from(era.era.into_inner())]).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use std::iter;

    use lb_groth16::{AdditiveGroup as _, Field as _};

    use super::{test_utils::slot_windows, *};
    use crate::mantle::gas::test_utils::FixedThresholds;

    pub const SLOT_WINDOW: NonZeroU64 = NonZeroU64::new(100).expect("100 is not 0");
    /// The window of the single era of [`accepting_context`].
    const ACCEPTING_SLOT_WINDOW: NonZeroU64 = NonZeroU64::new(10).expect("10 is not 0");

    fn validation_context(
        nullifiers: &HashTrieMapSync<PowNullifier, Slot>,
        epoch_pow_reward: PowReward,
        epoch_reward_pool: PowReward,
    ) -> ClaimPoWRewardVerificationContext<'_> {
        ClaimPoWRewardVerificationContext {
            current_block_slot: Slot::from(0u64),
            reward_difficulty: PowTarget::default(),
            pow_nullifiers: nullifiers,
            epoch_pow_reward,
            epoch_reward_pool,
            current_epoch_nonce: nonce_for_epoch(0),
            previous_epoch_nonce: nonce_for_epoch(0),
            blocks_slot: HashTrieMapSync::new_sync(),
            slot_windows: slot_windows(SLOT_WINDOW.get(), []),
        }
    }

    #[test]
    fn pow_reward_enabled_accepts_pool_exactly_covering_the_reward() {
        // Spec §5.6: claiming is enabled when `pow_reward_pool >= sigma_e`.
        // A pool exactly equal to the reward must be claimable; rejecting it
        // (as the previous `<=` comparison did) strands the last reward.
        let nullifiers = HashTrieMapSync::new_sync();
        let ctx = validation_context(&nullifiers, 10, 10);
        assert_eq!(ctx.are_pow_reward_enabled(), Ok(()));
    }

    #[test]
    fn pow_reward_enabled_rejects_pool_below_the_reward() {
        let nullifiers = HashTrieMapSync::new_sync();
        let ctx = validation_context(&nullifiers, 10, 9);
        assert_eq!(
            ctx.are_pow_reward_enabled(),
            Err(ClaimPowRewardError::InsufficientPoolBalance {
                pool: 9,
                reward: 10,
            })
        );
    }

    #[test]
    fn pow_reward_enabled_rejects_zero_reward() {
        // sigma_e == 0 is the safety cutoff: claims are rejected outright,
        // regardless of the pool balance.
        let nullifiers = HashTrieMapSync::new_sync();
        let ctx = validation_context(&nullifiers, 0, 1_000);
        assert_eq!(
            ctx.are_pow_reward_enabled(),
            Err(ClaimPowRewardError::EmptyRewards)
        );
    }

    const CURRENT_EPOCH: u32 = 5;
    const PREVIOUS_EPOCH: u32 = 4;
    const CLAIM_BLOCK_HASH: Hash = [1u8; 32];

    /// A distinct, deterministic stand-in epoch nonce, so tests can build
    /// claims and contexts for different epochs without a real ledger.
    fn nonce_for_epoch(epoch: u32) -> ZkHash {
        ZkHasher::digest(&[fr_from_mod_bytes(&epoch.to_le_bytes())])
    }

    fn claim_op(epoch: u32) -> ClaimPowRewardOp {
        ClaimPowRewardOp {
            epoch_nonce: nonce_for_epoch(epoch),
            block_hash: CLAIM_BLOCK_HASH,
            public_key: ZkPublicKey::new(Fr::from(42u64)),
        }
    }

    /// A context that accepts `claim_op(CURRENT_EPOCH)`: funded pool,
    /// permissive difficulty, claim block a few slots back.
    fn accepting_context(
        nullifiers: &HashTrieMapSync<PowNullifier, Slot>,
    ) -> ClaimPoWRewardVerificationContext<'_> {
        ClaimPoWRewardVerificationContext {
            current_block_slot: Slot::from(50u64),
            // p - 1, the largest field element: every ticket passes.
            reward_difficulty: -Fr::ONE,
            pow_nullifiers: nullifiers,
            epoch_pow_reward: 10,
            epoch_reward_pool: 1_000,
            current_epoch_nonce: nonce_for_epoch(CURRENT_EPOCH),
            previous_epoch_nonce: nonce_for_epoch(PREVIOUS_EPOCH),
            blocks_slot: iter::once((CLAIM_BLOCK_HASH, Slot::from(45u64))).collect(),
            slot_windows: slot_windows(ACCEPTING_SLOT_WINDOW.get(), []),
        }
    }

    #[test]
    fn puzzle_ticket_is_deterministic_and_binds_every_field() {
        let op = claim_op(CURRENT_EPOCH);
        assert_eq!(
            op.get_puzzle_ticket(),
            claim_op(CURRENT_EPOCH).get_puzzle_ticket()
        );

        // Spec §3: the ticket commits to all three fields, so changing the
        // epoch nonce (cross-epoch replay), the anchored block, or the
        // beneficiary key each invalidates the solution.
        let mut other_epoch = op.clone();
        other_epoch.epoch_nonce = nonce_for_epoch(CURRENT_EPOCH + 1);
        assert_ne!(op.get_puzzle_ticket(), other_epoch.get_puzzle_ticket());

        let mut other_block = op.clone();
        other_block.block_hash = [2u8; 32];
        assert_ne!(op.get_puzzle_ticket(), other_block.get_puzzle_ticket());

        let mut other_key = op.clone();
        other_key.public_key = ZkPublicKey::new(Fr::from(43u64));
        assert_ne!(op.get_puzzle_ticket(), other_key.get_puzzle_ticket());
    }

    #[test]
    fn accept_claim_accepts_blocks_inside_the_window() {
        let nullifiers = HashTrieMapSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);
        let current_slot = 200u64;
        ctx.current_block_slot = Slot::from(current_slot);

        // Gap of zero: the claim's block is the current block.
        let slot_window = ACCEPTING_SLOT_WINDOW;
        ctx.blocks_slot
            .insert_mut(CLAIM_BLOCK_HASH, Slot::from(current_slot));
        assert_eq!(ctx.accept_claim(CLAIM_BLOCK_HASH), Ok(()));

        // Gap exactly equal to the window is still inside it (§5.1.1:
        // `0 <= current - anchor <= WINDOW`, measured in slots).
        ctx.blocks_slot.insert_mut(
            CLAIM_BLOCK_HASH,
            Slot::from(current_slot - slot_window.get()),
        );
        assert_eq!(ctx.accept_claim(CLAIM_BLOCK_HASH), Ok(()));
    }

    #[test]
    fn accept_claim_rejects_unknown_block() {
        let nullifiers = HashTrieMapSync::new_sync();
        let ctx = accepting_context(&nullifiers);
        let unknown = [9u8; 32];
        assert_eq!(
            ctx.accept_claim(unknown),
            Err(ClaimPowRewardError::MissingBlock { block_id: unknown })
        );
    }

    #[test]
    fn accept_claim_rejects_block_beyond_the_window() {
        let nullifiers = HashTrieMapSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);
        let current_slot = 200u64;
        ctx.current_block_slot = Slot::from(current_slot);
        // Gap of WINDOW + 1: one slot too old.
        let anchor = Slot::from(current_slot - ACCEPTING_SLOT_WINDOW.get() - 1);
        ctx.blocks_slot.insert_mut(CLAIM_BLOCK_HASH, anchor);
        assert_eq!(
            ctx.accept_claim(CLAIM_BLOCK_HASH),
            Err(ClaimPowRewardError::OutOfWindowSlot {
                slot: anchor,
                current_slot: Slot::from(current_slot),
            })
        );
    }

    #[test]
    fn accept_claim_rejects_block_from_the_future() {
        let nullifiers = HashTrieMapSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);
        // The claim's block is ahead of the current slot (negative gap),
        // e.g. a hash from a competing, longer branch.
        ctx.blocks_slot
            .insert_mut(CLAIM_BLOCK_HASH, Slot::from(51u64));
        assert_eq!(
            ctx.accept_claim(CLAIM_BLOCK_HASH),
            Err(ClaimPowRewardError::OutOfWindowSlot {
                slot: Slot::from(51u64),
                current_slot: Slot::from(50),
            })
        );
    }

    #[test]
    fn preverify_accepts_every_claim() {
        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(claim_op(CURRENT_EPOCH), NoOpProof);

        assert_eq!(signed_operation.preverify(&()), Ok(()));
    }

    #[test]
    fn verify_accepts_a_claim_with_the_current_epoch_nonce() {
        let nullifiers = HashTrieMapSync::new_sync();
        let ctx = accepting_context(&nullifiers);
        let op = claim_op(CURRENT_EPOCH);
        let signed_operation = SignedOperation::new(op, NoOpProof)
            .into_preverified(&())
            .unwrap();
        assert!(signed_operation.verify(&ctx).is_ok());
    }

    #[test]
    fn verify_accepts_a_claim_with_the_previous_epoch_nonce() {
        // Spec §5.3 step 3: a solution mined just before an epoch boundary
        // stays claimable, so the previous epoch's nonce is also accepted.
        let nullifiers = HashTrieMapSync::new_sync();
        let ctx = accepting_context(&nullifiers);
        let op = claim_op(PREVIOUS_EPOCH);
        let signed_operation = SignedOperation::new(op, NoOpProof)
            .into_preverified(&())
            .unwrap();

        assert!(signed_operation.verify(&ctx).is_ok());
    }

    #[test]
    fn verify_judges_a_claim_under_the_window_of_the_era_of_its_anchor_block() {
        let nullifiers = HashTrieMapSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);
        // Era 0 has a window of 20, era 1, from slot 40 on, one of 10. The
        // claim's block, in era 0, is 15 slots back from the current block,
        // in era 1: inside the window of the anchor's era, outside the
        // current one's.
        ctx.slot_windows = slot_windows(20, [(4, 10)]);
        ctx.blocks_slot
            .insert_mut(CLAIM_BLOCK_HASH, Slot::from(35u64));
        let verify = |epoch| {
            SignedOperation::new(claim_op(epoch), NoOpProof)
                .into_preverified(&())
                .unwrap()
                .verify(&ctx)
        };

        // Whichever accepted nonce the claim carries.
        assert_eq!(verify(CURRENT_EPOCH), Ok(()));
        assert_eq!(verify(PREVIOUS_EPOCH), Ok(()));
    }

    #[test]
    fn verify_rejects_a_claim_beyond_the_window_of_the_era_of_its_anchor_block() {
        let nullifiers = HashTrieMapSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);
        // Era 0 has a window of 10, era 1, from slot 40 on, one of 20. The
        // claim's block, in era 0, is 15 slots back: outside the window of
        // the anchor's era, although inside the current one's.
        ctx.slot_windows = slot_windows(10, [(4, 20)]);
        ctx.blocks_slot
            .insert_mut(CLAIM_BLOCK_HASH, Slot::from(35u64));
        let verify = |epoch| {
            SignedOperation::new(claim_op(epoch), NoOpProof)
                .into_preverified(&())
                .unwrap()
                .verify(&ctx)
        };
        let out_of_window = Err(ClaimPowRewardError::OutOfWindowSlot {
            slot: Slot::from(35u64),
            current_slot: Slot::from(50u64),
        });

        assert_eq!(verify(CURRENT_EPOCH), out_of_window);
        assert_eq!(verify(PREVIOUS_EPOCH), out_of_window);
    }

    #[test]
    fn verify_rejects_a_claim_while_the_pool_cannot_cover_the_reward() {
        let nullifiers = HashTrieMapSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);
        ctx.epoch_reward_pool = ctx.epoch_pow_reward - 1;
        let signed_operation = SignedOperation::new(claim_op(CURRENT_EPOCH), NoOpProof)
            .into_preverified(&())
            .unwrap();
        assert_eq!(
            signed_operation.verify(&ctx).unwrap_err(),
            ClaimPowRewardError::InsufficientPoolBalance {
                pool: 9,
                reward: 10,
            }
        );
    }

    #[test]
    fn verify_rejects_a_claim_anchored_to_an_unknown_block() {
        let nullifiers = HashTrieMapSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);
        ctx.blocks_slot = HashTrieMapSync::new_sync();
        let signed_operation = SignedOperation::new(claim_op(CURRENT_EPOCH), NoOpProof)
            .into_preverified(&())
            .unwrap();
        assert_eq!(
            signed_operation.verify(&ctx).unwrap_err(),
            ClaimPowRewardError::MissingBlock {
                block_id: CLAIM_BLOCK_HASH,
            }
        );
    }

    #[test]
    fn verify_rejects_a_claim_with_a_stale_epoch_nonce() {
        let nullifiers = HashTrieMapSync::new_sync();
        let ctx = accepting_context(&nullifiers);
        let op = claim_op(PREVIOUS_EPOCH - 1);
        let signed_operation = SignedOperation::new(op, NoOpProof)
            .into_preverified(&())
            .unwrap();
        assert_eq!(
            signed_operation.verify(&ctx).err().unwrap(),
            ClaimPowRewardError::MismatchEpochNonce {
                claim: signed_operation.operation().epoch_nonce,
                accepted: (
                    nonce_for_epoch(PREVIOUS_EPOCH),
                    nonce_for_epoch(CURRENT_EPOCH)
                ),
            }
        );
    }

    #[test]
    fn verify_rejects_a_ticket_above_the_reward_difficulty() {
        let nullifiers = HashTrieMapSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);
        // The hardest possible target: only a ticket of exactly zero would
        // pass, and this op's ticket is not zero.
        ctx.reward_difficulty = Fr::ZERO;
        let op = claim_op(CURRENT_EPOCH);
        let signed_operation = SignedOperation::new(op, NoOpProof)
            .into_preverified(&())
            .unwrap();
        assert_eq!(
            signed_operation.verify(&ctx).err().unwrap(),
            ClaimPowRewardError::InvalidPoWRewardTicket
        );
    }

    #[test]
    fn verify_rejects_a_ticket_equal_to_the_reward_difficulty() {
        // Spec §5.3: the check is the strict `puzzle_ticket <
        // difficulty_reward`, so a ticket exactly on the target does not
        // qualify.
        let nullifiers = HashTrieMapSync::new_sync();
        let op = claim_op(CURRENT_EPOCH);
        let mut ctx = accepting_context(&nullifiers);
        ctx.reward_difficulty = op.get_puzzle_ticket().into();
        let signed_operation = SignedOperation::new(op, NoOpProof)
            .into_preverified(&())
            .unwrap();
        assert_eq!(
            signed_operation.verify(&ctx).err().unwrap(),
            ClaimPowRewardError::InvalidPoWRewardTicket
        );
    }

    #[test]
    fn verify_rejects_an_already_claimed_ticket() {
        let op = claim_op(CURRENT_EPOCH);
        let nullifiers =
            HashTrieMapSync::new_sync().insert(op.get_puzzle_ticket(), Slot::from(45u64));
        let ctx = accepting_context(&nullifiers);
        let signed_operation = SignedOperation::new(op, NoOpProof)
            .into_preverified(&())
            .unwrap();
        assert_eq!(
            signed_operation.verify(&ctx).err().unwrap(),
            ClaimPowRewardError::DoubleClaimed
        );
    }

    #[test]
    fn execute_issues_reward_utxo_and_registers_nullifier() {
        let op = claim_op(CURRENT_EPOCH);
        let signed_operation = SignedOperation::new(op, NoOpProof)
            .into_preverified(&())
            .unwrap()
            .into_verified(&accepting_context(&HashTrieMapSync::new_sync()))
            .unwrap();
        let puzzle_ticket = signed_operation.operation().get_puzzle_ticket();
        let operation_op_id = signed_operation.operation().op_id();
        let operation_public_key = signed_operation.operation().public_key;
        let epoch_reward = 10;
        let tx_hash = TxHash::from([11u8; 32]);

        let (ctx, events) = signed_operation
            .execute(ClaimPoWRewardExecutionContext {
                reward_pool: 1_000,
                epoch_reward,
                nullifiers: HashTrieMapSync::new_sync(),
                tx_hash,
                utxos: Utxos::new(),
                block_slots: iter::once((CLAIM_BLOCK_HASH, Slot::from(45u64))).collect(),
            })
            .expect("claim execution should succeed");

        // The spent solution is recorded against the anchor block's slot,
        // and the pool pays out sigma_e.
        assert_eq!(ctx.nullifiers.get(&puzzle_ticket), Some(&Slot::from(45u64)));
        assert_eq!(ctx.reward_pool, 990);

        // The reward note lands in the UTXO set, payable to the op's key
        // (§5.3 execution step 3). Regression: the persistent-tree insert
        // result used to be discarded, so the note never reached the set.
        let expected_utxo = Utxo {
            op_id: operation_op_id,
            output_index: 0,
            note: Note::new(epoch_reward, operation_public_key),
        };
        assert_eq!(ctx.utxos.get(&expected_utxo.id()), Some(expected_utxo));

        let mut events = events.iter();
        let Some(TxEvent {
            tx_hash: event_tx_hash,
            op_id,
            payload:
                TxEventPayload::PoWRewardClaimed {
                    pow_nullifier,
                    utxo,
                },
        }) = events.next()
        else {
            panic!("expected PoWRewardClaimed tx event");
        };
        assert_eq!(*event_tx_hash, tx_hash);
        assert_eq!(*op_id, operation_op_id);
        assert_eq!(*pow_nullifier, puzzle_ticket);
        assert_eq!(*utxo, expected_utxo);
        assert!(events.next().is_none());
    }

    #[test]
    #[should_panic(expected = "Pool funding is check in validation")]
    fn execute_panics_when_pool_cannot_cover_the_reward() {
        // Verification (`validate_enough_funds_in_pool`) is the guard that
        // keeps uncoverable claims out of blocks — a builder must never
        // include one. Execution treats a shortfall as a broken invariant
        // and aborts loudly rather than minting a reward note the pool
        // cannot back.
        let op = claim_op(CURRENT_EPOCH);
        let signed_operation = SignedOperation::new(op, NoOpProof)
            .into_preverified(&())
            .unwrap()
            .into_verified(&accepting_context(&HashTrieMapSync::new_sync()))
            .unwrap();

        drop(signed_operation.execute(ClaimPoWRewardExecutionContext {
            reward_pool: 5,
            epoch_reward: 10,
            nullifiers: HashTrieMapSync::new_sync(),
            tx_hash: TxHash::from([11u8; 32]),
            utxos: Utxos::new(),
            block_slots: iter::once((CLAIM_BLOCK_HASH, Slot::from(45u64))).collect(),
        }));
    }

    #[test]
    fn claim_pow_reward_op_execution_gas_does_not_scale_with_the_threshold() {
        for threshold in [0, 1, 3] {
            assert_eq!(
                ClaimPowRewardOp::sample().execution_gas(&FixedThresholds(threshold)),
                Ok(Gas::new(1))
            );
        }
    }
}
