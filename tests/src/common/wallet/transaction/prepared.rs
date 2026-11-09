//! Funded wallet transaction before final signing.

use lb_core::mantle::{
    TxHash,
    transactions::{OpProofs, Ops, tx_list::ops::OpsContext},
};

use super::{
    error::WalletTransactionError, signed::SignedWalletTransaction,
    signing::sign_prepared_wallet_transaction,
};
use crate::common::wallet::WalletReservedInputs;

pub struct PreparedWalletTransaction {
    mantle_tx: Ops,
    context: OpsContext,
    tx_hash: TxHash,
    transfer_proofs: OpProofs,
    leading_op_proofs: OpProofs,
    reserved_inputs: WalletReservedInputs,
}

impl PreparedWalletTransaction {
    #[must_use]
    pub(super) const fn new(
        mantle_tx: Ops,
        context: OpsContext,
        tx_hash: TxHash,
        transfer_proofs: OpProofs,
        leading_op_proofs: OpProofs,
        reserved_inputs: WalletReservedInputs,
    ) -> Self {
        Self {
            mantle_tx,
            context,
            tx_hash,
            transfer_proofs,
            leading_op_proofs,
            reserved_inputs,
        }
    }

    #[must_use]
    pub const fn tx_hash(&self) -> TxHash {
        self.tx_hash
    }

    pub fn sign_with_leading_proofs(
        self,
        leading_op_proofs: OpProofs,
    ) -> Result<SignedWalletTransaction, WalletTransactionError> {
        let Self {
            mantle_tx,
            context,
            tx_hash,
            transfer_proofs,
            leading_op_proofs: stored_leading_op_proofs,
            reserved_inputs,
        } = self;

        let mut all_leading_proofs = stored_leading_op_proofs.into_inner().into_inner();
        all_leading_proofs.extend(leading_op_proofs.into_inner().into_inner());

        sign_prepared_wallet_transaction(
            mantle_tx,
            &context,
            tx_hash,
            transfer_proofs,
            reserved_inputs,
            OpProofs::try_from(all_leading_proofs)?,
        )
    }
}
