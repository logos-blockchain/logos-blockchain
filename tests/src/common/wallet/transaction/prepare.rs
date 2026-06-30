//! Orchestrates wallet funding, transfer proofs, and reserved-input extraction.

use std::collections::HashMap;

use lb_core::mantle::{
    MantleTx, NoteId, Op, Transaction as _, TxHash, Utxo, tx_builder::MantleTxBuilder,
};
use lb_key_management_system_service::keys::ZkPublicKey;

use super::{
    builder_funding::fund_wallet_transaction,
    error::WalletTransactionError,
    intent::WalletTransactionIntent,
    prepared::PreparedWalletTransaction,
    signing::{WalletTransferSigners, build_transfer_proofs},
};
use crate::common::wallet::{WalletFundingResources, WalletFundingSource, WalletReservedInputs};

pub struct PreparedWalletTransactionWorkItem {
    funded_builder: MantleTxBuilder,
    tx_hash: TxHash,
    ops: Vec<Op>,
    transfer_signers: WalletTransferSigners,
    reserved_inputs: WalletReservedInputs,
}

impl PreparedWalletTransactionWorkItem {
    #[must_use]
    pub fn reserved_inputs(&self) -> WalletReservedInputs {
        self.reserved_inputs.clone()
    }
}

pub fn prepare_wallet_transaction(
    intent: WalletTransactionIntent,
    resources: WalletFundingResources,
) -> Result<PreparedWalletTransaction, WalletTransactionError> {
    finalize_prepared_wallet_transaction(prepare_wallet_transaction_work_item(intent, resources)?)
}

pub fn prepare_wallet_transaction_work_item(
    intent: WalletTransactionIntent,
    resources: WalletFundingResources,
) -> Result<PreparedWalletTransactionWorkItem, WalletTransactionError> {
    let sender_pk = resources.sender().public_key();
    let fee_sponsor_pk = resources.fee_sponsor().map(WalletFundingSource::public_key);
    let transfer_signers = transfer_signers_for_funding(&resources);
    let input_utxos_by_note_id = input_utxos_by_note_id(&resources);

    let funded_builder = fund_wallet_transaction(intent, resources)?;
    let mantle_tx = funded_builder.clone().build()?;
    let tx_hash = mantle_tx.hash();
    let funding_inputs = funding_inputs_from_transfers(&mantle_tx, &input_utxos_by_note_id)?;
    let reserved_inputs = wallet_reserved_inputs_from_inputs(
        &funding_inputs,
        sender_pk,
        fee_sponsor_pk.unwrap_or(sender_pk),
    );

    Ok(PreparedWalletTransactionWorkItem {
        funded_builder,
        tx_hash,
        ops: mantle_tx.ops().to_vec(),
        transfer_signers,
        reserved_inputs,
    })
}

pub fn finalize_prepared_wallet_transaction(
    work_item: PreparedWalletTransactionWorkItem,
) -> Result<PreparedWalletTransaction, WalletTransactionError> {
    let PreparedWalletTransactionWorkItem {
        funded_builder,
        tx_hash,
        ops,
        transfer_signers,
        reserved_inputs,
    } = work_item;
    let transfer_proofs = build_transfer_proofs(&ops, &tx_hash, &transfer_signers)?;

    Ok(PreparedWalletTransaction::new(
        funded_builder,
        tx_hash,
        transfer_proofs,
        reserved_inputs,
    ))
}

fn transfer_signers_for_funding(resources: &WalletFundingResources) -> WalletTransferSigners {
    let mut transfer_signers = HashMap::new();

    for utxo in resources.sender().available_utxos() {
        transfer_signers.insert(utxo.id(), resources.sender().signing_key().clone());
    }

    if let Some(fee_sponsor) = resources.fee_sponsor() {
        for utxo in fee_sponsor.available_utxos() {
            transfer_signers.insert(utxo.id(), fee_sponsor.signing_key().clone());
        }
    }

    transfer_signers
}

fn input_utxos_by_note_id(resources: &WalletFundingResources) -> HashMap<NoteId, Utxo> {
    resources
        .sender()
        .available_utxos()
        .iter()
        .copied()
        .chain(
            resources
                .fee_sponsor()
                .into_iter()
                .flat_map(|fee_sponsor| fee_sponsor.available_utxos().iter().copied()),
        )
        .map(|utxo| (utxo.id(), utxo))
        .collect()
}

fn wallet_reserved_inputs_from_inputs(
    inputs: &[Utxo],
    wallet_pk: ZkPublicKey,
    fee_sponsor_pk: ZkPublicKey,
) -> WalletReservedInputs {
    let mut sender = Vec::new();
    let mut fee_sponsor = Vec::new();

    for utxo in inputs.iter().copied() {
        if utxo.note.pk == wallet_pk {
            sender.push(utxo);
        } else if utxo.note.pk == fee_sponsor_pk {
            fee_sponsor.push(utxo);
        }
    }

    WalletReservedInputs::new(sender, fee_sponsor)
}

fn funding_inputs_from_transfers(
    mantle_tx: &MantleTx,
    input_utxos_by_note_id: &HashMap<NoteId, Utxo>,
) -> Result<Vec<Utxo>, WalletTransactionError> {
    mantle_tx
        .ops()
        .iter()
        .filter_map(|op| match op {
            Op::Transfer(transfer_op) => Some(transfer_op),
            _ => None,
        })
        .flat_map(|transfer_op| transfer_op.inputs.iter())
        .map(|note_id| {
            input_utxos_by_note_id
                .get(note_id)
                .copied()
                .ok_or(WalletTransactionError::MissingFundingInput { note_id: *note_id })
        })
        .collect()
}
