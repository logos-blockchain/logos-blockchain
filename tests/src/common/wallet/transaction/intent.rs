//! Requested transaction shape before wallet funding is applied.

use std::collections::HashMap;

use lb_core::mantle::{
    Note, Op,
    ops::channel::inscribe::InscriptionOp,
    transactions::{
        GasPrices, MantleTxBuilder,
        tx_list::ops::{OpsContext, OpsGasContext},
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};

use super::error::WalletTransactionError;
#[derive(Clone)]
pub struct WalletTransactionIntent {
    tx_builder: MantleTxBuilder,
    context: OpsContext,
    sender_output_total: u64,
    leading_inscription_signers: Vec<Ed25519Key>,
}

impl WalletTransactionIntent {
    #[must_use]
    const fn new(
        tx_builder: MantleTxBuilder,
        context: OpsContext,
        sender_output_total: u64,
    ) -> Self {
        Self {
            tx_builder,
            context,
            sender_output_total,
            leading_inscription_signers: Vec::new(),
        }
    }

    pub fn from_builder(
        tx_builder: MantleTxBuilder,
        context: OpsContext,
    ) -> Result<Self, WalletTransactionError> {
        let sender_output_total = transfer_output_total(&tx_builder)?;

        Ok(Self::new(tx_builder, context, sender_output_total))
    }

    pub fn transfer(receivers: &[(ZkPublicKey, u64)]) -> Result<Self, WalletTransactionError> {
        let empty_context = OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), HashMap::new(), GasPrices::default()),
            ..OpsContext::default()
        };
        let mut tx_builder = MantleTxBuilder::new();

        for (receiver_pk, value) in receivers {
            tx_builder = tx_builder.add_ledger_output(Note::new(*value, *receiver_pk))?;
        }

        Self::from_builder(tx_builder, empty_context)
    }

    /// Add a channel inscription before the builder-managed transfer op.
    /// Its signature is generated after funding has fixed the complete tx hash.
    pub fn with_leading_inscription(
        mut self,
        operation: InscriptionOp,
        signing_key: Ed25519Key,
    ) -> Result<Self, WalletTransactionError> {
        self.tx_builder = self.tx_builder.push_op(Op::ChannelInscribe(operation))?;
        self.leading_inscription_signers.push(signing_key);
        Ok(self)
    }

    #[must_use]
    pub fn with_gas_prices(mut self, gas_prices: GasPrices) -> Self {
        self.context.gas_context = OpsGasContext::new(HashMap::new(), HashMap::new(), gas_prices);
        self
    }

    #[must_use]
    pub(super) fn into_parts(self) -> (MantleTxBuilder, OpsContext, u64, Vec<Ed25519Key>) {
        (
            self.tx_builder,
            self.context,
            self.sender_output_total,
            self.leading_inscription_signers,
        )
    }
}

fn transfer_output_total(tx_builder: &MantleTxBuilder) -> Result<u64, WalletTransactionError> {
    let ops = tx_builder.clone().build()?;
    ops.iter()
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

#[cfg(test)]
mod tests {
    use lb_core::mantle::ops::channel::{ChannelId, MsgId, inscribe::Inscription};

    use super::*;

    #[test]
    fn plain_transfer_intent_keeps_the_independent_transaction_shape() {
        let intent = WalletTransactionIntent::transfer(&[(ZkPublicKey::zero(), 10)])
            .expect("plain transfer intent");
        let (builder, _, sender_output_total, inscription_signers) = intent.into_parts();
        let ops = builder.build().expect("transfer builder");

        assert_eq!(sender_output_total, 10);
        assert!(inscription_signers.is_empty());
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops.iter().next(), Some(Op::Transfer(_))));
    }

    #[test]
    fn hybrid_intent_contains_one_leading_inscription_and_the_transfer() {
        let signing_key = Ed25519Key::from_bytes(&[0x42; 32]);
        let inscription = InscriptionOp {
            channel_id: ChannelId::from([0x24; 32]),
            inscription: Inscription::try_from(1u64.to_le_bytes().to_vec())
                .expect("small fixed-width payload"),
            parent: MsgId::root(),
            signer: signing_key.public_key().into_unverified(),
        };
        let intent = WalletTransactionIntent::transfer(&[(ZkPublicKey::zero(), 10)])
            .expect("transfer intent")
            .with_leading_inscription(inscription, signing_key)
            .expect("hybrid intent");
        let (builder, _, _, inscription_signers) = intent.into_parts();
        let ops = builder.build().expect("hybrid builder");

        assert_eq!(ops.len(), 2);
        assert!(matches!(ops.iter().next(), Some(Op::ChannelInscribe(_))));
        assert!(matches!(ops.get(1), Some(Op::Transfer(_))));
        assert_eq!(inscription_signers.len(), 1);
    }
}
