use std::collections::HashMap;

use ark_ff::Zero as _;
use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_groth16::{Fr, fr_from_mod_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    crypto::{Hash, ZkDigest as _, ZkHash, ZkHasher},
    events::{TxEvent, TxEventPayload},
    mantle::{
        Note, TxHash, Utxo, Value,
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, Utxos,
            VerifiableOperation, verification_mode,
        },
        ops::{NoOpProof, OpId},
    },
};

pub const SLOT_WINDOW: u64 = 100;
/// `d_reward`: the difficulty threshold a puzzle ticket must be strictly
/// below to qualify for a `PoW` reward claim.
pub type PowTarget = Fr;
/// A `PoW` reward amount, denominated like any other note [`Value`].
pub type PowReward = Value;

/// Nullifier of a spent `PoW` solution, recorded on claim to prevent the same
/// solution from being claimed twice.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct PowNullifier(#[serde(with = "serde_fr")] ZkHash);

impl PowNullifier {
    #[must_use]
    pub const fn as_fr(&self) -> &Fr {
        &self.0
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
            self.epoch_nonce,
            fr_from_mod_bytes(&self.block_hash),
            *self.public_key.as_fr(),
        ]))
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
        accepted: (Epoch, Epoch),
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
    /// Nullifiers of already-claimed `PoW` solutions.
    pub pow_nullifiers: &'a rpds::HashTrieSetSync<PowNullifier>,
    // needed not in spec yet
    /// `sigma_e`: reward amount per claim for the current epoch.
    pub epoch_pow_reward: PowReward,
    /// `R_PoW`: current balance of the `PoW` reward pool.
    pub epoch_reward_pool: PowReward,
    /// Nonce of the current epoch.
    pub current_epoch_nonce: Epoch,
    /// Nonce of the previous epoch, also accepted for claims.
    pub previous_epoch_nonce: Epoch,
    /// Slots of known blocks, used to check the claim's block is within
    /// the acceptance window.
    pub blocks_slot: HashMap<Hash, Slot>,
}

impl ClaimPoWRewardVerificationContext<'_> {
    /// Claiming must be enabled for this block context (pool can cover the
    /// reward)
    fn are_pow_reward_enabled(&self) -> Result<(), ClaimPowRewardError> {
        if self.epoch_pow_reward.is_zero() {
            return Err(ClaimPowRewardError::EmptyRewards);
        }
        if self.epoch_reward_pool < self.epoch_pow_reward {
            return Err(ClaimPowRewardError::InsufficientPoolBalance {
                pool: self.epoch_reward_pool,
                reward: self.epoch_pow_reward,
            });
        }
        Ok(())
    }

    /// On-chain `block_hash` window check, measured in slots.
    pub fn accept_claim<const WINDOW: u64>(
        &self,
        block_id: Hash,
    ) -> Result<(), ClaimPowRewardError> {
        let Some(&block_slot) = self.blocks_slot.get(&block_id) else {
            return Err(ClaimPowRewardError::MissingBlock { block_id });
        };

        let Some(slot_gap) = self.current_block_slot.checked_sub(block_slot) else {
            return Err(ClaimPowRewardError::OutOfWindowSlot {
                slot: block_slot,
                current_slot: self.current_block_slot,
            });
        };
        if slot_gap > Slot::from(WINDOW) {
            return Err(ClaimPowRewardError::OutOfWindowSlot {
                slot: block_slot,
                current_slot: self.current_block_slot,
            });
        }
        Ok(())
    }

    /// Epoch nonce must match the current epoch or the previous epoch nonce
    fn validate_current_epoch_nonce(
        &self,
        claim_epoch_nonce: ZkHash,
    ) -> Result<(), ClaimPowRewardError> {
        let previous_epoch_nonce = ZkHasher::digest(&[fr_from_mod_bytes(
            &self.previous_epoch_nonce.into_inner().to_le_bytes(),
        )]);
        if claim_epoch_nonce == previous_epoch_nonce {
            return Ok(());
        }

        let current_epoch_nonce = ZkHasher::digest(&[fr_from_mod_bytes(
            &self.current_epoch_nonce.into_inner().to_le_bytes(),
        )]);
        if claim_epoch_nonce == current_epoch_nonce {
            return Ok(());
        }

        Err(ClaimPowRewardError::MismatchEpochNonce {
            claim: claim_epoch_nonce,
            accepted: (self.previous_epoch_nonce, self.current_epoch_nonce),
        })
    }

    /// The puzzle ticket must be strictly below the current reward
    /// difficulty (§5.3: `puzzle_ticket < difficulty_reward`).
    fn validate_difficulty_reward(
        &self,
        puzzle_ticket: PuzzleTicket,
    ) -> Result<(), ClaimPowRewardError> {
        let ticket_as_fr = *puzzle_ticket.as_fr();
        if ticket_as_fr >= self.reward_difficulty {
            return Err(ClaimPowRewardError::InvalidPoWRewardTicket);
        }
        Ok(())
    }

    /// The puzzle ticket must not already have been claimed.
    fn validate_double_claiming(
        &self,
        puzzle_ticket: PuzzleTicket,
    ) -> Result<(), ClaimPowRewardError> {
        if self.pow_nullifiers.contains(&puzzle_ticket) {
            return Err(ClaimPowRewardError::DoubleClaimed);
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
    pub nullifiers: rpds::HashTrieSetSync<PowNullifier>,
    /// Hash of the transaction carrying this claim.
    pub tx_hash: TxHash,
    /// Unspent transaction outputs, extended with the reward note.
    pub utxos: Utxos,
}

impl ClaimPoWRewardExecutionContext {
    /// Deduct the paid-out `epoch_reward` from the `reward_pool`.
    const fn decrement_reward_pool(&mut self) {
        self.reward_pool = self.reward_pool.saturating_sub(self.epoch_reward);
    }
}

impl ProvableOperation for ClaimPowRewardOp {
    type Proof = NoOpProof;
}

impl PreverifiableOperation<verification_mode::StandardMode> for ClaimPowRewardOp {
    type Context<'a> = ();
    type Error = ClaimPowRewardError;

    fn preverify(
        &self,
        _proof: &Self::Proof,
        _context: &Self::Context<'_>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl VerifiableOperation<verification_mode::StandardMode> for ClaimPowRewardOp {
    type Context<'a> = ClaimPoWRewardVerificationContext<'a>;
    type Error = ClaimPowRewardError;

    fn verify(&self, _proof: &Self::Proof, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        context.are_pow_reward_enabled()?;
        // TODO Plug constant window
        context.accept_claim::<{ SLOT_WINDOW }>(self.block_hash)?;
        context.validate_current_epoch_nonce(self.epoch_nonce)?;
        let puzzle_ticket = self.get_puzzle_ticket();
        context.validate_difficulty_reward(puzzle_ticket)?;
        context.validate_double_claiming(puzzle_ticket)?;
        Ok(())
    }
}

impl ExecutableOperation for ClaimPowRewardOp {
    type Context<'a> = ClaimPoWRewardExecutionContext;
    type Error = ClaimPowRewardError;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        // add the nullifier to the set
        let nullifier = self.get_puzzle_ticket();
        context.nullifiers.insert_mut(nullifier);
        // create output note
        let note = Note::new(context.epoch_reward, self.public_key);
        let op_id = self.op_id();
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
mod tests {
    use lb_groth16::{AdditiveGroup as _, Field as _};

    use super::*;

    fn validation_context(
        nullifiers: &rpds::HashTrieSetSync<PowNullifier>,
        epoch_pow_reward: PowReward,
        epoch_reward_pool: PowReward,
    ) -> ClaimPoWRewardVerificationContext<'_> {
        ClaimPoWRewardVerificationContext {
            current_block_slot: Slot::from(0u64),
            reward_difficulty: PowTarget::default(),
            pow_nullifiers: nullifiers,
            epoch_pow_reward,
            epoch_reward_pool,
            current_epoch_nonce: 0.into(),
            previous_epoch_nonce: 0.into(),
            blocks_slot: HashMap::new(),
        }
    }

    #[test]
    fn pow_reward_enabled_accepts_pool_exactly_covering_the_reward() {
        // Spec §5.6: claiming is enabled when `pow_reward_pool >= sigma_e`.
        // A pool exactly equal to the reward must be claimable; rejecting it
        // (as the previous `<=` comparison did) strands the last reward.
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let ctx = validation_context(&nullifiers, 10, 10);
        assert_eq!(ctx.are_pow_reward_enabled(), Ok(()));
    }

    #[test]
    fn pow_reward_enabled_rejects_pool_below_the_reward() {
        let nullifiers = rpds::HashTrieSetSync::new_sync();
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
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let ctx = validation_context(&nullifiers, 0, 1_000);
        assert_eq!(
            ctx.are_pow_reward_enabled(),
            Err(ClaimPowRewardError::EmptyRewards)
        );
    }

    const CURRENT_EPOCH: u32 = 5;
    const PREVIOUS_EPOCH: u32 = 4;
    const CLAIM_BLOCK_HASH: Hash = [1u8; 32];

    /// The epoch nonce `validate_current_epoch_nonce` derives for an epoch.
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
        nullifiers: &rpds::HashTrieSetSync<PowNullifier>,
    ) -> ClaimPoWRewardVerificationContext<'_> {
        ClaimPoWRewardVerificationContext {
            current_block_slot: Slot::from(50u64),
            // p - 1, the largest field element: every ticket passes.
            reward_difficulty: -Fr::ONE,
            pow_nullifiers: nullifiers,
            epoch_pow_reward: 10,
            epoch_reward_pool: 1_000,
            current_epoch_nonce: CURRENT_EPOCH.into(),
            previous_epoch_nonce: PREVIOUS_EPOCH.into(),
            blocks_slot: HashMap::from([(CLAIM_BLOCK_HASH, Slot::from(45u64))]),
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
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);

        // Gap of zero: the claim's block is the current block.
        ctx.blocks_slot.insert(CLAIM_BLOCK_HASH, Slot::from(50u64));
        assert_eq!(ctx.accept_claim::<10>(CLAIM_BLOCK_HASH), Ok(()));

        // Gap exactly equal to the window is still inside it (§5.1.1:
        // `0 <= current - anchor <= WINDOW`, measured in slots).
        ctx.blocks_slot.insert(CLAIM_BLOCK_HASH, Slot::from(40u64));
        assert_eq!(ctx.accept_claim::<10>(CLAIM_BLOCK_HASH), Ok(()));
    }

    #[test]
    fn accept_claim_rejects_unknown_block() {
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let ctx = accepting_context(&nullifiers);
        let unknown = [9u8; 32];
        assert_eq!(
            ctx.accept_claim::<10>(unknown),
            Err(ClaimPowRewardError::MissingBlock { block_id: unknown })
        );
    }

    #[test]
    fn accept_claim_rejects_block_beyond_the_window() {
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);
        // Gap of WINDOW + 1: one slot too old.
        ctx.blocks_slot.insert(CLAIM_BLOCK_HASH, Slot::from(39u64));
        assert_eq!(
            ctx.accept_claim::<10>(CLAIM_BLOCK_HASH),
            Err(ClaimPowRewardError::OutOfWindowSlot {
                slot: Slot::from(39u64),
                current_slot: Slot::from(50),
            })
        );
    }

    #[test]
    fn accept_claim_rejects_block_from_the_future() {
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);
        // The claim's block is ahead of the current slot (negative gap),
        // e.g. a hash from a competing, longer branch.
        ctx.blocks_slot.insert(CLAIM_BLOCK_HASH, Slot::from(51u64));
        assert_eq!(
            ctx.accept_claim::<10>(CLAIM_BLOCK_HASH),
            Err(ClaimPowRewardError::OutOfWindowSlot {
                slot: Slot::from(51u64),
                current_slot: Slot::from(50),
            })
        );
    }

    #[test]
    fn validate_accepts_claim_with_current_epoch_nonce() {
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let ctx = accepting_context(&nullifiers);
        assert_eq!(claim_op(CURRENT_EPOCH).verify(&NoOpProof, &ctx), Ok(()));
    }

    #[test]
    fn validate_accepts_claim_with_previous_epoch_nonce() {
        // Spec §5.3 step 3: a solution mined just before an epoch boundary
        // stays claimable, so the previous epoch's nonce is also accepted.
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let ctx = accepting_context(&nullifiers);
        assert_eq!(claim_op(PREVIOUS_EPOCH).verify(&NoOpProof, &ctx), Ok(()));
    }

    #[test]
    fn validate_rejects_claim_with_stale_epoch_nonce() {
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let ctx = accepting_context(&nullifiers);
        let op = claim_op(PREVIOUS_EPOCH - 1);
        assert_eq!(
            op.verify(&NoOpProof, &ctx),
            Err(ClaimPowRewardError::MismatchEpochNonce {
                claim: op.epoch_nonce,
                accepted: (PREVIOUS_EPOCH.into(), CURRENT_EPOCH.into()),
            })
        );
    }

    #[test]
    fn validate_rejects_ticket_above_the_reward_difficulty() {
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let mut ctx = accepting_context(&nullifiers);
        // The hardest possible target: only a ticket of exactly zero would
        // pass, and this op's ticket is not zero.
        ctx.reward_difficulty = Fr::ZERO;
        assert_eq!(
            claim_op(CURRENT_EPOCH).verify(&NoOpProof, &ctx),
            Err(ClaimPowRewardError::InvalidPoWRewardTicket)
        );
    }

    #[test]
    fn validate_rejects_ticket_equal_to_the_reward_difficulty() {
        // Spec §5.3: the check is the strict `puzzle_ticket <
        // difficulty_reward`, so a ticket exactly on the target does not
        // qualify.
        let nullifiers = rpds::HashTrieSetSync::new_sync();
        let op = claim_op(CURRENT_EPOCH);
        let mut ctx = accepting_context(&nullifiers);
        ctx.reward_difficulty = op.get_puzzle_ticket().into();
        assert_eq!(
            op.verify(&NoOpProof, &ctx),
            Err(ClaimPowRewardError::InvalidPoWRewardTicket)
        );
    }

    #[test]
    fn validate_rejects_already_claimed_ticket() {
        let op = claim_op(CURRENT_EPOCH);
        let nullifiers = rpds::HashTrieSetSync::new_sync().insert(op.get_puzzle_ticket());
        let ctx = accepting_context(&nullifiers);
        assert_eq!(
            op.verify(&NoOpProof, &ctx),
            Err(ClaimPowRewardError::DoubleClaimed)
        );
    }

    #[test]
    fn execute_issues_reward_utxo_and_registers_nullifier() {
        let op = claim_op(CURRENT_EPOCH);
        let epoch_reward = 10;
        let tx_hash = TxHash::from([11u8; 32]);

        let (ctx, events) = op
            .execute(ClaimPoWRewardExecutionContext {
                reward_pool: 1_000,
                epoch_reward,
                nullifiers: rpds::HashTrieSetSync::new_sync(),
                tx_hash,
                utxos: Utxos::new(),
            })
            .expect("claim execution should succeed");

        // The spent solution is recorded and the pool pays out sigma_e.
        assert!(ctx.nullifiers.contains(&op.get_puzzle_ticket()));
        assert_eq!(ctx.reward_pool, 990);

        // The reward note lands in the UTXO set, payable to the op's key
        // (§5.3 execution step 3). Regression: the persistent-tree insert
        // result used to be discarded, so the note never reached the set.
        let expected_utxo = Utxo {
            op_id: op.op_id(),
            output_index: 0,
            note: Note::new(epoch_reward, op.public_key),
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
        assert_eq!(*op_id, op.op_id());
        assert_eq!(*pow_nullifier, op.get_puzzle_ticket());
        assert_eq!(*utxo, expected_utxo);
        assert!(events.next().is_none());
    }

    #[test]
    fn execute_saturates_when_pool_cannot_cover_the_reward() {
        // Validation (`pow_reward_enabled`) is the real guard; execution
        // saturates defensively rather than underflow the pool.
        let op = claim_op(CURRENT_EPOCH);
        let (ctx, _events) = op
            .execute(ClaimPoWRewardExecutionContext {
                reward_pool: 5,
                epoch_reward: 10,
                nullifiers: rpds::HashTrieSetSync::new_sync(),
                tx_hash: TxHash::from([11u8; 32]),
                utxos: Utxos::new(),
            })
            .expect("claim execution should succeed");

        assert_eq!(ctx.reward_pool, 0);
    }
}
