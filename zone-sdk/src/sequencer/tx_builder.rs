use lb_core::{
    mantle::{
        MantleTx, SignedMantleTx, Transaction as _,
        channel::{ChannelState, SlotTimeframe, SlotTimeout},
        ops::{
            Op, OpProof,
            channel::{
                ChannelId, ChannelKeyIndex, MsgId,
                config::{ChannelConfigOp, Keys},
                inscribe::{Inscription, InscriptionOp},
            },
        },
        transactions::{MantleTxBuilder, Ops, TxHash},
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};
use lb_http_api_common::bodies::wallet::fund::WalletFundRequestBody;
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature};

use super::types::{Error, FundingConfig};
use crate::adapter;

/// Assemble the ops for a transaction, funding it from the node's wallet when
/// a [`FundingConfig`] is present.
///
/// With funding, the node appends a fee transfer (paid from
/// `funding.funding_pk`, change back to it) and returns the proof for that
/// transfer; all other ops must be proven by the caller over the funded
/// transaction hash. Without funding the ops become a fee-less transaction
/// (only valid while gas prices are zero).
pub(super) async fn fund_ops<Node>(
    node: &Node,
    funding: Option<&FundingConfig>,
    ops: Vec<Op>,
) -> Result<(MantleTx, Option<OpProof>), Error>
where
    Node: adapter::Node + Sync,
{
    let Some(funding) = funding else {
        let ops = Ops::try_from(ops)
            .map_err(|e| Error::Network(format!("too many ops in transaction: {e:?}")))?;
        return Ok((MantleTx(ops), None));
    };

    let tx_builder = MantleTxBuilder::new()
        .extend_ops(ops)
        .map_err(|e| Error::Network(format!("too many ops in transaction: {e:?}")))?;
    let response = node
        .fund_tx(WalletFundRequestBody {
            // Fund against the node's latest tip.
            tip: None,
            tx_builder,
            change_public_key: funding.funding_pk,
            funding_public_keys: vec![funding.funding_pk],
            max_tx_fee: funding.max_tx_fee,
        })
        .await
        .map_err(|e| Error::Network(format!("funding failed: {e}")))?;

    Ok((response.funded_tx, response.transfer_proof))
}

/// Append the fee transfer's proof to the channel-op proofs, matching the
/// funded transaction's op layout (funding appends the transfer as the last
/// op; a fee-less transaction carries none).
pub(super) fn attach_transfer_proof(
    tx: &MantleTx,
    mut channel_proofs: Vec<OpProof>,
    transfer_proof: Option<OpProof>,
) -> Result<Vec<OpProof>, Error> {
    let transfer_count = tx
        .ops()
        .iter()
        .filter(|op| matches!(op, Op::Transfer(_)))
        .count();
    match (transfer_count, transfer_proof) {
        (0, _) => {}
        (1, Some(proof)) => channel_proofs.push(proof),
        (1, None) => {
            return Err(Error::Network(
                "funded transaction carries a fee transfer but no transfer proof".into(),
            ));
        }
        (n, _) => {
            return Err(Error::Network(format!(
                "unexpected transfer op count in funded transaction: {n}"
            )));
        }
    }
    Ok(channel_proofs)
}

/// Build per-op proofs for an atomic withdraw bundle. The same single-signer
/// `ChannelMultiSigProof` is reused for every `ChannelWithdraw` op (all sign
/// the same tx hash with the same key), the inscription op carries an
/// `Ed25519Sig` proof and the fee transfer — when the transaction was funded
/// — carries the wallet's proof.
pub(super) fn build_atomic_withdraw_ops_proofs(
    tx: &MantleTx,
    own_key_index: ChannelKeyIndex,
    own_sig: Ed25519Signature,
    transfer_proof: Option<&OpProof>,
) -> Result<Vec<OpProof>, Error> {
    let withdraw_proof =
        ChannelMultiSigProof::try_new([IndexedSignature::new(own_key_index, own_sig)].into())
            .map_err(|e| Error::Network(format!("multi-sig proof assembly failed: {e:?}")))?;
    let mut ops_proofs = Vec::with_capacity(tx.ops().len());
    for op in tx.ops() {
        match op {
            Op::ChannelWithdraw(_) => {
                ops_proofs.push(OpProof::ChannelMultiSigProof(withdraw_proof.clone()));
            }
            Op::ChannelInscribe(_) => ops_proofs.push(OpProof::Ed25519Sig(own_sig)),
            Op::Transfer(_) => match transfer_proof {
                Some(proof) => ops_proofs.push(proof.clone()),
                None => {
                    return Err(Error::Network(
                        "funded transaction carries a fee transfer but no transfer proof".into(),
                    ));
                }
            },
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

pub(super) async fn create_inscribe_tx<Node>(
    node: &Node,
    funding: Option<&FundingConfig>,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Inscription,
    parent: MsgId,
) -> Result<(SignedMantleTx, MsgId), Error>
where
    Node: adapter::Node + Sync,
{
    let signer = signing_key.public_key();

    let inscribe_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer,
    };
    let msg_id = inscribe_op.id();

    let (inscribe_tx, transfer_proof) =
        fund_ops(node, funding, vec![Op::ChannelInscribe(inscribe_op)]).await?;

    let tx_hash = inscribe_tx.hash();
    let signature = sign_tx(tx_hash, signing_key);
    let ops_proofs = attach_transfer_proof(
        &inscribe_tx,
        vec![OpProof::Ed25519Sig(signature)],
        transfer_proof,
    )?;

    let signed_tx = SignedMantleTx {
        ops_proofs,
        mantle_tx: inscribe_tx,
    };

    Ok((signed_tx, msg_id))
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the channel config op fields plus the funding context"
)]
pub(super) async fn create_channel_config_tx<Node>(
    node: &Node,
    funding: Option<&FundingConfig>,
    channel_id: ChannelId,
    signing_keys: &[&Ed25519Key],
    keys: Keys,
    posting_timeframe: SlotTimeframe,
    posting_timeout: SlotTimeout,
    configuration_threshold: u16,
    withdraw_threshold: u16,
) -> Result<SignedMantleTx, Error>
where
    Node: adapter::Node + Sync,
{
    let config_op = ChannelConfigOp {
        channel: channel_id,
        keys,
        posting_timeframe,
        posting_timeout,
        configuration_threshold,
        withdraw_threshold,
    };

    let (config_tx, transfer_proof) =
        fund_ops(node, funding, vec![Op::ChannelConfig(config_op)]).await?;

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
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let proof = ChannelMultiSigProof::try_new(signatures).unwrap();
    let ops_proofs = attach_transfer_proof(
        &config_tx,
        vec![OpProof::ChannelMultiSigProof(proof)],
        transfer_proof,
    )?;

    Ok(SignedMantleTx {
        ops_proofs,
        mantle_tx: config_tx,
    })
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
