use key_management_system_service::keys::{
    Ed25519Key, Ed25519Signature, ZkKey, ZkPublicKey, ZkSignature,
};
use nomos_core::{
    mantle::{
        MantleTx, NoteId, SignedMantleTx, Transaction as _,
        ledger::Tx as LedgerTx,
        ops::{
            Op, OpProof,
            channel::{
                ChannelId, Ed25519PublicKey, MsgId, blob::BlobOp, inscribe::InscriptionOp,
                set_keys::SetKeysOp,
            },
        },
        tx::TxHash,
    },
    sdp::{
        ActiveMessage, ActivityMetadata, DeclarationMessage, ServiceType, SessionNumber,
        WithdrawMessage, da,
    },
};

fn empty_ledger_signature(tx_hash: &TxHash) -> ZkSignature {
    ZkKey::multi_sign(&[], tx_hash.as_ref()).expect("multi-sign with empty key set works")
}

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

    let ledger_tx = LedgerTx::new(vec![], vec![]);

    let inscribe_tx = MantleTx {
        ops: vec![Op::ChannelInscribe(inscribe_op)],
        ledger_tx,
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
        ledger_tx_proof: empty_ledger_signature(&tx_hash),
        mantle_tx: inscribe_tx,
    }
}

#[must_use]
pub fn create_channel_blob_tx(
    signing_key: &Ed25519Key,
    channel_id: ChannelId,
    session: SessionNumber,
    blob: [u8; 32],
    blob_size: u64,
    parent: MsgId,
) -> SignedMantleTx {
    let verifying_key_bytes = signing_key.public_key().to_bytes();
    let verifying_key = Ed25519PublicKey::from_bytes(&verifying_key_bytes).unwrap();

    let blob_op = BlobOp {
        channel: channel_id,
        session,
        blob,
        blob_size,
        da_storage_gas_price: 0,
        parent,
        signer: verifying_key,
    };

    let blob_tx = MantleTx {
        ops: vec![Op::ChannelBlob(blob_op)],
        ledger_tx: LedgerTx::new(vec![], vec![]),
        storage_gas_price: 0,
        execution_gas_price: 0,
    };

    let tx_hash = blob_tx.hash();
    let signature_bytes = signing_key
        .sign_payload(tx_hash.as_signing_bytes().as_ref())
        .to_bytes();
    let signature = Ed25519Signature::from_bytes(&signature_bytes);

    SignedMantleTx {
        ops_proofs: vec![OpProof::Ed25519Sig(signature)],
        ledger_tx_proof: empty_ledger_signature(&tx_hash),
        mantle_tx: blob_tx,
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
        ledger_tx: LedgerTx::new(vec![], vec![]),
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
        ledger_tx_proof: empty_ledger_signature(&tx_hash),
        mantle_tx: set_keys_tx,
    }
}

#[must_use]
pub fn create_sdp_declare_tx(
    provider_signing_key: &Ed25519Key,
    service_type: ServiceType,
    locators: Vec<nomos_core::sdp::Locator>,
    zk_id: ZkPublicKey,
    zk_sk: &ZkKey,
    locked_note_id: NoteId,
    note_sk: &ZkKey,
) -> (SignedMantleTx, DeclarationMessage) {
    let provider_pk_bytes = provider_signing_key.public_key().to_bytes();
    let provider_id = nomos_core::sdp::ProviderId::try_from(provider_pk_bytes)
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
        ledger_tx: LedgerTx::new(vec![], vec![]),
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
        ledger_tx_proof: empty_ledger_signature(&tx_hash),
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
        ledger_tx: LedgerTx::new(vec![], vec![]),
        execution_gas_price: 0,
        storage_gas_price: 0,
    };

    let tx_hash = mantle_tx.hash();
    let zk_sig = prove_zk_signature(&tx_hash, &[note_sk.clone(), zk_sk.clone()]);

    SignedMantleTx {
        ops_proofs: vec![OpProof::ZkSig(zk_sig)],
        ledger_tx_proof: empty_ledger_signature(&tx_hash),
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
        ledger_tx: LedgerTx::new(vec![], vec![]),
        execution_gas_price: 0,
        storage_gas_price: 0,
    };

    let tx_hash = mantle_tx.hash();
    let zk_sig = prove_zk_signature(&tx_hash, &[note_sk.clone(), zk_sk.clone()]);

    SignedMantleTx {
        ops_proofs: vec![OpProof::ZkSig(zk_sig)],
        ledger_tx_proof: empty_ledger_signature(&tx_hash),
        mantle_tx,
    }
}

#[must_use]
pub const fn empty_da_activity_proof() -> ActivityMetadata {
    ActivityMetadata::DataAvailability(da::ActivityProof {
        current_session: 0,
        previous_session_opinions: vec![],
        current_session_opinions: vec![],
    })
}
