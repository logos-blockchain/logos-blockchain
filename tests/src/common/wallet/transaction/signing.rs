//! Transfer proof construction and final Mantle transaction signing.

use std::collections::HashMap;

use lb_core::mantle::{
    GasCalculator as _, MantleTx, NoteId, Op, OpProof, SignedMantleTx, TxHash,
    gas::MainnetGasConstants,
    traits::Hashable as _,
    transactions::{MantleTxBuilder, MantleTxContext, OpsProofs},
};
use lb_key_management_system_service::keys::ZkKey;

use super::{error::WalletTransactionError, signed::SignedWalletTransaction};
use crate::common::wallet::WalletReservedInputs;

pub(super) type WalletTransferSigners = HashMap<NoteId, ZkKey>;

pub(super) fn sign_prepared_wallet_transaction(
    funded_builder: MantleTxBuilder,
    context: &MantleTxContext,
    tx_hash: TxHash,
    transfer_proofs: OpsProofs,
    reserved_inputs: WalletReservedInputs,
    leading_op_proofs: OpsProofs,
) -> Result<SignedWalletTransaction, WalletTransactionError> {
    let gas_prices = context.gas_context.get_gas_prices();
    let mantle_tx = funded_builder.build()?;
    let mut op_proofs = leading_op_proofs.into_inner();
    op_proofs.extend(transfer_proofs);
    let op_proofs = OpsProofs::new_unchecked(op_proofs);

    let signed_tx = SignedMantleTx::new(mantle_tx, op_proofs).preverify()?;
    let spent_fee = signed_tx
        .total_gas_cost::<MainnetGasConstants>(&gas_prices)?
        .into_inner();

    Ok(SignedWalletTransaction::new(
        signed_tx,
        tx_hash,
        reserved_inputs,
        spent_fee,
    ))
}

/// Build one `ZkSig` proof per transfer op in a funded transaction, signing
/// every input with the same wallet key. Suitable for transactions whose
/// funding inputs all come from a single wallet account.
pub fn transfer_proofs_for_funded_wallet_tx(
    tx: &MantleTx,
    signing_key: &ZkKey,
) -> Result<OpsProofs, WalletTransactionError> {
    let tx_hash = tx.hash();
    let proofs = tx
        .ops()
        .iter()
        .filter_map(|op| match op {
            Op::Transfer(transfer_op) => Some(transfer_op),
            _ => None,
        })
        .map(|transfer_op| -> Result<OpProof, WalletTransactionError> {
            let signing_keys = transfer_op
                .inputs
                .iter()
                .map(|_| signing_key.clone())
                .collect::<Vec<_>>();
            Ok(OpProof::ZkSig(ZkKey::multi_sign(
                &signing_keys,
                &tx_hash.to_fr(),
            )?))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OpsProofs::try_from(proofs).expect("transaction proofs are bounded"))
}

pub(super) fn build_transfer_proofs(
    ops: &[Op],
    tx_hash: &TxHash,
    transfer_signers: &WalletTransferSigners,
) -> Result<OpsProofs, WalletTransactionError> {
    let proofs = ops
        .iter()
        .filter_map(|op| match op {
            Op::Transfer(transfer_op) => Some(transfer_op),
            _ => None,
        })
        .map(|transfer_op| -> Result<OpProof, WalletTransactionError> {
            let signing_keys = transfer_op
                .inputs
                .iter()
                .map(|note_id| {
                    transfer_signers
                        .get(note_id)
                        .cloned()
                        .ok_or(WalletTransactionError::MissingSigningKey { note_id: *note_id })
                })
                .collect::<Result<Vec<_>, _>>()?;

            Ok(OpProof::ZkSig(ZkKey::multi_sign(
                &signing_keys,
                &tx_hash.to_fr(),
            )?))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OpsProofs::try_from(proofs).expect("transaction proofs are bounded"))
}
