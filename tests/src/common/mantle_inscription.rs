use std::collections::HashMap;

use lb_core::mantle::{
    MantleTx, OpProof, SignedMantleTx, Transaction as _, Utxo,
    ops::{
        Op,
        channel::{ChannelId, MsgId, inscribe::InscriptionOp},
    },
    tx::{MantleTxContext, MantleTxGasContext},
    tx_builder::MantleTxBuilder,
};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature, ZkKey};
use lb_testing_framework::NodeHttpClient;

use crate::common::wallet::{current_utxos_for_public_key, fund_transfer_builder_from_utxos};

const STORAGE_GAS_PRICE: u64 = 1;

pub async fn build_funded_inscription_transaction(
    client: &NodeHttpClient,
    genesis_utxos: &[Utxo],
    funding_secret_key: &ZkKey,
    payload_size: usize,
    inscription: Vec<u8>,
) -> SignedMantleTx {
    let funding_utxos = collect_funding_utxos(client, genesis_utxos, funding_secret_key).await;
    let tx_builder = build_inscription_tx_builder(payload_size, inscription);
    let funded_tx = fund_inscription_transaction(funding_utxos, &tx_builder, funding_secret_key);

    sign_inscription_transaction(funded_tx)
}

async fn collect_funding_utxos(
    client: &NodeHttpClient,
    genesis_utxos: &[Utxo],
    funding_secret_key: &ZkKey,
) -> Vec<Utxo> {
    current_utxos_for_public_key(client, genesis_utxos, funding_secret_key.to_public_key()).await
}

fn build_inscription_tx_builder(payload_size: usize, inscription: Vec<u8>) -> MantleTxBuilder {
    let empty_context = MantleTxGasContext::new(HashMap::new());
    let tx_context = MantleTxContext {
        gas_context: empty_context,
        leader_reward_amount: 0,
    };

    MantleTxBuilder::new(tx_context)
        .push_op(Op::ChannelInscribe(build_inscription_op(
            payload_size,
            inscription,
        )))
        .set_storage_gas_price(STORAGE_GAS_PRICE.into())
        .set_execution_gas_price(0.into())
}

fn fund_inscription_transaction(
    funding_utxos: Vec<Utxo>,
    tx_builder: &MantleTxBuilder,
    funding_secret_key: &ZkKey,
) -> FundedInscriptionTransaction {
    let funding_public_key = funding_secret_key.to_public_key();
    let funded_builder =
        fund_transfer_builder_from_utxos(funding_utxos, tx_builder, funding_public_key)
            .expect("funding inscription transaction should succeed");

    let signing_keys = funded_builder
        .ledger_inputs()
        .iter()
        .map(|_| funding_secret_key.clone())
        .collect();

    FundedInscriptionTransaction {
        tx: funded_builder.build(),
        signing_keys,
    }
}

fn sign_inscription_transaction(funded_tx: FundedInscriptionTransaction) -> SignedMantleTx {
    let signing_key = Ed25519Key::from_bytes(&[0u8; 32]);
    let tx_hash = funded_tx.tx.hash();
    let ed25519_sig = Ed25519Signature::from_bytes(
        &signing_key
            .sign_payload(tx_hash.as_signing_bytes().as_ref())
            .to_bytes(),
    );
    let transfer_proof = ZkKey::multi_sign(&funded_tx.signing_keys, tx_hash.as_ref())
        .expect("transfer proof should build");

    SignedMantleTx::new(
        funded_tx.tx,
        vec![
            OpProof::Ed25519Sig(ed25519_sig),
            OpProof::ZkSig(transfer_proof),
        ],
    )
    .expect("funded inscription transaction should be valid")
}

fn build_inscription_op(payload_size: usize, inscription: Vec<u8>) -> InscriptionOp {
    let signing_key = Ed25519Key::from_bytes(&[0u8; 32]);

    InscriptionOp {
        channel_id: channel_id_for_payload(payload_size),
        inscription,
        parent: MsgId::root(),
        signer: signing_key.public_key(),
    }
}

fn channel_id_for_payload(payload_size: usize) -> ChannelId {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&(payload_size as u64).to_le_bytes());

    ChannelId::from(bytes)
}

struct FundedInscriptionTransaction {
    tx: MantleTx,
    signing_keys: Vec<ZkKey>,
}
