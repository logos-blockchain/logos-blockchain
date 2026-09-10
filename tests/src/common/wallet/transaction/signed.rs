//! Signed wallet transaction plus reservation and fee accounting metadata.

use lb_core::mantle::{
    SignedOps,
    ledger::verification_mode::StandardMode,
    transactions::{hash::TxHash, states::Preverified},
};

use crate::common::wallet::WalletReservedInputs;

pub struct SignedWalletTransaction {
    signed_tx: SignedOps<Preverified, StandardMode>,
    tx_hash: TxHash,
    reserved_inputs: WalletReservedInputs,
    paid_fee: u64,
    mandatory_fee_at_preparation: u64,
}

impl SignedWalletTransaction {
    #[must_use]
    pub(super) const fn new(
        signed_tx: SignedOps<Preverified, StandardMode>,
        tx_hash: TxHash,
        reserved_inputs: WalletReservedInputs,
        paid_fee: u64,
        mandatory_fee_at_preparation: u64,
    ) -> Self {
        Self {
            signed_tx,
            tx_hash,
            reserved_inputs,
            paid_fee,
            mandatory_fee_at_preparation,
        }
    }

    #[must_use]
    pub const fn signed_tx(&self) -> &SignedOps<Preverified, StandardMode> {
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
        self.paid_fee
    }

    #[must_use]
    pub const fn paid_fee(&self) -> u64 {
        self.paid_fee
    }

    #[must_use]
    pub const fn mandatory_fee_at_preparation(&self) -> u64 {
        self.mandatory_fee_at_preparation
    }
}
