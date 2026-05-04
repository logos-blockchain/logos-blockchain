use std::collections::HashMap;

use lb_core::mantle::{
    OpProof, TxHash,
    gas::GasPrice,
    genesis_tx::GENESIS_STORAGE_GAS_PRICE,
    ops::{
        Op,
        channel::{ChannelId, MsgId, inscribe::InscriptionOp},
    },
    tx::{GasPrices, MantleTxContext, MantleTxGasContext},
    tx_builder::MantleTxBuilder,
};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature};

pub fn build_inscription_tx_builder(
    inscription: Vec<u8>,
    signing_key: &Ed25519Key,
    channel_id: ChannelId,
    parent: Option<MsgId>,
) -> MantleTxBuilder {
    let tx_context = MantleTxContext {
        gas_context: MantleTxGasContext::new(
            HashMap::new(),
            GasPrices {
                execution_base_gas_price: GasPrice::new(0),
                storage_gas_price: GENESIS_STORAGE_GAS_PRICE,
            },
        ),
        leader_reward_amount: 0,
    };

    MantleTxBuilder::new(tx_context).push_op(Op::ChannelInscribe(InscriptionOp {
        channel_id,
        inscription,
        parent: parent.unwrap_or_else(MsgId::root),
        signer: signing_key.public_key(),
    }))
}

#[must_use]
pub fn inscription_signature_proof(tx_hash: TxHash, signing_key: &Ed25519Key) -> OpProof {
    OpProof::Ed25519Sig(Ed25519Signature::from_bytes(
        &signing_key
            .sign_payload(tx_hash.as_signing_bytes().as_ref())
            .to_bytes(),
    ))
}

#[must_use]
pub fn channel_id_for_payload_size(payload_size: usize) -> ChannelId {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&(payload_size as u64).to_le_bytes());

    ChannelId::from(bytes)
}
