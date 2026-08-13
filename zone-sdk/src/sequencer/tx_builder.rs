use lb_core::{
    mantle::{
        SignedMantleTx, TxHash,
        channel::{ChannelState, SlotTimeframe, SlotTimeout},
        ops::{
            Op, OpProof,
            channel::{
                ChannelId, ChannelKeyIndex, MsgId,
                config::{ChannelConfigOp, Keys},
                inscribe::{Inscription, InscriptionOp},
            },
        },
        traits::Hashable as _,
        transactions::{
            MantleTxBuilder, Ops, OpsProofs,
            mantle_tx::{MantleTx, RawMantleTx},
            states::Unverified,
        },
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};
use lb_http_api_common::bodies::wallet::fund::WalletFundRequestBody;
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature};

use super::types::{Error, FundingConfig};
use crate::adapter;

/// Append the fee transfer's proof to the channel-op proofs, matching the
/// funded transaction's op layout (funding appends the transfer as the last
/// op).
pub(super) fn attach_transfer_proof(
    tx: &impl MantleTx,
    mut channel_proofs: OpsProofs,
    transfer_proof: Option<OpProof>,
) -> Result<OpsProofs, Error> {
    let transfer_count = tx
        .ops()
        .iter()
        .filter(|op| matches!(op, Op::Transfer(_)))
        .count();
    match (transfer_count, transfer_proof) {
        (0, _) => {}
        (1, Some(proof)) => channel_proofs
            .try_push(proof)
            .map_err(|e| Error::Network(format!("too many operation proofs: {e:?}")))?,
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
/// TODO: enable it back when wallet tracks channel notes
#[expect(
    dead_code,
    reason = "Belongs to the atomic withdraw flow; restored with `do_publish_atomic_withdraw`."
)]
pub(super) fn build_atomic_withdraw_ops_proofs(
    tx: &impl MantleTx,
    own_key_index: ChannelKeyIndex,
    own_sig: Ed25519Signature,
    transfer_proof: Option<&OpProof>,
) -> Result<OpsProofs, Error> {
    let withdraw_proof =
        ChannelMultiSigProof::try_new([IndexedSignature::new(own_key_index, own_sig)].into())
            .map_err(|e| Error::Network(format!("multi-sig proof assembly failed: {e:?}")))?;
    let mut ops_proofs = OpsProofs::empty();
    for op in tx.ops() {
        match op {
            Op::ChannelWithdraw(_) => {
                ops_proofs
                    .try_push(OpProof::ChannelMultiSigProof(withdraw_proof.clone()))
                    .map_err(|e| Error::Network(format!("too many operation proofs: {e:?}")))?;
            }
            Op::ChannelInscribe(_) => ops_proofs
                .try_push(OpProof::Ed25519Sig(own_sig))
                .map_err(|e| Error::Network(format!("too many operation proofs: {e:?}")))?,
            Op::Transfer(_) => match transfer_proof {
                Some(proof) => ops_proofs
                    .try_push(proof.clone())
                    .map_err(|e| Error::Network(format!("too many operation proofs: {e:?}")))?,
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
/// TODO: reactivate when channel notes are tracked by the wallet
#[expect(
    dead_code,
    reason = "Belongs to the atomic withdraw flow; restored with `do_publish_atomic_withdraw`."
)]
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

/// Fund a pre-built channel-op builder and sign each channel op over the
/// funded hash. Returns the signed tx together with the **pre-funding**
/// builder (the channel ops without the fee transfer), which the caller stores
/// so a later stale-refund can re-fund the exact same ops with fresh inputs —
/// no need to reverse-engineer which op is the fee out of the funded tx.
///
/// Single-signer only: inscriptions sign `Ed25519`, configs sign a one-index
/// `ChannelMultiSigProof` (`own_key_index`); other ops are rejected.
pub(super) async fn fund_and_sign<Node>(
    node: &Node,
    funding: &FundingConfig,
    signing_key: &Ed25519Key,
    own_key_index: Option<ChannelKeyIndex>,
    builder: MantleTxBuilder,
) -> Result<(SignedMantleTx<Unverified>, MantleTxBuilder), Error>
where
    Node: adapter::Node + Sync,
{
    let pre_fund = builder.clone();
    let response = node
        .fund_tx(WalletFundRequestBody {
            tip: None,
            tx_builder: builder,
            change_public_key: funding.funding_pk,
            funding_public_keys: vec![funding.funding_pk],
            max_tx_fee: funding.max_tx_fee,
            priority_fee: funding.priority_fee,
        })
        .await
        .map_err(|e| Error::Network(format!("funding failed: {e}")))?;
    let funded_tx = response.funded_tx;
    let transfer_proof = response.transfer_proof;
    let new_hash = funded_tx.hash();

    let mut proofs = OpsProofs::empty();
    for op in funded_tx.ops() {
        match op {
            Op::ChannelInscribe(_) => proofs
                .try_push(OpProof::Ed25519Sig(sign_tx(new_hash, signing_key)))
                .map_err(|e| Error::Network(format!("too many operation proofs: {e:?}")))?,
            Op::ChannelConfig(_) => {
                let signatures = own_key_index
                    .map(|idx| {
                        IndexedSignature::new(
                            idx,
                            signing_key.sign_payload(new_hash.as_signing_bytes().as_ref()),
                        )
                    })
                    .into_iter()
                    .collect::<Vec<_>>()
                    .try_into()
                    .map_err(|e| {
                        Error::Network(format!("multi-sig proof assembly failed: {e:?}"))
                    })?;
                let proof = ChannelMultiSigProof::try_new(signatures).map_err(|e| {
                    Error::Network(format!("multi-sig proof assembly failed: {e:?}"))
                })?;
                proofs
                    .try_push(OpProof::ChannelMultiSigProof(proof))
                    .map_err(|e| Error::Network(format!("too many operation proofs: {e:?}")))?;
            }
            Op::Transfer(_) => {}
            other => {
                return Err(Error::Network(format!(
                    "cannot fund/sign tx with unsupported op: {other:?}"
                )));
            }
        }
    }

    let ops_proofs = attach_transfer_proof(&funded_tx, proofs, transfer_proof)?;
    Ok((SignedMantleTx::new(funded_tx, ops_proofs), pre_fund))
}

pub(super) async fn create_inscribe_tx<Node>(
    node: &Node,
    funding: &FundingConfig,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Inscription,
    parent: MsgId,
) -> Result<(SignedMantleTx<Unverified>, MsgId, MantleTxBuilder), Error>
where
    Node: adapter::Node + Sync,
{
    let inscribe_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer: signing_key.public_key(),
    };
    let msg_id = inscribe_op.id();

    let builder = MantleTxBuilder::new()
        .extend_ops(vec![Op::ChannelInscribe(inscribe_op)])
        .map_err(|e| Error::Network(format!("too many ops in transaction: {e:?}")))?;
    let (signed_tx, builder) = fund_and_sign(node, funding, signing_key, None, builder).await?;

    Ok((signed_tx, msg_id, builder))
}

/// Build and fund a `ChannelConfig` transaction.
///
/// `signer` is the sequencer's signing key paired with its index in the
/// channel's *current* (pre-update) `accredited_keys` — that is the list the
/// ledger verifies the signature against. Pass `None` for an unclaimed
/// channel, whose configuration requires no signatures.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the channel config op fields plus the funding context"
)]
pub(super) async fn create_channel_config_tx<Node>(
    node: &Node,
    funding: &FundingConfig,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    own_key_index: Option<ChannelKeyIndex>,
    keys: Keys,
    posting_timeframe: SlotTimeframe,
    posting_timeout: SlotTimeout,
    configuration_threshold: u16,
    transfer_threshold: u16,
) -> Result<(SignedMantleTx<Unverified>, MantleTxBuilder), Error>
where
    Node: adapter::Node + Sync,
{
    let config_op = ChannelConfigOp {
        channel: channel_id,
        keys,
        posting_timeframe,
        posting_timeout,
        configuration_threshold,
        transfer_threshold,
    };

    let builder = MantleTxBuilder::new()
        .extend_ops(vec![Op::ChannelConfig(config_op)])
        .map_err(|e| Error::Network(format!("too many ops in transaction: {e:?}")))?;

    // `own_key_index == None` is an unclaimed channel, whose config needs no
    // signature; `signing_key` is then unused.
    let (signed_tx, builder) =
        fund_and_sign(node, funding, signing_key, own_key_index, builder).await?;

    Ok((signed_tx, builder))
}

pub(super) fn prepare_tx(
    mut ops: Ops,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Inscription,
    parent: MsgId,
) -> (RawMantleTx, MsgId, Ed25519Signature) {
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
    let tx = RawMantleTx(ops);

    let inscription_sig = sign_tx(tx.hash(), signing_key);

    (tx, msg_id, inscription_sig)
}

pub(super) fn sign_tx(tx_hash: TxHash, signing_key: &Ed25519Key) -> Ed25519Signature {
    signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref())
}
