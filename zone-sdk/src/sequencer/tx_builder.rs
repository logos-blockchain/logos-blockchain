use std::num::NonZero;

use lb_core::{
    mantle::{
        SignedOps, TxHash,
        channel::{SlotTimeframe, SlotTimeout},
        ledger::verification_mode::StandardMode,
        ops::{
            ChannelInscribeProof, Op, OpProof, OpRef,
            channel::{
                ChannelId, ChannelKeyIndex, MsgId, VerifiedChannelKeys,
                config::ChannelConfigOp,
                inscribe::{ChannelSteps, Inscription, InscriptionOp},
            },
        },
        traits::{Hashable as _, MantleTx},
        transactions::{MantleTxBuilder, OpProofs, Ops, states::Unverified},
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature, IndexedSignatures},
};
use lb_http_api_common::bodies::wallet::fund::WalletFundRequestBody;
use lb_key_management_system_service::keys::{
    Ed25519Key, Ed25519Signature, UnverifiedEd25519PublicKey,
};

use super::types::{Error, FundingConfig};
use crate::adapter;

/// Assemble the ops for a transaction, funding it from the node's wallet.
///
/// The node appends a fee transfer (paid from `funding.funding_pk`, change
/// back to it) and returns the proof for that transfer; all other ops must
/// be proven by the caller over the funded transaction hash. Also returns
/// the pre-funding builder, the channel ops alone, so the caller can
/// re-fund the same ops later with fresh fee inputs.
pub(super) async fn fund_ops<Node>(
    node: &Node,
    funding: &FundingConfig,
    ops: Vec<Op>,
) -> Result<(Ops, Option<OpProof>, MantleTxBuilder), Error>
where
    Node: adapter::Node + Sync,
{
    let tx_builder = MantleTxBuilder::new()
        .extend_ops(ops)
        .map_err(|e| Error::Network(format!("too many ops in transaction: {e:?}")))?;
    let (funded_tx, transfer_proof) = fund_builder(node, funding, tx_builder.clone()).await?;
    Ok((funded_tx, transfer_proof, tx_builder))
}

/// Fund the channel ops in `tx_builder` from the node's wallet. Funding the
/// same builder again draws fresh fee inputs: the same ops under a new hash.
pub(super) async fn fund_builder<Node>(
    node: &Node,
    funding: &FundingConfig,
    tx_builder: MantleTxBuilder,
) -> Result<(Ops, Option<OpProof>), Error>
where
    Node: adapter::Node + Sync,
{
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
    mut channel_proofs: OpProofs,
    transfer_proof: Option<OpProof>,
) -> Result<OpProofs, Error> {
    let transfer_count = tx
        .op_refs()
        .into_iter()
        .filter(|op| matches!(op, OpRef::Transfer(_)))
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

/// Sign a prepared multi-sig payload with `signing_key`, returning its
/// [`IndexedSignature`] against `accredited_keys`. Errors if `signing_key` is
/// not in `accredited_keys`.
pub fn sign_prepared(
    signing_key: &Ed25519Key,
    accredited_keys: &[UnverifiedEd25519PublicKey],
    sign_payload: &[u8],
) -> Result<IndexedSignature, Error> {
    let own_pk = signing_key.public_key().into_unverified();
    let index = accredited_keys
        .iter()
        .position(|key| *key == own_pk)
        .ok_or_else(|| Error::Network("key not in the prepared accredited set".into()))?;
    let index = ChannelKeyIndex::try_from(index)
        .map_err(|_| Error::Network("accredited key index exceeds u16".into()))?;
    Ok(IndexedSignature::new(
        index,
        signing_key.sign_payload(sign_payload),
    ))
}

pub(super) async fn create_inscribe_tx<Node>(
    node: &Node,
    funding: &FundingConfig,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Inscription,
    parent: MsgId,
) -> Result<(SignedOps<Unverified, StandardMode>, MsgId, MantleTxBuilder), Error>
where
    Node: adapter::Node + Sync,
{
    let signer = signing_key.public_key().into_unverified();

    let inscribe_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer,
        steps: ChannelSteps::default(),
    };
    let msg_id = inscribe_op.id();

    let (inscribe_tx, transfer_proof, pre_fund) =
        fund_ops(node, funding, vec![Op::ChannelInscribe(inscribe_op)]).await?;

    let tx_hash = inscribe_tx.hash();
    let signature = sign_tx(tx_hash, signing_key);
    let ops_proofs = attach_transfer_proof(
        &inscribe_tx,
        [OpProof::ChannelInscribe(
            ChannelInscribeProof::without_steps(signature),
        )]
        .into(),
        transfer_proof,
    )?;

    let signed_tx = SignedOps::from_parts(inscribe_tx, ops_proofs)
        .unwrap_or_else(|error| panic!("Node returned an unprovable transaction: {error}"));

    Ok((signed_tx, msg_id, pre_fund))
}

/// Sign a funded tx this sequencer built itself. A plain inscription carries
/// the sequencer's `Ed25519Sig`.
pub(super) fn sign_own_tx(
    tx: Ops,
    transfer_proof: Option<OpProof>,
    signing_key: &Ed25519Key,
) -> Result<SignedOps<Unverified, StandardMode>, Error> {
    let own_sig = sign_tx(tx.hash(), signing_key);
    let ops_proofs = attach_transfer_proof(
        &tx,
        [OpProof::ChannelInscribe(
            ChannelInscribeProof::without_steps(own_sig),
        )]
        .into(),
        transfer_proof,
    )?;
    SignedOps::from_parts(tx, ops_proofs)
        .map_err(|error| Error::Network(format!("failed to assemble signed tx: {error:?}")))
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
    keys: VerifiedChannelKeys,
    posting_timeframe: SlotTimeframe,
    posting_timeout: SlotTimeout,
    configuration_threshold: u16,
) -> Result<(Ops, Option<OpProof>), Error>
where
    Node: adapter::Node + Sync,
{
    // Reject a config the ledger would reject before funding it.
    let key_count = keys.len();
    let non_zero = |threshold: u16, name: &str| {
        NonZero::new(threshold).ok_or_else(|| Error::Network(format!("{name} must be at least 1")))
    };
    let config_op = ChannelConfigOp::new(
        channel_id,
        parent,
        keys,
        posting_timeframe,
        posting_timeout,
        non_zero(configuration_threshold, "configuration_threshold")?,
    )
    .map_err(|_| {
        Error::Network(format!(
            "configuration_threshold {configuration_threshold} exceeds the {key_count} accredited keys"
        ))
    })?;

    let (config_tx, transfer_proof, _) =
        fund_ops(node, funding, vec![Op::ChannelConfig(config_op)]).await?;
    Ok((config_tx, transfer_proof))
}

/// Assemble a fully-signed channel-config tx from a funded config tx, the
/// fee-transfer proof, and the collected accredited-key signatures.
///
/// `signatures` must be indexed against the channel's *current* (pre-update)
/// `accredited_keys` — the list the ledger verifies against — with at most one
/// signature per index, in any order. Pass an empty vec to configure an
/// unclaimed channel, whose configuration requires no signatures (the empty
/// multi-sig proof).
pub(super) fn assemble_channel_config_tx(
    config_tx: Ops,
    transfer_proof: Option<OpProof>,
    signatures: IndexedSignatures,
) -> Result<SignedOps<Unverified, StandardMode>, Error> {
    let proof = ChannelMultiSigProof::new(signatures);
    let ops_proofs = attach_transfer_proof(
        &config_tx,
        [OpProof::ChannelMultiSigProof(proof)].into(),
        transfer_proof,
    )?;

    SignedOps::from_parts(config_tx, ops_proofs)
        .map_err(|error| Error::Network(format!("failed to assemble channel config tx: {error:?}")))
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
    keys: VerifiedChannelKeys,
    posting_timeframe: SlotTimeframe,
    posting_timeout: SlotTimeout,
    configuration_threshold: u16,
) -> Result<SignedOps<Unverified, StandardMode>, Error>
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
    )
    .await?;

    let signatures = signer.map_or(IndexedSignatures::empty(), |(index, key)| {
        (index, sign_tx(config_tx.hash(), key)).into()
    });

    assemble_channel_config_tx(config_tx, transfer_proof, signatures)
}

pub(super) fn prepare_tx(
    mut ops: Ops,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Inscription,
    parent: MsgId,
) -> (Ops, MsgId, Ed25519Signature) {
    let inscription_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer: signing_key.public_key().into_unverified(),
        steps: ChannelSteps::default(),
    };
    let msg_id = inscription_op.id();
    // TODO: Return `Error` in case there's too many ops already.
    ops.try_push(Op::ChannelInscribe(inscription_op)).unwrap();

    // TODO: fund tx (ops)
    let inscription_sig = sign_tx(ops.hash(), signing_key);

    (ops, msg_id, inscription_sig)
}

pub(super) fn sign_tx(tx_hash: TxHash, signing_key: &Ed25519Key) -> Ed25519Signature {
    signing_key.sign_payload(tx_hash.as_signing_bytes())
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

    #[test]
    fn sign_prepared_indexes_by_position_and_verifies() {
        let keys: Vec<Ed25519Key> = (1u8..=3)
            .map(|b| Ed25519Key::from_bytes(&[b; 32]))
            .collect();
        let accredited: Vec<UnverifiedEd25519PublicKey> = keys
            .iter()
            .map(|k| k.public_key().into_unverified())
            .collect();
        let payload = b"channel config sign payload";

        // Signing with the middle key indexes at its position, and the
        // signature verifies against that key over the payload.
        let signed = sign_prepared(&keys[1], &accredited, payload).expect("signer is accredited");
        assert_eq!(signed.channel_key_index, 1);
        accredited[1]
            .verify(payload, &signed.signature)
            .expect("signature verifies against the signer's public key");
    }

    #[test]
    fn sign_prepared_rejects_unaccredited_key() {
        let accredited = vec![
            Ed25519Key::from_bytes(&[1; 32])
                .public_key()
                .into_unverified(),
            Ed25519Key::from_bytes(&[2; 32])
                .public_key()
                .into_unverified(),
        ];
        let outsider = Ed25519Key::from_bytes(&[9; 32]);
        assert!(sign_prepared(&outsider, &accredited, b"payload").is_err());
    }

    #[test]
    fn sign_prepared_empty_accredited_is_rejected() {
        let key = Ed25519Key::from_bytes(&[1; 32]);
        assert!(sign_prepared(&key, &[], b"payload").is_err());
    }
}
