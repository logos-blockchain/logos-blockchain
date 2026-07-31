//! Funded wallet transaction before final signing.

use lb_core::mantle::{
    TxHash,
    transactions::{MantleTxBuilder, MantleTxContext, OpsProofs},
};

use super::{
    error::WalletTransactionError, signed::SignedWalletTransaction,
    signing::sign_prepared_wallet_transaction,
};
use crate::common::wallet::WalletReservedInputs;

pub struct PreparedWalletTransaction {
    funded_builder: MantleTxBuilder,
    context: MantleTxContext,
    tx_hash: TxHash,
    transfer_proofs: OpsProofs,
    reserved_inputs: WalletReservedInputs,
    fee_policy: Option<crate::common::wallet::TransactionFeePolicy>,
}

impl PreparedWalletTransaction {
    #[must_use]
    pub(super) const fn new(
        funded_builder: MantleTxBuilder,
        context: MantleTxContext,
        tx_hash: TxHash,
        transfer_proofs: OpsProofs,
        reserved_inputs: WalletReservedInputs,
        fee_policy: Option<crate::common::wallet::TransactionFeePolicy>,
    ) -> Self {
        Self {
            funded_builder,
            context,
            tx_hash,
            transfer_proofs,
            reserved_inputs,
            fee_policy,
        }
    }

    #[must_use]
    pub const fn tx_hash(&self) -> TxHash {
        self.tx_hash
    }

    pub fn sign_with_leading_proofs(
        self,
        leading_op_proofs: OpsProofs,
    ) -> Result<SignedWalletTransaction, WalletTransactionError> {
        let Self {
            funded_builder,
            context,
            tx_hash,
            transfer_proofs,
            reserved_inputs,
            fee_policy,
        } = self;

        sign_prepared_wallet_transaction(
            funded_builder,
            &context,
            tx_hash,
            transfer_proofs,
            reserved_inputs,
            fee_policy.as_ref(),
            leading_op_proofs,
        )
    }
}
