use lb_core::{
    mantle::{
        MantleTx, NoteId, SignedMantleTx, Transaction as _,
        ops::{
            Op, OpProof,
            channel::{
                ChannelId, Ed25519PublicKey, MsgId, inscribe::InscriptionOp, set_keys::SetKeysOp,
            },
        },
        tx::TxHash,
    },
    sdp::{ActiveMessage, DeclarationMessage, ServiceType, WithdrawMessage},
};
use lb_key_management_system_service::keys::{
    Ed25519Key, Ed25519Signature, ZkKey, ZkPublicKey, ZkSignature,
};

const TEST_SIGNING_KEY_BYTES: [u8; 32] = [0u8; 32];

fn prove_zk_signature(tx_hash: &TxHash, keys: &[ZkKey]) -> ZkSignature {
    ZkKey::multi_sign(keys, tx_hash.as_ref()).expect("zk signature generation should succeed")
}

#[must_use]
pub fn create_channel_inscribe_tx(
    signing_key: &Ed25519Key,
    channel_id: ChannelId,
    inscription: Vec<u8>,
    parent: MsgId,
) -> SignedMantleTx {
    let verifying_key_bytes = signing_key.public_key().to_bytes();
    let verifying_key = Ed25519PublicKey::from_bytes(&verifying_key_bytes).unwrap();

    let inscribe_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer: verifying_key,
    };

    let inscribe_tx = MantleTx {
        ops: vec![Op::ChannelInscribe(inscribe_op)],
        storage_gas_price: 0,
        execution_gas_price: 0,
    };

    let tx_hash = inscribe_tx.hash();
    let signature_bytes = signing_key
        .sign_payload(tx_hash.as_signing_bytes().as_ref())
        .to_bytes();
    let signature = Ed25519Signature::from_bytes(&signature_bytes);

    SignedMantleTx {
        ops_proofs: vec![OpProof::Ed25519Sig(signature)],
        mantle_tx: inscribe_tx,
    }
}

#[must_use]
pub fn create_channel_set_keys_tx(
    signing_key: &Ed25519Key,
    channel_id: ChannelId,
    keys: Vec<Ed25519PublicKey>,
) -> SignedMantleTx {
    let set_keys_op = SetKeysOp {
        channel: channel_id,
        keys,
    };

    let set_keys_tx = MantleTx {
        ops: vec![Op::ChannelSetKeys(set_keys_op)],
        storage_gas_price: 0,
        execution_gas_price: 0,
    };

    let tx_hash = set_keys_tx.hash();
    let signature_bytes = signing_key
        .sign_payload(tx_hash.as_signing_bytes().as_ref())
        .to_bytes();
    let signature = Ed25519Signature::from_bytes(&signature_bytes);

    SignedMantleTx {
        ops_proofs: vec![OpProof::Ed25519Sig(signature)],
        mantle_tx: set_keys_tx,
    }
}

#[must_use]
pub fn create_sdp_declare_tx(
    provider_signing_key: &Ed25519Key,
    service_type: ServiceType,
    locators: Vec<lb_core::sdp::Locator>,
    zk_id: ZkPublicKey,
    zk_sk: &ZkKey,
    locked_note_id: NoteId,
    note_sk: &ZkKey,
) -> (SignedMantleTx, DeclarationMessage) {
    let provider_pk_bytes = provider_signing_key.public_key().to_bytes();
    let provider_id = lb_core::sdp::ProviderId::try_from(provider_pk_bytes)
        .expect("Valid provider id from signing key");

    let declaration = DeclarationMessage {
        service_type,
        locators,
        provider_id,
        zk_id,
        locked_note_id,
    };

    let mantle_tx = MantleTx {
        ops: vec![Op::SDPDeclare(declaration.clone())],
        execution_gas_price: 0,
        storage_gas_price: 0,
    };

    let tx_hash = mantle_tx.hash();

    let ed25519_signature_bytes = provider_signing_key
        .sign_payload(tx_hash.as_signing_bytes().as_ref())
        .to_bytes();
    let ed25519_sig = Ed25519Signature::from_bytes(&ed25519_signature_bytes);

    let zk_sig = prove_zk_signature(&tx_hash, &[note_sk.clone(), zk_sk.clone()]);

    let signed_tx = SignedMantleTx {
        ops_proofs: vec![OpProof::ZkAndEd25519Sigs {
            zk_sig,
            ed25519_sig,
        }],
        mantle_tx,
    };

    (signed_tx, declaration)
}

#[must_use]
pub fn create_sdp_active_tx(
    active: &ActiveMessage,
    zk_sk: &ZkKey,
    note_sk: &ZkKey,
) -> SignedMantleTx {
    let mantle_tx = MantleTx {
        ops: vec![Op::SDPActive(active.clone())],
        execution_gas_price: 0,
        storage_gas_price: 0,
    };

    let tx_hash = mantle_tx.hash();
    let zk_sig = prove_zk_signature(&tx_hash, &[note_sk.clone(), zk_sk.clone()]);

    SignedMantleTx {
        ops_proofs: vec![OpProof::ZkSig(zk_sig)],
        mantle_tx,
    }
}

#[must_use]
pub fn create_sdp_withdraw_tx(
    withdraw: WithdrawMessage,
    zk_sk: &ZkKey,
    note_sk: &ZkKey,
) -> SignedMantleTx {
    let mantle_tx = MantleTx {
        ops: vec![Op::SDPWithdraw(withdraw)],
        execution_gas_price: 0,
        storage_gas_price: 0,
    };

    let tx_hash = mantle_tx.hash();
    let zk_sig = prove_zk_signature(&tx_hash, &[note_sk.clone(), zk_sk.clone()]);

    SignedMantleTx {
        ops_proofs: vec![OpProof::ZkSig(zk_sig)],
        mantle_tx,
    }
}

/// Creates a valid inscription transaction using the same hardcoded key as the
/// mock wallet adapter.
#[must_use]
pub fn create_inscription_transaction_with_id(
    id: ChannelId,
    inscription: Option<Vec<u8>>,
) -> SignedMantleTx {
    let signing_key = Ed25519Key::from_bytes(&TEST_SIGNING_KEY_BYTES);
    let signer = signing_key.public_key();

    let inscription_op = InscriptionOp {
        channel_id: id,
        inscription: inscription
            .unwrap_or_else(|| format!("Test channel inscription {id:?}").into_bytes()),
        parent: MsgId::root(),
        signer,
    };

    let mantle_tx = MantleTx {
        ops: vec![Op::ChannelInscribe(inscription_op)],
        storage_gas_price: 0,
        execution_gas_price: 0,
    };

    let tx_hash = mantle_tx.hash();
    let signature = signing_key.sign_payload(&tx_hash.as_signing_bytes());

    SignedMantleTx::new(mantle_tx, vec![OpProof::Ed25519Sig(signature)]).unwrap()
}
