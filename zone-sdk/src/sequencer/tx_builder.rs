use lb_core::{
    mantle::{
        MantleTransaction, TxHash,
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

/// Assemble the ops for a transaction, funding it from the node's wallet.
///
/// The node appends a fee transfer (paid from `funding.funding_pk`, change
/// back to it) and returns the proof for that transfer; all other ops must
/// be proven by the caller over the funded transaction hash.
pub(super) async fn fund_ops<Node>(
    node: &Node,
    funding: &FundingConfig,
    ops: Vec<Op>,
) -> Result<(RawMantleTx, Option<OpProof>), Error>
where
    Node: adapter::Node + Sync,
{
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
            // The public request field is a percentage of the final
            // mandatory fee, not an absolute fee amount.
            priority_fee_percent: funding.priority_fee_percent,
        })
        .await
        .map_err(|e| Error::Network(format!("funding failed: {e}")))?;

    Ok((response.funded_tx, response.transfer_proof))
}

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

/// Build per-op proofs for a single-signer atomic channel bundle
/// (`publish_atomic_withdraw`'s `[inscribe, transfer, withdraw]` or
/// `publish_pin_deposit`'s `[inscribe, transfer]`). The same
/// single-signer `ChannelMultiSigProof` is reused for every `ChannelTransfer`
/// and `ChannelWithdraw` op (all sign the same tx hash with the same key), the
/// inscription op carries an `Ed25519Sig` proof and the fee transfer — when
/// the transaction was funded — carries the wallet's proof.
pub(super) fn build_atomic_bundle_ops_proofs(
    tx: &impl MantleTx,
    own_key_index: ChannelKeyIndex,
    own_sig: Ed25519Signature,
    transfer_proof: Option<&OpProof>,
) -> Result<OpsProofs, Error> {
    let channel_proof =
        ChannelMultiSigProof::try_new([IndexedSignature::new(own_key_index, own_sig)].into())
            .map_err(|e| Error::Network(format!("multi-sig proof assembly failed: {e:?}")))?;
    let mut ops_proofs = OpsProofs::empty();
    for op in tx.ops() {
        match op {
            // Channel transfers (recipient/change or re-created deposit notes)
            // and withdraws (releasing recipient notes) are single-signer
            // multi-sig proofs over the same funded tx hash.
            Op::ChannelTransfer(_) | Op::ChannelWithdraw(_) => {
                ops_proofs
                    .try_push(OpProof::ChannelMultiSigProof(channel_proof.clone()))
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
                    "unexpected op in atomic channel bundle: {op:?}"
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
    funding: &FundingConfig,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Inscription,
    parent: MsgId,
) -> Result<(MantleTransaction<Unverified>, MsgId), Error>
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
        [OpProof::Ed25519Sig(signature)].into(),
        transfer_proof,
    )?;

    let signed_tx = MantleTransaction::new(inscribe_tx, ops_proofs);

    Ok((signed_tx, msg_id))
}

/// Build and fund a `ChannelConfig` transaction, returning the funded raw
/// transaction and the fee-transfer proof (present whenever funding appended a
/// fee transfer).
///
/// This is the signature-agnostic half of building a config tx: it produces the
/// exact bytes the accredited keys must sign, without committing to how many
/// signatures will be collected. [`assemble_channel_config_tx`] completes the
/// tx once the signatures are in hand.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the channel config op fields plus the funding context"
)]
pub(super) async fn build_and_fund_config<Node>(
    node: &Node,
    funding: &FundingConfig,
    channel_id: ChannelId,
    parent: MsgId,
    keys: Keys,
    posting_timeframe: SlotTimeframe,
    posting_timeout: SlotTimeout,
    configuration_threshold: u16,
    transfer_threshold: u16,
) -> Result<(RawMantleTx, Option<OpProof>), Error>
where
    Node: adapter::Node + Sync,
{
    let config_op = ChannelConfigOp {
        channel: channel_id,
        parent,
        keys,
        posting_timeframe,
        posting_timeout,
        configuration_threshold,
        transfer_threshold,
    };

    fund_ops(node, funding, vec![Op::ChannelConfig(config_op)]).await
}

/// Assemble a fully-signed channel-config tx from a funded config tx, the
/// fee-transfer proof, and the collected accredited-key signatures.
///
/// `signatures` must be indexed against the channel's *current* (pre-update)
/// `accredited_keys` — the list the ledger verifies against — and strictly
/// ascending by index. Pass an empty vec to configure an unclaimed channel,
/// whose configuration requires no signatures (the empty multi-sig proof).
pub(super) fn assemble_channel_config_tx(
    config_tx: RawMantleTx,
    transfer_proof: Option<OpProof>,
    signatures: Vec<IndexedSignature>,
) -> Result<MantleTransaction<Unverified>, Error> {
    let signatures = signatures
        .try_into()
        .map_err(|e| Error::Network(format!("too many channel-config signatures: {e:?}")))?;
    let proof = ChannelMultiSigProof::try_new(signatures)
        .map_err(|e| Error::Network(format!("multi-sig proof assembly failed: {e:?}")))?;
    let ops_proofs = attach_transfer_proof(
        &config_tx,
        [OpProof::ChannelMultiSigProof(proof)].into(),
        transfer_proof,
    )?;

    Ok(MantleTransaction::new(config_tx, ops_proofs))
}

/// Build, fund, and single-signer-sign a `ChannelConfig` transaction.
///
/// `signer` is the sequencer's signing key paired with its index in the
/// channel's *current* (pre-update) `accredited_keys` — that is the list the
/// ledger verifies the signature against. Pass `None` for an unclaimed
/// channel, whose configuration requires no signatures.
///
/// Multi-sig callers build the funded tx with [`build_and_fund_config`],
/// collect the signatures out-of-band, then finish with
/// [`assemble_channel_config_tx`].
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the channel config op fields plus the funding context"
)]
pub(super) async fn create_channel_config_tx<Node>(
    node: &Node,
    funding: &FundingConfig,
    channel_id: ChannelId,
    parent: MsgId,
    signer: Option<(ChannelKeyIndex, &Ed25519Key)>,
    keys: Keys,
    posting_timeframe: SlotTimeframe,
    posting_timeout: SlotTimeout,
    configuration_threshold: u16,
    transfer_threshold: u16,
) -> Result<MantleTransaction<Unverified>, Error>
where
    Node: adapter::Node + Sync,
{
    let (config_tx, transfer_proof) = build_and_fund_config(
        node,
        funding,
        channel_id,
        parent,
        keys,
        posting_timeframe,
        posting_timeout,
        configuration_threshold,
        transfer_threshold,
    )
    .await?;

    let signatures = signer
        .map(|(index, key)| IndexedSignature::new(index, sign_tx(config_tx.hash(), key)))
        .into_iter()
        .collect::<Vec<_>>();

    assemble_channel_config_tx(config_tx, transfer_proof, signatures)
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

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::test_support::{MockNode, funding_config};

    #[tokio::test]
    async fn funding_path_passes_priority_fee_as_a_percentage() {
        let (priority_fees_tx, mut priority_fees_rx) = mpsc::channel(1);
        let node = MockNode {
            funding_priority_fees: Some(priority_fees_tx),
            ..MockNode::default()
        };
        let funding = funding_config();

        fund_ops(&node, &funding, Vec::new()).await.unwrap();

        assert_eq!(priority_fees_rx.recv().await, Some(12));
    }
}
