//! Channel ledger rules of the Mantle spec, applied to one block at a time.
//!
//! Only what a zone channel touches is modelled: channel state, channel notes
//! and plain notes. Ed25519 proofs are verified, ZK proofs are trusted. Every
//! transaction pays the real execution and storage gas at the ledger's gas
//! prices, as the node's ledger charges it.

use std::collections::{HashMap, HashSet};

use lb_core::{
    crypto::Hash,
    events::{DepositNote, DepositRecreatedNotes, TxEvent, TxEventPayload},
    mantle::{
        Note, NoteId, Utxo,
        channel::ChannelState,
        gas::{Gas, GasCost, MainnetGasProfile, TxGasCalculator as _},
        ledger::{Inputs, Outputs, verification_mode::StandardMode},
        ops::{
            OpId as _, OpProofRef, OpRef,
            channel::{
                ChannelId, ChannelKeyIndex, MsgId, UnverifiedChannelKeys,
                channel_transfer::ChannelTransferOp, config::ChannelConfigOp, deposit::DepositOp,
                inscribe::InscriptionOp, withdraw::ChannelWithdrawOp,
            },
            transfer::TransferOp,
        },
        traits::{Hashable as _, StorageSize as _},
        transactions::{GasPrices, SignedOps, states::Unverified, tx_list::ops::OpsGasContext},
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
};
use lb_key_management_system_service::keys::{UnverifiedEd25519PublicKey, ZkPublicKey};
use lb_zone_sdk::node_types::{Slot, TxHash};

/// Why a transaction was refused by the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub tx_hash: TxHash,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LedgerNote {
    utxo: Utxo,
    channel: Option<ChannelId>,
}

/// Channel state and notes after a block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimLedger {
    channels: HashMap<ChannelId, ChannelState>,
    notes: HashMap<NoteId, LedgerNote>,
    /// Notes minted out of band on this chain, so a fork re-mints what its
    /// branch has not seen.
    minted: HashSet<NoteId>,
    gas_prices: GasPrices,
}

impl SimLedger {
    #[must_use]
    pub fn channel(&self, id: &ChannelId) -> Option<&ChannelState> {
        self.channels.get(id)
    }

    #[must_use]
    pub fn note(&self, id: &NoteId) -> Option<&Note> {
        self.notes.get(id).map(|n| &n.utxo.note)
    }

    #[must_use]
    pub const fn gas_prices(&self) -> &GasPrices {
        &self.gas_prices
    }

    /// Prices charged from the next applied transaction on.
    pub const fn set_gas_prices(&mut self, prices: GasPrices) {
        self.gas_prices = prices;
    }

    /// The pricing context the node's wallet and ledger derive from the
    /// channels: thresholds in force plus the gas prices.
    #[must_use]
    pub fn gas_context(&self) -> OpsGasContext {
        let thresholds = |pick: fn(&ChannelState) -> ChannelKeyIndex| {
            self.channels
                .iter()
                .map(|(id, channel)| (*id, pick(channel)))
                .collect::<HashMap<_, _>>()
        };
        OpsGasContext::new(
            thresholds(|c| c.transfer_threshold),
            thresholds(|c| c.configuration_threshold),
            self.gas_prices.clone(),
        )
    }

    /// The channel owning `id`, if it is a channel note.
    #[must_use]
    pub fn note_channel(&self, id: &NoteId) -> Option<ChannelId> {
        self.notes.get(id)?.channel
    }

    #[must_use]
    pub fn channel_notes(&self, channel: &ChannelId) -> Vec<(NoteId, Note)> {
        let mut notes: Vec<(NoteId, Note)> = self
            .notes
            .iter()
            .filter(|(_, n)| n.channel == Some(*channel))
            .map(|(id, n)| (*id, n.utxo.note))
            .collect();
        notes.sort_by_key(|(id, _)| id.as_bytes());
        notes
    }

    /// Plain (non-channel) notes owned by `pk`.
    #[must_use]
    pub fn notes_of(&self, pk: &ZkPublicKey) -> Vec<(NoteId, Utxo)> {
        self.notes
            .iter()
            .filter(|(_, n)| n.channel.is_none() && n.utxo.note.pk == *pk)
            .map(|(id, n)| (*id, n.utxo))
            .collect()
    }

    /// Insert a plain note out of band (genesis funding, coin splits).
    pub fn mint(&mut self, id: NoteId, utxo: Utxo) {
        self.minted.insert(id);
        self.notes.insert(
            id,
            LedgerNote {
                utxo,
                channel: None,
            },
        );
    }

    /// Whether `id` was ever minted on this chain.
    #[must_use]
    pub fn has_minted(&self, id: &NoteId) -> bool {
        self.minted.contains(id)
    }

    /// Apply a whole transaction at `slot`; nothing changes on error. The
    /// transaction's balance (transfer inputs minus outputs) must cover its
    /// execution gas, priced against the channels before the transaction,
    /// plus the storage gas of its signed size.
    pub fn apply_tx(
        &mut self,
        tx: &SignedOps<Unverified, StandardMode>,
        slot: Slot,
    ) -> Result<Vec<TxEvent>, Rejection> {
        let tx_hash = tx.hash();
        let reject = |reason: String| Rejection { tx_hash, reason };
        let gas_cost = self
            .gas_cost(tx)
            .map_err(|e| reject(format!("gas: {e:?}")))?;
        let mut next = self.clone();
        let mut events = Vec::new();
        let mut balance: i128 = 0;
        for (op, proof) in tx.op_refs_iter().zip(tx.op_proof_refs_iter()) {
            balance += next
                .apply_op(op, proof, &tx_hash, slot, &mut events)
                .map_err(reject)?;
        }
        let required = i128::from(gas_cost.into_inner());
        if balance < required {
            return Err(reject(format!(
                "insufficient balance: {balance} paid, {required} gas required"
            )));
        }
        *self = next;
        Ok(events)
    }

    /// What the node's ledger charges `tx` against this state.
    pub fn gas_cost(
        &self,
        tx: &SignedOps<Unverified, StandardMode>,
    ) -> Result<GasCost, lb_core::mantle::gas::GasOverflow> {
        let context = self.gas_context();
        let execution = tx
            .op_refs()
            .execution_gas_consumption::<MainnetGasProfile>(&context)?;
        let storage = Gas::new(tx.storage_size() as u64);
        GasCost::calculate(execution, self.gas_prices.execution_base_gas_price)?.checked_add(
            GasCost::calculate(storage, self.gas_prices.storage_gas_price)?,
        )
    }

    fn apply_op(
        &mut self,
        op: OpRef<'_>,
        proof: OpProofRef<'_>,
        tx_hash: &TxHash,
        slot: Slot,
        events: &mut Vec<TxEvent>,
    ) -> Result<i128, String> {
        let signing = tx_hash.as_signing_bytes().as_ref();
        let unpaid = |result: Result<(), String>| result.map(|()| 0);
        match (op, proof) {
            (OpRef::ChannelInscribe(op), OpProofRef::Ed25519Sig(sig)) => {
                op.signer
                    .verify(signing, sig)
                    .map_err(|_| "inscription signature does not verify".to_owned())?;
                unpaid(self.inscribe(op, slot))
            }
            (OpRef::ChannelConfig(op), OpProofRef::ChannelMultiSigProof(proof)) => {
                unpaid(self.config(op, proof, signing, slot))
            }
            (OpRef::ChannelDeposit(op), OpProofRef::ZkSig(_)) => {
                unpaid(self.deposit(op, tx_hash, events))
            }
            (OpRef::ChannelWithdraw(op), OpProofRef::ChannelMultiSigProof(proof)) => {
                unpaid(self.withdraw(op, proof, signing))
            }
            (OpRef::ChannelTransfer(op), OpProofRef::ChannelMultiSigProof(proof)) => {
                unpaid(self.channel_transfer(op, proof, signing))
            }
            (OpRef::Transfer(op), OpProofRef::ZkSig(_)) => self.transfer(op),
            (op, _) => Err(format!("unsupported op or proof: {op:?}")),
        }
    }

    fn inscribe(&mut self, op: &InscriptionOp, slot: Slot) -> Result<(), String> {
        match self.channels.get(&op.channel_id) {
            Some(channel) => {
                if op.parent != channel.tip_message {
                    return Err(format!(
                        "invalid parent {} for channel tip {}",
                        op.parent, channel.tip_message
                    ));
                }
                let (index, _) = channel.round_robin(slot);
                let expected = channel
                    .accredited_keys
                    .get(usize::from(index))
                    .ok_or("round robin index out of range")?;
                if *expected != op.signer {
                    return Err(format!(
                        "signer is not the sequencer in turn (index {index})"
                    ));
                }
            }
            None => {
                if op.parent != MsgId::root() {
                    return Err("new channel needs a root parent".to_owned());
                }
            }
        }
        let channel = self
            .channels
            .entry(op.channel_id)
            .or_insert_with(|| default_channel(op.signer, slot));
        let (index, starting_slot) = channel.round_robin(slot);
        channel.tip_message = op.id();
        channel.tip_sequencer = index;
        channel.tip_sequencer_starting_slot = starting_slot;
        channel.tip_slot = slot;
        Ok(())
    }

    fn config(
        &mut self,
        op: &ChannelConfigOp,
        proof: &ChannelMultiSigProof,
        signing: &[u8],
        slot: Slot,
    ) -> Result<(), String> {
        if op.configuration_threshold == 0 || op.transfer_threshold == 0 {
            return Err("config thresholds must be positive".to_owned());
        }
        if let Some(channel) = self.channels.get(&op.channel) {
            if op.parent != channel.config_tip_hash {
                return Err(format!(
                    "invalid config parent {} for config tip {}",
                    op.parent, channel.config_tip_hash
                ));
            }
            verify_multi_sig(
                &channel.accredited_keys,
                channel.configuration_threshold,
                proof,
                signing,
            )?;
        } else {
            if op.parent != MsgId::root() {
                return Err("new channel config needs a root parent".to_owned());
            }
            if !proof.signatures().is_empty() {
                return Err("new channel config takes no signatures".to_owned());
            }
        }
        let keys: Vec<UnverifiedEd25519PublicKey> =
            op.keys.iter().map(|key| (*key).into_unverified()).collect();
        let first = keys.first().copied().ok_or("config without keys")?;
        let channel = self
            .channels
            .entry(op.channel)
            .or_insert_with(|| default_channel(first, slot));
        channel.accredited_keys = UnverifiedChannelKeys::try_from(keys)
            .map_err(|e| format!("keys: {e:?}"))?
            .into();
        channel.configuration_threshold = op.configuration_threshold;
        channel.transfer_threshold = op.transfer_threshold;
        channel.posting_timeframe = op.posting_timeframe.clone();
        channel.posting_timeout = op.posting_timeout.clone();
        channel.tip_sequencer = 0;
        channel.tip_sequencer_starting_slot = slot;
        channel.tip_slot = slot;
        channel.config_tip_hash = op.id();
        Ok(())
    }

    fn deposit(
        &mut self,
        op: &DepositOp,
        tx_hash: &TxHash,
        events: &mut Vec<TxEvent>,
    ) -> Result<(), String> {
        if !self.channels.contains_key(&op.channel_id) {
            return Err("deposit into unknown channel".to_owned());
        }
        let inputs = self.take_inputs(&op.inputs, None)?;
        let op_id = op.op_id();
        let mut amount = 0u64;
        let mut recreated = Vec::new();
        for (index, note) in inputs.into_iter().enumerate() {
            amount += note.value;
            let utxo = Utxo::new(op_id, index, note);
            let note_id = utxo.id();
            self.notes.insert(
                note_id,
                LedgerNote {
                    utxo,
                    channel: Some(op.channel_id),
                },
            );
            recreated.push(DepositNote {
                note_id,
                value: note.value,
                pk: note.pk,
            });
        }
        let notes = DepositRecreatedNotes::try_from(recreated)
            .map_err(|e| format!("too many deposit notes: {e:?}"))?;
        events.push(TxEvent::new(
            *tx_hash,
            op_id,
            TxEventPayload::Deposit {
                channel_id: op.channel_id,
                amount,
                metadata: op.metadata.clone(),
                notes,
            },
        ));
        Ok(())
    }

    fn withdraw(
        &mut self,
        op: &ChannelWithdrawOp,
        proof: &ChannelMultiSigProof,
        signing: &[u8],
    ) -> Result<(), String> {
        let channel = self
            .channels
            .get(&op.channel_id)
            .ok_or("withdraw from unknown channel")?;
        verify_multi_sig(
            &channel.accredited_keys,
            channel.transfer_threshold,
            proof,
            signing,
        )?;
        for id in op.inputs.iter() {
            let note = self
                .notes
                .get_mut(id)
                .ok_or_else(|| format!("withdraw input {id:?} does not exist"))?;
            if note.channel != Some(op.channel_id) {
                return Err(format!(
                    "withdraw input {id:?} is not a note of this channel"
                ));
            }
            note.channel = None;
        }
        Ok(())
    }

    fn channel_transfer(
        &mut self,
        op: &ChannelTransferOp,
        proof: &ChannelMultiSigProof,
        signing: &[u8],
    ) -> Result<(), String> {
        op.outputs
            .validate()
            .map_err(|e| format!("transfer outputs: {e:?}"))?;
        let channel = self
            .channels
            .get(&op.channel_id)
            .ok_or("transfer in unknown channel")?;
        verify_multi_sig(
            &channel.accredited_keys,
            channel.transfer_threshold,
            proof,
            signing,
        )?;
        let inputs = self.take_inputs(&op.inputs, Some(op.channel_id))?;
        let in_amount: u64 = inputs.iter().map(|n| n.value).sum();
        let out_amount = op
            .outputs
            .amount()
            .map_err(|e| format!("transfer outputs amount: {e:?}"))?;
        if in_amount != out_amount {
            return Err(format!(
                "unbalanced channel transfer: {in_amount} in, {out_amount} out"
            ));
        }
        self.insert_outputs(&op.outputs, op.op_id(), Some(op.channel_id));
        Ok(())
    }

    /// Returns the value the transfer leaves unspent, which pays the fee.
    fn transfer(&mut self, op: &TransferOp) -> Result<i128, String> {
        let inputs = self.take_inputs(&op.inputs, None)?;
        let in_amount: u64 = inputs.iter().map(|n| n.value).sum();
        let out_amount = op
            .outputs
            .amount()
            .map_err(|e| format!("transfer outputs amount: {e:?}"))?;
        if out_amount > in_amount {
            return Err(format!(
                "transfer creates value: {in_amount} in, {out_amount} out"
            ));
        }
        self.insert_outputs(&op.outputs, op.op_id(), None);
        Ok(i128::from(in_amount) - i128::from(out_amount))
    }

    /// Remove `inputs`, requiring each to exist and to belong to `channel`
    /// (`None` = a plain note).
    fn take_inputs(
        &mut self,
        inputs: &Inputs,
        channel: Option<ChannelId>,
    ) -> Result<Vec<Note>, String> {
        if inputs.is_empty() {
            return Err("empty inputs".to_owned());
        }
        let mut notes = Vec::with_capacity(inputs.len());
        for id in inputs.iter() {
            let entry = self
                .notes
                .get(id)
                .ok_or_else(|| format!("input {id:?} does not exist"))?;
            if entry.channel != channel {
                return Err(format!(
                    "input {id:?} belongs to {:?}, expected {channel:?}",
                    entry.channel
                ));
            }
            notes.push(entry.utxo.note);
            self.notes.remove(id);
        }
        Ok(notes)
    }

    fn insert_outputs(&mut self, outputs: &Outputs, op_id: Hash, channel: Option<ChannelId>) {
        for (index, note) in outputs.iter().enumerate() {
            let utxo = Utxo::new(op_id, index, *note);
            self.notes.insert(utxo.id(), LedgerNote { utxo, channel });
        }
    }
}

fn default_channel(signer: UnverifiedEd25519PublicKey, slot: Slot) -> ChannelState {
    ChannelState {
        accredited_keys: UnverifiedChannelKeys::from(signer).into(),
        configuration_threshold: 1,
        tip_message: MsgId::root(),
        config_tip_hash: MsgId::root(),
        tip_slot: slot,
        tip_sequencer: 0,
        tip_sequencer_starting_slot: slot,
        posting_timeframe: 0u32.into(),
        posting_timeout: 0u32.into(),
        transfer_threshold: 1,
    }
}

fn verify_multi_sig(
    keys: &UnverifiedChannelKeys,
    threshold: u16,
    proof: &ChannelMultiSigProof,
    signing: &[u8],
) -> Result<(), String> {
    let signatures = proof.signatures();
    if signatures.len() != usize::from(threshold) {
        return Err(format!(
            "{} signatures, threshold {threshold}",
            signatures.len()
        ));
    }
    for (index, signature) in signatures {
        let key = keys
            .get(usize::from(*index))
            .ok_or_else(|| format!("signature index {index} out of range"))?;
        key.verify(signing, signature)
            .map_err(|_| format!("signature at index {index} does not verify"))?;
    }
    Ok(())
}
