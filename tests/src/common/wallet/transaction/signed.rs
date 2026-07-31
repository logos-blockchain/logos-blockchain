//! Signed wallet transaction plus reservation and fee accounting metadata.

use lb_core::{
    header::HeaderId,
    mantle::{
        SignedMantleTx,
        transactions::{hash::TxHash, states::Preverified},
    },
};

use crate::common::wallet::{TransactionFeePolicy, WalletReservedInputs};

pub struct SignedWalletTransaction {
    signed_tx: SignedMantleTx<Preverified>,
    tx_hash: TxHash,
    reserved_inputs: WalletReservedInputs,
    paid_fee: u64,
    mandatory_fee_at_preparation: u64,
    prepared_at_tip: Option<HeaderId>,
    prepared_at_epoch: Option<u32>,
    valid_through_epoch: Option<u32>,
}

impl SignedWalletTransaction {
    #[must_use]
    pub(super) fn new(
        signed_tx: SignedMantleTx<Preverified>,
        tx_hash: TxHash,
        reserved_inputs: WalletReservedInputs,
        paid_fee: u64,
        mandatory_fee_at_preparation: u64,
        fee_policy: Option<&TransactionFeePolicy>,
    ) -> Self {
        Self {
            signed_tx,
            tx_hash,
            reserved_inputs,
            paid_fee,
            mandatory_fee_at_preparation,
            prepared_at_tip: fee_policy.as_ref().map(|p| p.horizon.prepared_at_tip),
            prepared_at_epoch: fee_policy
                .as_ref()
                .map(|p| p.horizon.prepared_at_epoch.into_inner()),
            valid_through_epoch: fee_policy
                .as_ref()
                .map(|p| p.horizon.valid_through_epoch.into_inner()),
        }
    }

    #[must_use]
    pub const fn signed_tx(&self) -> &SignedMantleTx<Preverified> {
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

    #[must_use]
    pub const fn prepared_at_tip(&self) -> Option<HeaderId> {
        self.prepared_at_tip
    }

    #[must_use]
    pub const fn prepared_at_epoch(&self) -> Option<u32> {
        self.prepared_at_epoch
    }

    #[must_use]
    pub const fn valid_through_epoch(&self) -> Option<u32> {
        self.valid_through_epoch
    }
}
