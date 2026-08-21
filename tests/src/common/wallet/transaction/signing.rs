//! Transfer proof construction and final Mantle transaction signing.

use std::collections::HashMap;

use lb_core::mantle::{
    NoteId, Op, OpProof, OpRef, SignedOps, TxGasCalculator as _, TxHash,
    gas::MainnetGasProfile,
    traits::Hashable as _,
    transactions::{MantleTxBuilder, OpProofs, Ops, tx_list::ops::OpsContext},
};
use lb_key_management_system_service::keys::ZkKey;

use super::{error::WalletTransactionError, signed::SignedWalletTransaction};
use crate::common::wallet::WalletReservedInputs;

pub(super) type WalletTransferSigners = HashMap<NoteId, ZkKey>;

pub(super) fn sign_prepared_wallet_transaction(
    funded_builder: MantleTxBuilder,
    context: &OpsContext,
    tx_hash: TxHash,
    transfer_proofs: OpProofs,
    reserved_inputs: WalletReservedInputs,
    leading_op_proofs: OpProofs,
) -> Result<SignedWalletTransaction, WalletTransactionError> {
    let gas_prices = context.gas_context.get_gas_prices();
    let mantle_tx = funded_builder.build()?;
    let op_proofs = {
        let mut leading_op_proofs_vec = leading_op_proofs.into_inner().into_inner();
        leading_op_proofs_vec.extend(transfer_proofs);
        OpProofs::try_from(leading_op_proofs_vec)?
    };

    let signed_tx = SignedOps::from_parts(mantle_tx, op_proofs)?.preverify()?;
    let mandatory_fee_at_preparation = signed_tx
        .total_gas_cost::<MainnetGasProfile>(&gas_prices)?
        .into_inner();
    let output_total = signed_tx
        .op_refs()
        .into_iter()
        .filter_map(|op| match op {
            OpRef::Transfer(transfer) => Some(transfer),
            _ => None,
        })
        .flat_map(|transfer| transfer.outputs.iter())
        .try_fold(0u64, |total, note| {
            total
                .checked_add(note.value)
                .ok_or(WalletTransactionError::OutputTotalOverflow)
        })?;
    let paid_fee = reserved_inputs
        .total_value()
        .checked_sub(output_total)
        .ok_or(WalletTransactionError::FeeAccounting)?;

    Ok(SignedWalletTransaction::new(
        signed_tx,
        tx_hash,
        reserved_inputs,
        paid_fee,
        mandatory_fee_at_preparation,
    ))
}

/// Build one `ZkSig` proof per transfer op in a funded transaction, signing
/// every input with the same wallet key. Suitable for transactions whose
/// funding inputs all come from a single wallet account.
pub fn transfer_proofs_for_funded_wallet_tx(
    tx: &Ops,
    signing_key: &ZkKey,
) -> Result<OpProofs, WalletTransactionError> {
    let tx_hash = tx.hash();
    let proofs = tx
        .into_iter()
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
    Ok(OpProofs::try_from(proofs).expect("transaction proofs are bounded"))
}

pub(super) fn build_transfer_proofs(
    ops: &[Op],
    tx_hash: &TxHash,
    transfer_signers: &WalletTransferSigners,
) -> Result<OpProofs, WalletTransactionError> {
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
    Ok(OpProofs::try_from(proofs).expect("transaction proofs are bounded"))
}
