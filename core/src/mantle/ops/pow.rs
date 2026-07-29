use std::collections::HashMap;

use ark_ff::Zero as _;
use lb_core_macros::NomCodec;
use lb_cryptarchia_engine::Epoch;
use lb_groth16::{Fr, fr_from_mod_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use nom::AsBytes as _;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    crypto::{Hash, ZkDigest as _, ZkHash, ZkHasher},
    events::TxEvent,
    mantle::ledger::Operation,
};

pub type PowTarget = Fr;
pub type PowReward = u64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize, NomCodec)]
pub struct PowNullifier(#[serde(with = "serde_fr")] ZkHash);

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

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, NomCodec)]
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
            fr_from_mod_bytes(self.block_hash.as_bytes()),
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

pub struct ClaimPoWRewardValidationContext<'a> {
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

impl ClaimPoWRewardValidationContext<'_> {
    /// Claiming must be enabled for this block context (pool can cover the
    /// reward)
    fn pow_reward_enabled(&self) -> Result<(), ClaimPowRewardError> {
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

        let check_height: i128 = i128::from(self.current_block_height) - i128::from(block_height);
        if check_height.is_negative() {
            return Err(ClaimPowRewardError::OutOfWindowHeight {
                height: block_height,
            });
        }
        if !(0 <= check_height && check_height <= i128::from(WINDOW)) {
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
        let current_epoch_nonce = ZkHasher::digest(&[fr_from_mod_bytes(
            &self.current_epoch_nonce.into_inner().to_le_bytes(),
        )]);
        if claim_epoch_nonce != previous_epoch_nonce && claim_epoch_nonce != current_epoch_nonce {
            return Err(ClaimPowRewardError::MismatchEpochNonce {
                claim: claim_epoch_nonce,
                accepted: (self.previous_epoch_nonce, self.current_epoch_nonce),
            });
        }
        Ok(())
    }

    fn validate_difficulty_reward(
        &self,
        puzzle_ticket: PuzzleTicket,
    ) -> Result<(), ClaimPowRewardError> {
        let ticket_as_fr = puzzle_ticket.0;
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

pub struct ClaimPoWRewardExecutionContext {
    _phantom: std::marker::PhantomData<()>, // fake content to be removed
}

impl Operation<ClaimPoWRewardValidationContext<'_>> for ClaimPowRewardOp {
    type ExecutionContext<'a>
        = ClaimPoWRewardExecutionContext
    where
        Self: 'a;
    type Error = ClaimPowRewardError;

    fn validate(&self, ctx: &ClaimPoWRewardValidationContext<'_>) -> Result<(), Self::Error> {
        ctx.pow_reward_enabled()?;
        // TODO Plug constant window
        ctx.accept_claim::<100>(self.block_hash)?;
        ctx.validate_current_epoch_nonce(self.epoch_nonce)?;
        let puzzle_ticket = self.get_puzzle_ticket();
        ctx.validate_difficulty_reward(puzzle_ticket)?;
        ctx.validate_double_claiming(puzzle_ticket)?;
        Ok(())
    }

    fn execute(
        &self,
        _ctx: Self::ExecutionContext<'_>,
    ) -> Result<(Self::ExecutionContext<'_>, Vec<TxEvent>), Self::Error> {
        unreachable!("Execution for ClaimPowReward is not integrated yet")
    }
}
