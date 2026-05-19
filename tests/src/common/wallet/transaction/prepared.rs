//! Funded wallet transaction before final signing.

use lb_core::mantle::{OpProof, TxHash, tx_builder::MantleTxBuilder};

use super::{
    error::WalletTransactionError, signed::SignedWalletTransaction,
    signing::sign_prepared_wallet_transaction,
};
use crate::common::wallet::WalletReservedInputs;

pub struct PreparedWalletTransaction {
    funded_builder: MantleTxBuilder,
    tx_hash: TxHash,
    transfer_proofs: Vec<OpProof>,
    reserved_inputs: WalletReservedInputs,
}

impl PreparedWalletTransaction {
    #[must_use]
    pub(super) const fn new(
        funded_builder: MantleTxBuilder,
        tx_hash: TxHash,
        transfer_proofs: Vec<OpProof>,
        reserved_inputs: WalletReservedInputs,
    ) -> Self {
        Self {
            funded_builder,
            tx_hash,
            transfer_proofs,
            reserved_inputs,
        }
    }

    #[must_use]
    pub const fn tx_hash(&self) -> TxHash {
        self.tx_hash
    }

    pub fn sign_with_leading_proofs(
        self,
        leading_op_proofs: Vec<OpProof>,
    ) -> Result<SignedWalletTransaction, WalletTransactionError> {
        let Self {
            funded_builder,
            tx_hash,
            transfer_proofs,
            reserved_inputs,
        } = self;

        sign_prepared_wallet_transaction(
            funded_builder,
            tx_hash,
            transfer_proofs,
            reserved_inputs,
            leading_op_proofs,
        )
    }
}
