//! Requested transaction shape before wallet funding is applied.

use std::collections::HashMap;

use lb_core::mantle::{
    Note, Op,
    tx::{GasPrices, MantleTxContext, MantleTxGasContext},
    tx_builder::MantleTxBuilder,
};
use lb_key_management_system_service::keys::ZkPublicKey;

use super::error::WalletTransactionError;

pub struct WalletTransactionIntent {
    tx_builder: MantleTxBuilder,
    sender_output_total: u64,
}

impl WalletTransactionIntent {
    #[must_use]
    const fn new(tx_builder: MantleTxBuilder, sender_output_total: u64) -> Self {
        Self {
            tx_builder,
            sender_output_total,
        }
    }

    pub fn from_builder(tx_builder: MantleTxBuilder) -> Result<Self, WalletTransactionError> {
        let sender_output_total = transfer_output_total(&tx_builder)?;

        Ok(Self::new(tx_builder, sender_output_total))
    }

    pub fn transfer(
        receivers: &[(ZkPublicKey, u64)],
        storage_gas_price: u64,
    ) -> Result<Self, WalletTransactionError> {
        let empty_context = MantleTxContext {
            gas_context: MantleTxGasContext::new(
                HashMap::new(),
                HashMap::new(),
                GasPrices::new(0, storage_gas_price),
            ),
            ..MantleTxContext::default()
        };
        let mut tx_builder = MantleTxBuilder::new(empty_context);

        for (receiver_pk, value) in receivers {
            tx_builder = tx_builder.add_ledger_output(Note::new(*value, *receiver_pk));
        }

        Self::from_builder(tx_builder)
    }

    pub(super) fn into_parts(self) -> (MantleTxBuilder, u64) {
        (self.tx_builder, self.sender_output_total)
    }
}

fn transfer_output_total(tx_builder: &MantleTxBuilder) -> Result<u64, WalletTransactionError> {
    tx_builder
        .clone()
        .build()
        .ops()
        .iter()
        .filter_map(|op| match op {
            Op::Transfer(transfer) => Some(transfer),
            _ => None,
        })
        .flat_map(|transfer| transfer.outputs.iter())
        .try_fold(0u64, |total, note| {
            total
                .checked_add(note.value)
                .ok_or(WalletTransactionError::OutputTotalOverflow)
        })
}
