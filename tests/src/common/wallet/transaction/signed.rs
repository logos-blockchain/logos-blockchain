//! Signed wallet transaction plus reservation and fee accounting metadata.

use lb_core::mantle::{SignedMantleTx, TxHash};

use crate::common::wallet::WalletReservedInputs;

pub struct SignedWalletTransaction {
    signed_tx: SignedMantleTx,
    tx_hash: TxHash,
    reserved_inputs: WalletReservedInputs,
    spent_fee: u64,
}

impl SignedWalletTransaction {
    #[must_use]
    pub(super) const fn new(
        signed_tx: SignedMantleTx,
        tx_hash: TxHash,
        reserved_inputs: WalletReservedInputs,
        spent_fee: u64,
    ) -> Self {
        Self {
            signed_tx,
            tx_hash,
            reserved_inputs,
            spent_fee,
        }
    }

    #[must_use]
    pub const fn signed_tx(&self) -> &SignedMantleTx {
        &self.signed_tx
    }

    #[must_use]
    pub const fn tx_hash(&self) -> TxHash {
        self.tx_hash
    }

    #[must_use]
    pub fn reserved_inputs(&self) -> WalletReservedInputs {
        self.reserved_inputs.clone()
    }

    #[must_use]
    pub const fn spent_fee(&self) -> u64 {
        self.spent_fee
    }
}
