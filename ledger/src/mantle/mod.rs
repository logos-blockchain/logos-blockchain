pub use lb_core::mantle::channel;
pub mod helpers;
pub mod leader;
pub mod pow;
pub mod sdp;

use lb_core::{
    crypto::{Hash, ZkHasher},
    events::TxEvent,
    mantle::{
        NoteId, Value,
        ledger::verification_mode::{GenesisMode, StandardMode},
        ops::{
            SignedOperation,
            channel::{
                config::{ChannelConfigExecutionContext, ChannelConfigOp},
                inscribe::{InscriptionExecutionContext, InscriptionOp},
            },
            leader_claim::{LeaderClaimError, RewardsRoot, VoucherCm},
            pow::{ClaimPowRewardError, PowReward},
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
            transfer::TransferError,
        },
        transactions::{GenesisDeclarations, states::Verified},
    },
    sdp::service_notes::ServiceNotes,
};
use lb_cryptarchia_engine::Slot;
use lb_log_targets::ledger;
use lb_mmr::MerkleMountainRange;
use sdp::Error as SdpLedgerError;
use tracing::error;

use crate::{
    Config, EpochState, UtxoTree,
    config::{EraScheduledConfig, config_for_epoch, pow_slot_windows},
    mantle::sdp::HeaderEffect,
};

const LOG_TARGET: &str = ledger::mantle::ROOT;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error(transparent)]
    Channel(#[from] channel::Error),
    #[error(transparent)]
    Leader(#[from] leader::Error),
    #[error("Sdp ledger error: {0:?}")]
    Sdp(#[from] SdpLedgerError),
    #[error(transparent)]
    Transfer(#[from] TransferError),
    #[error(transparent)]
    LeaderClaim(#[from] LeaderClaimError),
    #[error(transparent)]
    ClaimPow(#[from] ClaimPowRewardError),
    #[error(transparent)]
    PowRewardPool(#[from] pow::PowRewardPoolOverflow),
    #[error("Note not found: {0:?}")]
    NoteNotFound(NoteId),
}

/// A state of the mantle ledger
///
/// NOTE: Most collection fields in this struct should use `rpds`
/// since we keep a copy of this state for each block.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct LedgerState {
    channels: channel::Channels,
    pub sdp: sdp::SdpLedger,
    pub leaders: leader::LeaderState,
    pub pow: pow::PowState,
}

impl LedgerState {
    #[must_use]
    pub fn new(config: &Config, pow_reward_pool: PowReward, epoch_state: &EpochState) -> Self {
        Self {
            channels: channel::Channels::new(),
            sdp: sdp::SdpLedger::new(epoch_state.epoch())
                .with_blend_service(&config.sdp_config.service_rewards_params.blend, epoch_state),
            leaders: leader::LeaderState::new(),
            pow: pow::PowState::from_reward_pool_and_config(pow_reward_pool, config),
        }
    }

    pub fn from_genesis_tx(
        signed_operation_inscription: SignedOperation<InscriptionOp, Verified, GenesisMode>,
        signed_operation_declarations: GenesisDeclarations,
        config: &Config,
        pow_reward_pool: PowReward,
        utxo_tree: &UtxoTree,
        epoch_state: &EpochState,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        let mut tx_events = Vec::new();

        let (channels, events) = channel::Channels::from_genesis(signed_operation_inscription)?;
        tx_events.extend(events);

        let (sdp, events) = sdp::SdpLedger::from_genesis(
            &config.sdp_config,
            utxo_tree,
            &channels,
            epoch_state,
            signed_operation_declarations,
        )?;
        tx_events.extend(events);

        Ok((
            Self {
                channels,
                sdp,
                leaders: leader::LeaderState::new(),
                pow: pow::PowState::from_reward_pool_and_config(pow_reward_pool, config),
            },
            tx_events,
        ))
    }

    #[must_use]
    pub const fn service_notes(&self) -> &ServiceNotes {
        self.sdp.service_notes()
    }

    #[must_use]
    pub const fn sdp_ledger(&self) -> &sdp::SdpLedger {
        &self.sdp
    }

    #[must_use]
    pub const fn channels(&self) -> &channel::Channels {
        &self.channels
    }

    #[must_use]
    pub fn update_channels(self, channels: channel::Channels) -> Self {
        Self { channels, ..self }
    }

    /// Get the root of the voucher commitments snapshot.
    #[must_use]
    pub const fn vouchers_snapshot_root(&self) -> &RewardsRoot {
        self.leaders.vouchers_snapshot_root()
    }

    /// Get the MMR of all voucher commitments included in the chain.
    #[must_use]
    pub const fn vouchers(&self) -> &MerkleMountainRange<VoucherCm, ZkHasher> {
        self.leaders.vouchers()
    }

    #[must_use]
    pub fn leader_reward_amount(&self) -> Value {
        self.leaders.reward_amount()
    }

    pub fn try_apply_header(
        mut self,
        last_epoch_state: &EpochState,
        epoch_state: &EpochState,
        voucher: VoucherCm,
        eras: &EraScheduledConfig,
    ) -> Result<(Self, HeaderEffect), Error> {
        let last_config = config_for_epoch(eras, last_epoch_state.epoch);
        let config = config_for_epoch(eras, epoch_state.epoch);
        self.leaders = self.leaders.try_apply_header(epoch_state.epoch, voucher)?;
        let (new_sdp, effect) = self.sdp.try_apply_header(
            &last_config.sdp_config,
            &config.sdp_config,
            last_epoch_state,
            epoch_state,
        )?;
        self.sdp = new_sdp;
        self.pow = self
            .pow
            .try_apply_header(last_epoch_state, epoch_state, config)?;
        // An era entered since the last block may start with a pool of its
        // own: the latest such era's, should the block skip several.
        let entered_eras = eras.iter().filter(|era| {
            last_epoch_state.epoch < era.first_epoch && era.first_epoch <= epoch_state.epoch
        });
        if let Some(reward_pool) = entered_eras
            .rev()
            .find_map(|era| era.parameters.pow_reward_pool_reset)
        {
            self.pow.reset_reward_pool(reward_pool, config);
        }
        Ok((self, effect))
    }

    /// Record a newly applied block among the recently seen blocks that
    /// `PoW` reward claims may anchor to, pruning the blocks, and the
    /// nullifiers of the claims anchored to them, that aged out of the
    /// acceptance window of their era: no claim in this block or a later one
    /// can anchor to them any more.
    ///
    /// This runs only on the canonical apply path, where the block's id is
    /// known — a proposer applying the header of a block it is still
    /// building has no id to record (and that block's transactions cannot
    /// anchor to it anyway).
    pub fn add_seen_block(&mut self, block_hash: Hash, slot: Slot, eras: &EraScheduledConfig) {
        let slot_windows = pow_slot_windows(eras);
        self.pow.add_seen_block_slots(block_hash, slot);
        self.pow.prune_seen_block_slots(slot, &slot_windows);
        self.pow.prune_nullifiers_by_slots(slot, &slot_windows);
    }

    pub fn try_apply_channel_inscription(
        mut self,
        signed_operation: SignedOperation<InscriptionOp, Verified, StandardMode>,
        block_slot: Slot,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        let (result, events) = signed_operation
            .execute(InscriptionExecutionContext {
                channels: self.channels,
                block_slot,
            })
            .map_err(|(_signed_operation, error)| error)
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply channel inscribe message"),
            )?;
        self.channels = result.channels;

        Ok((self, events))
    }

    pub fn try_apply_channel_config(
        mut self,
        config_op: SignedOperation<ChannelConfigOp, Verified, StandardMode>,
        block_slot: Slot,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        let (result, events) = config_op
            .execute(ChannelConfigExecutionContext {
                channels: self.channels,
                block_slot,
            })
            .map_err(|(_signed_operation, error)| error)
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply channel set-keys message"),
            )?;
        self.channels = result.channels;

        Ok((self, events))
    }

    pub fn try_apply_sdp_declaration(
        mut self,
        sdp_declare_op: SignedOperation<SDPDeclareOp, Verified, StandardMode>,
        utxo_tree: &UtxoTree,
        config: &Config,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        let (sdp, events) = self
            .sdp
            .try_apply_sdp_declaration(utxo_tree, sdp_declare_op, &config.sdp_config)
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply SDP declare message"),
            )?;
        self.sdp = sdp;
        Ok((self, events))
    }

    pub fn try_apply_sdp_active(
        mut self,
        sdp_active_op: SignedOperation<SDPActiveOp, Verified, StandardMode>,
        config: &Config,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        let (sdp, events) = self
            .sdp
            .apply_active_msg(sdp_active_op, &config.sdp_config)
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply SDP active message"),
            )?;
        self.sdp = sdp;
        Ok((self, events))
    }

    pub fn try_apply_sdp_withdraw(
        mut self,
        sdp_withdraw_op: SignedOperation<SDPWithdrawOp, Verified, StandardMode>,
        config: &Config,
    ) -> Result<(Self, Vec<TxEvent>), Error> {
        let (result, events) = self
            .sdp
            .apply_withdrawn_msg(sdp_withdraw_op, &config.sdp_config)
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply SDP withdraw message"),
            )?;
        self.sdp = result;
        Ok((self, events))
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{AdditiveGroup as _, Fr};

    use super::*;
    use crate::{
        cryptarchia::tests::{GENESIS_POW_REWARD_POOL, config},
        mantle::pow::{compute_epoch_pow_reward, tests::epoch_state},
        test_utils::schedule,
    };

    const ERA_1_POOL: PowReward = 5_940_000_000_000_000;

    /// Era 0 keeps the genesis pool; era 1, from epoch 2, starts with
    /// [`ERA_1_POOL`].
    fn eras_with_a_pool_reset() -> (Config, EraScheduledConfig) {
        let era_1 = Config {
            pow_reward_pool_reset: Some(ERA_1_POOL),
            ..config()
        };
        (era_1.clone(), schedule(config(), [(2, era_1)]))
    }

    /// The `PoW` state of a ledger started in epoch `from` with the genesis
    /// pool, once a block of epoch `to` is applied.
    fn pow_after(from: u32, to: u32, eras: &EraScheduledConfig) -> pow::PowState {
        LedgerState::new(&config(), GENESIS_POW_REWARD_POOL, &epoch_state(from))
            .try_apply_header(&epoch_state(from), &epoch_state(to), Fr::ZERO.into(), eras)
            .expect("the epoch transition applies")
            .0
            .pow
    }

    #[test]
    fn the_pow_reward_pool_is_reset_on_entering_an_era_that_starts_with_one() {
        let (era_1, eras) = eras_with_a_pool_reset();

        // Within era 0, and within era 1, the pool carries over.
        for (from, to) in [(0, 1), (2, 3)] {
            assert_eq!(
                pow_after(from, to, &eras).reward_pool(),
                GENESIS_POW_REWARD_POOL
            );
        }
        // Entering era 1 resets it, whether from the epoch before it or from
        // an earlier one, with the epochs between left without a block.
        for (from, to) in [(1, 2), (0, 2), (1, 3)] {
            let pow = pow_after(from, to, &eras);
            assert_eq!(pow.reward_pool(), ERA_1_POOL);
            assert_eq!(
                pow.epoch_reward(),
                compute_epoch_pow_reward(ERA_1_POOL, &era_1)
            );
        }
    }
}
