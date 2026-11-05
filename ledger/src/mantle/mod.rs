pub use lb_core::mantle::channel;
pub mod helpers;
pub mod pow;
pub mod sdp;

use lb_core::{
    crypto::Hash,
    events::TxEvent,
    mantle::{
        ledger::verification_mode::{GenesisMode, StandardMode},
        note_set::NoteSets,
        ops::{
            SignedOperation,
            channel::{
                config::{ChannelConfigExecutionContext, ChannelConfigOp},
                inscribe::{InscriptionExecutionContext, InscriptionOp},
            },
            pow::{ClaimPowRewardError, PowReward},
            program::ProgramRegisterError,
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
            transfer::TransferError,
        },
        programs::Programs,
        transactions::{GenesisDeclarations, states::Verified},
    },
};
use lb_cryptarchia_engine::Slot;
use lb_log_targets::ledger;
use sdp::Error as SdpLedgerError;
use tracing::error;

use crate::{
    Config, EpochState,
    config::{EraScheduledConfig, config_for_epoch, pow_slot_windows},
    mantle::sdp::HeaderEffect,
};

const LOG_TARGET: &str = ledger::mantle::ROOT;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error(transparent)]
    Channel(#[from] channel::Error),
    #[error("Sdp ledger error: {0:?}")]
    Sdp(#[from] SdpLedgerError),
    #[error(transparent)]
    Transfer(#[from] TransferError),
    #[error(transparent)]
    ClaimPow(#[from] ClaimPowRewardError),
    #[error(transparent)]
    PowRewardPool(#[from] pow::PowRewardPoolOverflow),
    #[error(transparent)]
    Program(#[from] ProgramRegisterError),
}

/// A state of the mantle ledger
///
/// NOTE: Most collection fields in this struct should use `rpds`
/// since we keep a copy of this state for each block.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct LedgerState {
    channels: channel::Channels,
    pub sdp: sdp::SdpLedger,
    pub pow: pow::PowState,
    programs: Programs,
}

impl LedgerState {
    #[must_use]
    pub fn new(config: &Config, pow_reward_pool: PowReward, epoch_state: &EpochState) -> Self {
        Self {
            channels: channel::Channels::new(),
            sdp: sdp::SdpLedger::new(epoch_state.epoch())
                .with_blend_service(&config.sdp_config.service_rewards_params.blend, epoch_state),
            pow: pow::PowState::from_reward_pool_and_config(pow_reward_pool, config),
            programs: Programs::new(),
        }
    }

    pub fn from_genesis_tx(
        signed_operation_inscriptions: [SignedOperation<InscriptionOp, Verified, GenesisMode>; 2],
        signed_operation_declarations: GenesisDeclarations,
        config: &Config,
        note_sets: NoteSets,
        epoch_state: &EpochState,
    ) -> Result<(Self, NoteSets, Vec<TxEvent>), Error> {
        let mut tx_events = Vec::new();

        let (channels, events) = channel::Channels::from_genesis(signed_operation_inscriptions)?;
        tx_events.extend(events);

        let (sdp, note_sets, events) = sdp::SdpLedger::from_genesis(
            &config.sdp_config,
            note_sets,
            epoch_state,
            signed_operation_declarations,
        )?;
        tx_events.extend(events);

        Ok((
            Self {
                channels,
                sdp,
                pow: pow::PowState::from_reward_pool_and_config(
                    pow::POW_REWARD_POOL_GENESIS,
                    config,
                ),
                programs: Programs::new(),
            },
            note_sets,
            tx_events,
        ))
    }

    #[must_use]
    pub fn with_genesis_epoch_state(self, config: &Config, epoch_state: &EpochState) -> Self {
        Self {
            sdp: self
                .sdp
                .with_genesis_epoch_state(&config.sdp_config.service_rewards_params, epoch_state),
            ..self
        }
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

    #[must_use]
    pub const fn programs(&self) -> &Programs {
        &self.programs
    }

    #[must_use]
    pub fn update_programs(self, programs: Programs) -> Self {
        Self { programs, ..self }
    }

    pub fn try_apply_header(
        mut self,
        last_epoch_state: &EpochState,
        epoch_state: &EpochState,
        eras: &EraScheduledConfig,
    ) -> Result<(Self, HeaderEffect), Error> {
        let last_config = config_for_epoch(eras, last_epoch_state.epoch);
        let config = config_for_epoch(eras, epoch_state.epoch);
        let (new_sdp, effect) = self.sdp.try_apply_header(
            &last_config.sdp_config,
            &config.sdp_config,
            last_epoch_state,
            epoch_state,
        )?;
        self.sdp = new_sdp;
        self.pow = self.pow.apply_header(last_epoch_state, epoch_state, config);
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
        note_sets: NoteSets,
        block_slot: Slot,
    ) -> Result<(Self, NoteSets, Vec<TxEvent>), Error> {
        let (result, events) = signed_operation
            .execute(InscriptionExecutionContext {
                channels: self.channels,
                note_sets,
                block_slot,
            })
            .map_err(|(_signed_operation, error)| error)
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply channel inscribe message"),
            )?;
        self.channels = result.channels;

        Ok((self, result.note_sets, events))
    }

    pub fn try_apply_channel_config(
        mut self,
        config_op: SignedOperation<ChannelConfigOp, Verified, StandardMode>,
        note_sets: NoteSets,
        block_slot: Slot,
    ) -> Result<(Self, NoteSets, Vec<TxEvent>), Error> {
        let (result, events) = config_op
            .execute(ChannelConfigExecutionContext {
                channels: self.channels,
                note_sets,
                block_slot,
            })
            .map_err(|(_signed_operation, error)| error)
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply channel set-keys message"),
            )?;
        self.channels = result.channels;

        Ok((self, result.note_sets, events))
    }

    pub fn try_apply_sdp_declaration(
        mut self,
        sdp_declare_op: SignedOperation<SDPDeclareOp, Verified, StandardMode>,
        note_sets: NoteSets,
        config: &Config,
    ) -> Result<(Self, NoteSets, Vec<TxEvent>), Error> {
        let (sdp, note_sets, events) = self
            .sdp
            .try_apply_sdp_declaration(note_sets, sdp_declare_op, &config.sdp_config)
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply SDP declare message"),
            )?;
        self.sdp = sdp;
        Ok((self, note_sets, events))
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
        note_sets: NoteSets,
        config: &Config,
    ) -> Result<(Self, NoteSets, Vec<TxEvent>), Error> {
        let (result, note_sets, events) = self
            .sdp
            .apply_withdrawn_msg(note_sets, sdp_withdraw_op, &config.sdp_config)
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply SDP withdraw message"),
            )?;
        self.sdp = result;
        Ok((self, note_sets, events))
    }
}
