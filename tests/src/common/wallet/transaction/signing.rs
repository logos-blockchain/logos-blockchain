//! Transfer proof construction and final Mantle transaction signing.

use std::collections::HashMap;

use lb_core::mantle::{
    AuthenticatedMantleTx as _, NoteId, Op, OpProof, SignedMantleTx, TxHash,
    gas::MainnetGasConstants, tx_builder::MantleTxBuilder,
};
use lb_key_management_system_service::keys::ZkKey;

use super::{error::WalletTransactionError, signed::SignedWalletTransaction};
use crate::common::wallet::WalletReservedInputs;

pub(super) const ZKSIGN_MAX_INPUTS: usize = 32;
pub(super) type WalletTransferSigners = HashMap<NoteId, ZkKey>;

pub(super) fn sign_prepared_wallet_transaction(
    funded_builder: MantleTxBuilder,
    tx_hash: TxHash,
    transfer_proofs: Vec<OpProof>,
    reserved_inputs: WalletReservedInputs,
    leading_op_proofs: Vec<OpProof>,
) -> Result<SignedWalletTransaction, WalletTransactionError> {
    let gas_prices = funded_builder.get_gas_prices();
    let mantle_tx = funded_builder.build();
    let mut op_proofs = leading_op_proofs;
    op_proofs.extend(transfer_proofs);

    let signed_tx = SignedMantleTx::new(mantle_tx, op_proofs)?;
    let spent_fee = signed_tx
        .total_gas_cost::<MainnetGasConstants>(gas_prices)?
        .into_inner();

    Ok(SignedWalletTransaction::new(
        signed_tx,
        tx_hash,
        reserved_inputs,
        spent_fee,
    ))
}

pub(super) fn build_transfer_proofs(
    ops: &[Op],
    tx_hash: &TxHash,
    transfer_signers: &WalletTransferSigners,
) -> Result<Vec<OpProof>, WalletTransactionError> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Transfer(transfer_op) => Some(transfer_op),
            _ => None,
        })
        .map(|transfer_op| {
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
        .collect()
}
