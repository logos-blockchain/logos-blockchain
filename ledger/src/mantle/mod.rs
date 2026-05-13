pub use lb_core::mantle::channel;
pub mod helpers;
pub mod leader;
pub mod sdp;

use std::collections::HashMap;

use lb_core::{
    crypto::ZkHasher,
    mantle::{
        GenesisTx, NoteId, TxHash, Utxo, Value,
        ledger::Operation as _,
        ops::{
            channel::{
                config::{
                    ChannelConfigExecutionContext, ChannelConfigOp, ChannelConfigValidationContext,
                },
                inscribe::{
                    InscriptionExecutionContext, InscriptionOp, InscriptionValidationContext,
                },
            },
            leader_claim::{LeaderClaimError, RewardsRoot, VoucherCm},
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
            transfer::TransferError,
        },
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
    sdp::{
        Declaration, DeclarationId, ProviderId, ProviderInfo, ServiceType, SessionNumber,
        locked_notes::LockedNotes,
    },
};
use lb_cryptarchia_engine::Slot;
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
use lb_mmr::MerkleMountainRange;
use sdp::Error as SdpLedgerError;
use tracing::error;

use crate::{Config, EpochState, UtxoTree};

const LOG_TARGET: &str = "ledger::mantle";

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
}

impl LedgerState {
    #[must_use]
    pub fn new(config: &Config, epoch_state: &EpochState) -> Self {
        Self {
            channels: channel::Channels::new(),
            sdp: sdp::SdpLedger::new()
                .with_blend_service(&config.sdp_config.service_rewards_params.blend, epoch_state),
            leaders: leader::LeaderState::new(),
        }
    }

    pub fn from_genesis_tx(
        tx: impl GenesisTx,
        config: &Config,
        utxo_tree: &UtxoTree,
        epoch_state: &EpochState,
    ) -> Result<Self, Error> {
        let channels = channel::Channels::from_genesis(tx.genesis_inscription())?;
        let sdp = sdp::SdpLedger::from_genesis(
            &config.sdp_config,
            utxo_tree,
            epoch_state,
            tx.sdp_declarations(),
        )?;

        Ok(Self {
            channels,
            sdp,
            leaders: leader::LeaderState::new(),
        })
    }

    #[must_use]
    pub const fn locked_notes(&self) -> &LockedNotes {
        self.sdp.locked_notes()
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
    pub fn active_session_providers(
        &self,
        service_type: ServiceType,
    ) -> Option<HashMap<ProviderId, ProviderInfo>> {
        self.sdp.active_session_providers(service_type)
    }

    #[must_use]
    pub fn active_sessions(&self) -> HashMap<ServiceType, SessionNumber> {
        self.sdp.active_sessions()
    }

    #[must_use]
    pub fn sdp_declarations(&self) -> Vec<(DeclarationId, Declaration)> {
        self.sdp.declarations()
    }

    /// Get the root of the voucher commitments snapshot.
    #[must_use]
    pub const fn vouchers_snapshot_root(&self) -> RewardsRoot {
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
        epoch_state: &EpochState,
        voucher: VoucherCm,
        config: &Config,
    ) -> Result<(Self, Vec<Utxo>), Error> {
        self.leaders = self.leaders.try_apply_header(epoch_state.epoch, voucher)?;
        let (new_sdp, reward_utxos) = self.sdp.try_apply_header(&config.sdp_config, epoch_state)?;
        self.sdp = new_sdp;
        Ok((self, reward_utxos))
    }

    pub fn try_apply_channel_inscription(
        mut self,
        inscription_op: &InscriptionOp,
        inscription_sig: &Ed25519Signature,
        tx_hash: TxHash,
        block_slot: Slot,
    ) -> Result<Self, Error> {
        //validate the inscription
        inscription_op.validate(&InscriptionValidationContext {
            channels: &self.channels,
            tx_hash: &tx_hash,
            inscribe_sig: inscription_sig,
            block_slot,
        })?;

        // Execute the inscription
        self.channels = inscription_op
            .execute(InscriptionExecutionContext {
                channels: self.channels,
                block_slot,
            })
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply channel inscribe message"),
            )?
            .channels;

        Ok(self)
    }

    pub fn try_apply_channel_set_keys(
        mut self,
        config_op: &ChannelConfigOp,
        config_sigs: &ChannelMultiSigProof,
        tx_hash: &TxHash,
        block_slot: Slot,
    ) -> Result<Self, Error> {
        // Validate the SetKeys
        config_op.validate(&ChannelConfigValidationContext {
            channels: &self.channels,
            tx_hash,
            config_sigs,
        })?;

        // Execute the SetKeys
        self.channels = config_op
            .execute(ChannelConfigExecutionContext {
                channels: self.channels,
                block_slot,
            })
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply channel set-keys message"),
            )?
            .channels;

        Ok(self)
    }

    pub fn try_apply_sdp_declaration(
        mut self,
        sdp_declare_op: &SDPDeclareOp,
        sdp_declare_zk_sig: &ZkSignature,
        sdp_declare_ed_sig: &Ed25519Signature,
        utxo_tree: &UtxoTree,
        tx_hash: TxHash,
        config: &Config,
    ) -> Result<Self, Error> {
        self.sdp = self
            .sdp
            .try_apply_sdp_declaration(
                utxo_tree,
                sdp_declare_op,
                sdp_declare_zk_sig,
                sdp_declare_ed_sig,
                tx_hash,
                &config.sdp_config,
            )
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply SDP declare message"),
            )?;
        Ok(self)
    }

    pub fn try_apply_sdp_active(
        mut self,
        sdp_active_op: &SDPActiveOp,
        sdp_active_zk_sig: &ZkSignature,
        tx_hash: TxHash,
        config: &Config,
    ) -> Result<Self, Error> {
        self.sdp = self
            .sdp
            .apply_active_msg(
                sdp_active_op,
                sdp_active_zk_sig,
                tx_hash,
                &config.sdp_config,
            )
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply SDP active message"),
            )?;
        Ok(self)
    }

    pub fn try_apply_sdp_withdraw(
        mut self,
        sdp_withdraw_op: &SDPWithdrawOp,
        sdp_withdraw_zk_sig: &ZkSignature,
        tx_hash: TxHash,
        config: &Config,
    ) -> Result<Self, Error> {
        self.sdp = self
            .sdp
            .apply_withdrawn_msg(
                sdp_withdraw_op,
                sdp_withdraw_zk_sig,
                tx_hash,
                &config.sdp_config,
            )
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply SDP withdraw message"),
            )?;
        Ok(self)
    }
}
