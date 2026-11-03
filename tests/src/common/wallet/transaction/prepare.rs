//! Orchestrates wallet funding, transfer proofs, and reserved-input extraction.

use std::collections::{BTreeMap, HashMap};

use lb_core::mantle::{
    Op,
    notes::NoteNullifier,
    ops::OpRef,
    traits::{Hashable as _, MantleTx},
    transactions::{
        MantleTxBuilder, builder::ZkTransferOpening, hash::TxHash, tx_list::ops::OpsContext,
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_wallet::OwnedNote;

use super::{
    builder_funding::fund_wallet_transaction,
    error::WalletTransactionError,
    intent::WalletTransactionIntent,
    prepared::PreparedWalletTransaction,
    signing::{WalletTransferSigners, build_leading_inscription_proofs, build_transfer_proofs},
};
use crate::common::wallet::{
    WalletFundingResources, WalletFundingSource, WalletReservedInputs, record_note_openings,
};

/// Intermediate transaction state after funding/input reservation but before
/// proof generation.
///
/// Keeping this separate lets tests reserve inputs cheaply, then finalize
/// expensive proof/signing work concurrently.
pub struct PreparedWalletTransactionWorkItem {
    funded_builder: MantleTxBuilder,
    context: OpsContext,
    tx_hash: TxHash,
    ops: Vec<Op>,
    // the notes each ZkTransfer spends and creates, by Operation index
    openings: BTreeMap<usize, ZkTransferOpening>,
    transfer_signers: WalletTransferSigners,
    leading_inscription_signers: Vec<Ed25519Key>,
    reserved_inputs: WalletReservedInputs,
}

impl PreparedWalletTransactionWorkItem {
    #[must_use]
    pub fn reserved_inputs(&self) -> WalletReservedInputs {
        self.reserved_inputs.clone()
    }
}

/// Prepare and immediately finalize a wallet transaction.
pub fn prepare_wallet_transaction(
    intent: WalletTransactionIntent,
    resources: WalletFundingResources,
    priority_fee_percent: u64,
) -> Result<PreparedWalletTransaction, WalletTransactionError> {
    finalize_prepared_wallet_transaction(prepare_wallet_transaction_work_item(
        intent,
        resources,
        priority_fee_percent,
    )?)
}

/// Fund a transaction and compute the inputs that must be reserved.
///
/// The returned work item has enough information to prevent duplicate input
/// selection before transfer proofs are built.
pub fn prepare_wallet_transaction_work_item(
    intent: WalletTransactionIntent,
    resources: WalletFundingResources,
    priority_fee_percent: u64,
) -> Result<PreparedWalletTransactionWorkItem, WalletTransactionError> {
    let sender_pk = resources.sender().owner_public_key();
    let fee_sponsor_pk = resources.fee_sponsor().map(WalletFundingSource::public_key);
    let transfer_signers = transfer_signers_for_funding(&resources);
    let input_utxos_by_note_id = input_utxos_by_note_id(&resources);

    let (funded_builder, context, leading_inscription_signers) =
        fund_wallet_transaction(intent, resources, priority_fee_percent)?;
    let openings = funded_builder.openings();
    // A scanner adopts the notes of its keys the transaction creates once
    // their commitments are appended.
    record_note_openings(
        openings
            .values()
            .flat_map(|opening| opening.outputs.iter().copied()),
    );
    let mantle_tx = funded_builder.clone().build()?;
    let tx_hash = mantle_tx.hash();
    let funding_inputs = funding_inputs_from_transfers(&mantle_tx, &input_utxos_by_note_id)?;
    let reserved_inputs = wallet_reserved_inputs_from_inputs(
        &funding_inputs,
        sender_pk,
        fee_sponsor_pk.unwrap_or(sender_pk),
    );

    let ops = mantle_tx.into_inner().into();
    Ok(PreparedWalletTransactionWorkItem {
        funded_builder,
        context,
        tx_hash,
        ops,
        openings,
        transfer_signers,
        leading_inscription_signers,
        reserved_inputs,
    })
}

/// Build transfer proofs and return a fully prepared transaction.
pub fn finalize_prepared_wallet_transaction(
    work_item: PreparedWalletTransactionWorkItem,
) -> Result<PreparedWalletTransaction, WalletTransactionError> {
    let PreparedWalletTransactionWorkItem {
        funded_builder,
        context,
        tx_hash,
        ops,
        openings,
        transfer_signers,
        leading_inscription_signers,
        reserved_inputs,
    } = work_item;
    let transfer_proofs = build_transfer_proofs(&ops, &openings, &tx_hash, &transfer_signers)?;
    let leading_op_proofs =
        build_leading_inscription_proofs(&ops, &tx_hash, &leading_inscription_signers)?;

    Ok(PreparedWalletTransaction::new(
        funded_builder,
        context,
        tx_hash,
        transfer_proofs,
        leading_op_proofs,
        reserved_inputs,
    ))
}

fn transfer_signers_for_funding(resources: &WalletFundingResources) -> WalletTransferSigners {
    let mut transfer_signers = HashMap::new();

    for utxo in resources.sender().available_utxos() {
        transfer_signers.insert(
            utxo.nullifier,
            (utxo.clone(), resources.sender().signing_key().clone()),
        );
    }

    if let Some(fee_sponsor) = resources.fee_sponsor() {
        for utxo in fee_sponsor.available_utxos() {
            transfer_signers.insert(
                utxo.nullifier,
                (utxo.clone(), fee_sponsor.signing_key().clone()),
            );
        }
    }

    transfer_signers
}

fn input_utxos_by_note_id(resources: &WalletFundingResources) -> HashMap<NoteNullifier, OwnedNote> {
    resources
        .sender()
        .available_utxos()
        .iter()
        .cloned()
        .chain(
            resources
                .fee_sponsor()
                .into_iter()
                .flat_map(|fee_sponsor| fee_sponsor.available_utxos().iter().cloned()),
        )
        .map(|utxo| (utxo.nullifier, utxo))
        .collect()
}

fn wallet_reserved_inputs_from_inputs(
    inputs: &[OwnedNote],
    wallet_pk: ZkPublicKey,
    fee_sponsor_pk: ZkPublicKey,
) -> WalletReservedInputs {
    let mut sender = Vec::new();
    let mut fee_sponsor = Vec::new();

    for utxo in inputs.iter().cloned() {
        if utxo.note.public_key == wallet_pk {
            sender.push(utxo);
        } else if utxo.note.public_key == fee_sponsor_pk {
            fee_sponsor.push(utxo);
        }
    }

    WalletReservedInputs::new(sender, fee_sponsor)
}

fn funding_inputs_from_transfers(
    mantle_tx: &impl MantleTx,
    input_utxos_by_note_id: &HashMap<NoteNullifier, OwnedNote>,
) -> Result<Vec<OwnedNote>, WalletTransactionError> {
    mantle_tx
        .op_refs()
        .into_iter()
        .filter_map(|op| match op {
            OpRef::Transfer(transfer_op) => Some(transfer_op),
            _ => None,
        })
        .flat_map(|transfer_op| transfer_op.inputs.iter())
        .map(|nullifier| {
            input_utxos_by_note_id.get(nullifier).cloned().ok_or(
                WalletTransactionError::MissingFundingInput {
                    nullifier: *nullifier,
                },
            )
        })
        .collect()
}
