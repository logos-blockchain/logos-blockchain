use std::collections::HashMap;

use ark_ff::Zero as _;
use lb_codec::{BinaryCodec, BinaryEncode};
use lb_cryptarchia_engine::Epoch;
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
            Utxos,
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode,
        },
        ops::{NoOpProof, OpId},
    },
};

pub type PowTarget = Fr;
pub type PowReward = Value;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct PowNullifier(#[serde(with = "serde_fr")] ZkHash);

impl PowNullifier {
    #[must_use]
    pub const fn as_fr(&self) -> &Fr {
        &self.0
    }
}

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

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ClaimPowRewardOp {
    #[serde(with = "serde_fr")]
    pub epoch_nonce: ZkHash,
    pub block_hash: Hash,
    pub public_key: ZkPublicKey,
}

impl ClaimPowRewardOp {
    #[must_use]
    pub fn get_puzzle_ticket(&self) -> PuzzleTicket {
        PowNullifier(ZkHasher::digest(&[
            self.epoch_nonce,
            fr_from_mod_bytes(&self.block_hash),
            *self.public_key.as_fr(),
        ]))
    }
}

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
    #[error("Out of window height ({height})")]
    OutOfWindowHeight { height: u64 },
    #[error("Missing block ({block_id:?})")]
    MissingBlock { block_id: Hash },
}

pub struct ClaimPoWRewardVerificationContext<'a> {
    // As per spec
    pub current_block_height: u64,
    pub reward_difficulty: PowTarget,
    pub pow_nullifiers: &'a rpds::HashTrieSetSync<PowNullifier>,
    // needed not in spec yet
    pub epoch_pow_reward: PowReward,
    pub epoch_reward_pool: PowReward,
    pub current_epoch_nonce: Epoch,
    pub previous_epoch_nonce: Epoch,
    pub blocks_height: HashMap<Hash, u64>,
}

impl ClaimPoWRewardVerificationContext<'_> {
    /// Claiming must be enabled for this block context (pool can cover the
    /// reward)
    fn are_pow_reward_enabled(&self) -> Result<(), ClaimPowRewardError> {
        if self.epoch_pow_reward.is_zero() {
            return Err(ClaimPowRewardError::EmptyRewards);
        }
        if self.epoch_reward_pool <= self.epoch_pow_reward {
            return Err(ClaimPowRewardError::InsufficientPoolBalance {
                pool: self.epoch_reward_pool,
                reward: self.epoch_pow_reward,
            });
        }
        Ok(())
    }

    /// On-chain `block_hash` window check
    pub fn accept_claim<const WINDOW: u64>(
        &self,
        block_id: Hash,
    ) -> Result<(), ClaimPowRewardError> {
        let Some(&block_height) = self.blocks_height.get(&block_id) else {
            return Err(ClaimPowRewardError::MissingBlock { block_id });
        };

        let Some(check_height) = self.current_block_height.checked_sub(block_height) else {
            return Err(ClaimPowRewardError::OutOfWindowHeight {
                height: block_height,
            });
        };
        if check_height > WINDOW {
            return Err(ClaimPowRewardError::OutOfWindowHeight {
                height: block_height,
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

    fn validate_difficulty_reward(
        &self,
        puzzle_ticket: PuzzleTicket,
    ) -> Result<(), ClaimPowRewardError> {
        let ticket_as_fr = *puzzle_ticket.as_fr();
        if ticket_as_fr > self.reward_difficulty {
            return Err(ClaimPowRewardError::InvalidPoWRewardTicket);
        }
        Ok(())
    }

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

pub struct ClaimPoWRewardExecutionContext {
    pub reward_pool: PowReward,
    pub epoch_reward: PowReward,
    pub nullifiers: rpds::HashTrieSetSync<PowNullifier>,
    pub tx_hash: TxHash,
    pub utxos: Utxos,
}

impl ClaimPoWRewardExecutionContext {
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
        context.accept_claim::<100>(self.block_hash)?;
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
        context.utxos.insert(utxo.id(), utxo);
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
