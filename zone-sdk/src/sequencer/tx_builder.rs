use lb_core::{
    mantle::{
        MantleTx, SignedMantleTx, Transaction as _,
        channel::{ChannelState, SlotTimeframe, SlotTimeout},
        encoding::Ops,
        ops::{
            Op, OpProof,
            channel::{
                ChannelId, ChannelKeyIndex, MsgId,
                config::{ChannelConfigOp, Keys},
                inscribe::{Inscription, InscriptionOp},
            },
        },
        tx::TxHash,
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature};

use super::types::Error;

/// Build per-op proofs for an atomic withdraw bundle. The same single-signer
/// `ChannelMultiSigProof` is reused for every `ChannelWithdraw` op (all sign
/// the same tx hash with the same key) and the inscription op carries an
/// `Ed25519Sig` proof.
pub(super) fn build_atomic_withdraw_ops_proofs(
    tx: &MantleTx,
    own_key_index: ChannelKeyIndex,
    own_sig: Ed25519Signature,
) -> Result<Vec<OpProof>, Error> {
    let withdraw_proof =
        ChannelMultiSigProof::new(vec![IndexedSignature::new(own_key_index, own_sig)])
            .map_err(|e| Error::Network(format!("multi-sig proof assembly failed: {e:?}")))?;
    let mut ops_proofs = Vec::with_capacity(tx.ops().len());
    for op in tx.ops() {
        match op {
            Op::ChannelWithdraw(_) => {
                ops_proofs.push(OpProof::ChannelMultiSigProof(withdraw_proof.clone()));
            }
            Op::ChannelInscribe(_) => ops_proofs.push(OpProof::Ed25519Sig(own_sig)),
            _ => {
                return Err(Error::Network(format!(
                    "unexpected op in atomic withdraw bundle: {op:?}"
                )));
            }
        }
    }
    Ok(ops_proofs)
}

/// Find the position of the SDK's public key in the channel's `accredited_keys`
/// list. Returns an error if our key is not on the accredited list (we can't
/// sign for this channel).
pub(super) fn find_own_key_index(
    channel_state: &ChannelState,
    signing_key: &Ed25519Key,
) -> Result<ChannelKeyIndex, Error> {
    let own_pk = signing_key.public_key();
    channel_state
        .accredited_keys
        .iter()
        .position(|k| *k == own_pk)
        .map(|i| i as ChannelKeyIndex)
        .ok_or_else(|| Error::Network("sequencer key not in channel accredited_keys".into()))
}

pub(super) fn create_inscribe_tx(
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Inscription,
    parent: MsgId,
) -> (SignedMantleTx, MsgId) {
    let signer = signing_key.public_key();

    let inscribe_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer,
    };
    let msg_id = inscribe_op.id();

    // TODO: set realistic gas prices and fund tx
    let inscribe_tx = MantleTx([Op::ChannelInscribe(inscribe_op)].into());

    let tx_hash = inscribe_tx.hash();
    let signature = sign_tx(tx_hash, signing_key);

    let signed_tx = SignedMantleTx {
        ops_proofs: vec![OpProof::Ed25519Sig(signature)],
        mantle_tx: inscribe_tx,
    };

    (signed_tx, msg_id)
}

pub(super) fn create_channel_config_tx(
    channel_id: ChannelId,
    signing_keys: &[&Ed25519Key],
    keys: Keys,
    posting_timeframe: SlotTimeframe,
    posting_timeout: SlotTimeout,
    configuration_threshold: u16,
    withdraw_threshold: u16,
) -> SignedMantleTx {
    let config_op = ChannelConfigOp {
        channel: channel_id,
        keys,
        posting_timeframe,
        posting_timeout,
        configuration_threshold,
        withdraw_threshold,
    };

    // TODO: fund tx
    let config_tx = MantleTx([Op::ChannelConfig(config_op)].into());

    let tx_hash = config_tx.hash();
    let signatures = signing_keys
        .iter()
        .enumerate()
        .map(|(index, key)| {
            IndexedSignature::new(
                index as ChannelKeyIndex,
                key.sign_payload(tx_hash.as_signing_bytes().as_ref()),
            )
        })
        .collect();
    let proof = ChannelMultiSigProof::new(signatures).unwrap();

    SignedMantleTx {
        ops_proofs: vec![OpProof::ChannelMultiSigProof(proof)],
        mantle_tx: config_tx,
    }
}

pub(super) fn prepare_tx(
    mut ops: Ops,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Inscription,
    parent: MsgId,
) -> (MantleTx, MsgId, Ed25519Signature) {
    let inscription_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer: signing_key.public_key(),
    };
    let msg_id = inscription_op.id();
    // TODO: Return `Error` in case there's too many ops already.
    ops.try_push(Op::ChannelInscribe(inscription_op)).unwrap();

    // TODO: fund tx
    let tx = MantleTx(ops);

    let inscription_sig = sign_tx(tx.hash(), signing_key);

    (tx, msg_id, inscription_sig)
}

pub(super) fn sign_tx(tx_hash: TxHash, signing_key: &Ed25519Key) -> Ed25519Signature {
    signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref())
}
