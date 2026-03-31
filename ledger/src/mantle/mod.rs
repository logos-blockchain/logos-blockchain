pub mod channel;
pub mod leader;
pub mod sdp;

use std::collections::HashMap;

use lb_core::{
    crypto::ZkHash,
    mantle::{
        GenesisTx, NoteId, TxHash, Utxo, Value,
        ops::{
            channel::{deposit::DepositOp, inscribe::InscriptionOp, set_keys::SetKeysOp},
            leader_claim::{LeaderClaimOp, RewardsRoot, VoucherCm},
            sdp::{SDPActiveOp, SDPDeclareOp, SDPWithdrawOp},
        },
    },
    sdp::{Declaration, DeclarationId, ProviderId, ProviderInfo, ServiceType, SessionNumber},
};
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
use lb_utxotree::MerklePath;
use sdp::{Error as SdpLedgerError, locked_notes::LockedNotes};
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
    #[error("Note not found: {0:?}")]
    NoteNotFound(NoteId),
}

/// A state of the mantle ledger
///
/// NOTE: Most collection fields in this struct should use `rpds`
/// since we keep a copy of this state for each block.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, PartialEq, Debug)]
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
            sdp: sdp::SdpLedger::new().with_blend_service(
                config.sdp_config.service_rewards_params.blend.clone(),
                epoch_state,
            ),
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
            tx.hash(),
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

    #[must_use]
    pub fn has_claimable_voucher(&self, voucher_cm: &VoucherCm) -> bool {
        self.leaders.has_claimable_voucher(voucher_cm)
    }

    #[must_use]
    pub const fn claimable_vouchers_root(&self) -> RewardsRoot {
        self.leaders.claimable_vouchers_root()
    }

    #[must_use]
    pub fn voucher_merkle_path(&self, voucher_cm: VoucherCm) -> Option<MerklePath<ZkHash>> {
        self.leaders.voucher_merkle_path(voucher_cm)
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
    ) -> Result<Self, Error> {
        self.channels = self
            .channels
            .apply_msg(
                inscription_op.channel_id,
                &inscription_op.parent,
                inscription_op.id(),
                &inscription_op.signer,
            )
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply channel inscribe message"),
            )?;
        Ok(self)
    }

    pub fn try_apply_channel_set_keys(
        mut self,
        set_keys_op: &SetKeysOp,
        set_keys_sig: &Ed25519Signature,
        tx_hash: &TxHash,
    ) -> Result<Self, Error> {
        self.channels = self
            .channels
            .set_keys(set_keys_op.channel, set_keys_op, set_keys_sig, tx_hash)
            .inspect_err(
                |err| error!(target: LOG_TARGET, %err, "failed to apply channel set-keys message"),
            )?;
        Ok(self)
    }

    pub fn try_apply_channel_deposit(mut self, op: &DepositOp) -> Result<(Self, Value), Error> {
        self.channels = self.channels.deposit(op).inspect_err(
            |err| error!(target: LOG_TARGET, %err, "Failed to apply the Channel Deposit message."),
        )?;
        Ok((self, op.amount))
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
        let Some((utxo, _)) = utxo_tree.utxos().get(&sdp_declare_op.locked_note_id) else {
            return Err(Error::NoteNotFound(sdp_declare_op.locked_note_id));
        };
        self.sdp = self
            .sdp
            .apply_declare_msg(
                sdp_declare_op,
                utxo.note,
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

    pub fn try_apply_leader_claim(
        mut self,
        leader_claim_op: &LeaderClaimOp,
    ) -> Result<(Self, Value), Error> {
        // Correct derivation of the voucher nullifier and membership in the merkle tree
        // can be verified outside of this function since public inputs are already
        // available. Callers are expected to validate the proof
        // before calling this function.
        let reward;
        (self.leaders, reward) = self.leaders.claim(leader_claim_op).inspect_err(
            |err| error!(target: LOG_TARGET, %err, "failed to apply leader claim message"),
        )?;
        Ok((self, reward))
    }
}
