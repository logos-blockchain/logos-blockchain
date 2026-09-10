use lb_core::{
    mantle::{
        SignedOps, TxHash,
        channel::{ChannelState, SlotTimeframe, SlotTimeout},
        ledger::verification_mode::StandardMode,
        ops::{
            Op, OpProof, OpRef,
            channel::{
                ChannelId, ChannelKeyIndex, MsgId,
                config::{ChannelConfigOp, Keys},
                inscribe::{Inscription, InscriptionOp},
            },
        },
        traits::{Hashable as _, MantleTx},
        transactions::{MantleTxBuilder, OpProofs, Ops, states::Unverified},
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};
use lb_http_api_common::bodies::wallet::fund::WalletFundRequestBody;
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519PublicKey, Ed25519Signature};

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
) -> Result<(Ops, Option<OpProof>), Error>
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

/// Build per-op proofs for an atomic channel bundle: transfer/withdraw ops all
/// share `channel_proof`, the inscription carries `inscribe_sig`, and the fee
/// transfer (when funded) carries `transfer_proof`.
pub(super) fn assemble_atomic_bundle_ops_proofs(
    tx: &impl MantleTx,
    inscribe_sig: Ed25519Signature,
    channel_proof: &ChannelMultiSigProof,
    transfer_proof: Option<&OpProof>,
) -> Result<OpProofs, Error> {
    let mut ops_proofs = OpProofs::empty();
    for op in tx.op_refs() {
        match op {
            OpRef::ChannelTransfer(_) | OpRef::ChannelWithdraw(_) => {
                ops_proofs
                    .try_push(OpProof::ChannelMultiSigProof(channel_proof.clone()))
                    .map_err(|e| Error::Network(format!("too many operation proofs: {e:?}")))?;
            }
            OpRef::ChannelInscribe(_) => ops_proofs
                .try_push(OpProof::Ed25519Sig(inscribe_sig))
                .map_err(|e| Error::Network(format!("too many operation proofs: {e:?}")))?,
            OpRef::Transfer(_) => match transfer_proof {
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

/// Assemble a fully-signed atomic bundle. `signatures` must be indexed against
/// the channel's `accredited_keys`, strictly ascending, exactly
/// `transfer_threshold` of them.
pub(super) fn assemble_atomic_bundle_tx(
    tx: Ops,
    inscribe_sig: Ed25519Signature,
    signatures: Vec<IndexedSignature>,
    transfer_proof: Option<&OpProof>,
) -> Result<SignedOps<Unverified, StandardMode>, Error> {
    let signatures = signatures
        .try_into()
        .map_err(|e| Error::Network(format!("too many atomic-bundle signatures: {e:?}")))?;
    let channel_proof = ChannelMultiSigProof::try_new(signatures)
        .map_err(|e| Error::Network(format!("multi-sig proof assembly failed: {e:?}")))?;
    let ops_proofs =
        assemble_atomic_bundle_ops_proofs(&tx, inscribe_sig, &channel_proof, transfer_proof)?;
    SignedOps::from_parts(tx, ops_proofs)
        .map_err(|error| Error::Network(format!("failed to assemble atomic bundle tx: {error:?}")))
}

/// Every way a prepared bundle has gone stale since prepare, as human-readable
/// reasons (empty when it can still land as built): the channel config moved
/// (keys/threshold the signatures were collected under), or the inscription
/// parent is no longer the one prepare would pick now.
pub(super) fn stale_bundle_reasons(
    prepared_keys: &[Ed25519PublicKey],
    prepared_threshold: ChannelKeyIndex,
    live_keys: &[Ed25519PublicKey],
    live_threshold: ChannelKeyIndex,
    prepared_parent: MsgId,
    live_parent: MsgId,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if prepared_threshold != live_threshold {
        reasons.push(format!(
            "transfer_threshold changed {prepared_threshold} -> {live_threshold}"
        ));
    }
    if prepared_keys != live_keys {
        reasons.push(format!(
            "accredited keys changed ({} -> {} keys)",
            prepared_keys.len(),
            live_keys.len()
        ));
    }
    if prepared_parent != live_parent {
        reasons.push(format!(
            "inscription parent {prepared_parent:?} is no longer the publish tip {live_parent:?}"
        ));
    }
    reasons
}

/// Check externally-collected multi-sig `signatures` against `accredited_keys`
/// / `threshold` over `sign_payload`, mirroring the ledger's
/// `verify_channel_multi_sig` so an unlandable proof fails at submit instead of
/// parking in the pending set (only chain inclusion evicts it): exactly
/// `threshold` signatures, strictly ascending (hence unique) indices that
/// resolve to an accredited key, and each signature valid for the key at its
/// index. Reports every problem found in one [`Error::InvalidMultiSig`].
pub(super) fn validate_multi_sig(
    accredited_keys: &[Ed25519PublicKey],
    threshold: ChannelKeyIndex,
    sign_payload: &[u8],
    signatures: &[IndexedSignature],
) -> Result<(), Error> {
    let mut problems = Vec::new();
    if signatures.len() != usize::from(threshold) {
        problems.push(format!(
            "signature count {} does not match transfer_threshold {threshold}",
            signatures.len()
        ));
    }
    let mut previous: Option<ChannelKeyIndex> = None;
    for signature in signatures {
        let index = signature.channel_key_index;
        if let Some(prev) = previous
            && prev >= index
        {
            problems.push(format!(
                "indices must be strictly ascending, got {index} after {prev}"
            ));
        }
        previous = Some(index);
        match accredited_keys.get(usize::from(index)) {
            None => problems.push(format!(
                "index {index} is outside the {} accredited keys",
                accredited_keys.len()
            )),
            Some(key) => {
                if key.verify(sign_payload, &signature.signature).is_err() {
                    problems.push(format!(
                        "signature at index {index} does not verify against the accredited key"
                    ));
                }
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::InvalidMultiSig(problems.join("; ")))
    }
}

/// Sign a prepared multi-sig payload with `signing_key`, returning its
/// [`IndexedSignature`] against `accredited_keys`. Errors if `signing_key` is
/// not in `accredited_keys`.
pub fn sign_prepared(
    signing_key: &Ed25519Key,
    accredited_keys: &[Ed25519PublicKey],
    sign_payload: &[u8],
) -> Result<IndexedSignature, Error> {
    let own_pk = signing_key.public_key();
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
) -> Result<(SignedOps<Unverified, StandardMode>, MsgId), Error>
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

    let signed_tx = SignedOps::from_parts(inscribe_tx, ops_proofs)
        .unwrap_or_else(|error| panic!("Node returned an unprovable transaction: {error}"));

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
) -> Result<(Ops, Option<OpProof>), Error>
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
    config_tx: Ops,
    transfer_proof: Option<OpProof>,
    signatures: Vec<IndexedSignature>,
) -> Result<SignedOps<Unverified, StandardMode>, Error> {
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
    keys: Keys,
    posting_timeframe: SlotTimeframe,
    posting_timeout: SlotTimeout,
    configuration_threshold: u16,
    transfer_threshold: u16,
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
) -> (Ops, MsgId, Ed25519Signature) {
    let inscription_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer: signing_key.public_key(),
    };
    let msg_id = inscription_op.id();
    // TODO: Return `Error` in case there's too many ops already.
    ops.try_push(Op::ChannelInscribe(inscription_op)).unwrap();

    // TODO: fund tx (ops)
    let inscription_sig = sign_tx(ops.hash(), signing_key);

    (ops, msg_id, inscription_sig)
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

    #[test]
    fn sign_prepared_indexes_by_position_and_verifies() {
        let keys: Vec<Ed25519Key> = (1u8..=3)
            .map(|b| Ed25519Key::from_bytes(&[b; 32]))
            .collect();
        let accredited: Vec<Ed25519PublicKey> = keys.iter().map(Ed25519Key::public_key).collect();
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
            Ed25519Key::from_bytes(&[1; 32]).public_key(),
            Ed25519Key::from_bytes(&[2; 32]).public_key(),
        ];
        let outsider = Ed25519Key::from_bytes(&[9; 32]);
        assert!(sign_prepared(&outsider, &accredited, b"payload").is_err());
    }

    #[test]
    fn sign_prepared_empty_accredited_is_rejected() {
        let key = Ed25519Key::from_bytes(&[1; 32]);
        assert!(sign_prepared(&key, &[], b"payload").is_err());
    }

    /// Three accredited keys and a 2-of-3 signature set over `payload`.
    fn multi_sig_fixture() -> (
        Vec<Ed25519Key>,
        Vec<Ed25519PublicKey>,
        Vec<IndexedSignature>,
    ) {
        let keys: Vec<Ed25519Key> = (1u8..=3)
            .map(|b| Ed25519Key::from_bytes(&[b; 32]))
            .collect();
        let accredited: Vec<Ed25519PublicKey> = keys.iter().map(Ed25519Key::public_key).collect();
        let sigs = vec![
            sign_prepared(&keys[0], &accredited, PAYLOAD).unwrap(),
            sign_prepared(&keys[2], &accredited, PAYLOAD).unwrap(),
        ];
        (keys, accredited, sigs)
    }

    const PAYLOAD: &[u8] = b"atomic bundle sign payload";

    fn invalid_multi_sig_message(result: Result<(), Error>) -> String {
        match result {
            Err(Error::InvalidMultiSig(msg)) => msg,
            other => panic!("expected InvalidMultiSig, got {other:?}"),
        }
    }

    #[test]
    fn validate_multi_sig_accepts_threshold_signatures_in_order() {
        let (_, accredited, sigs) = multi_sig_fixture();
        validate_multi_sig(&accredited, 2, PAYLOAD, &sigs).expect("2-of-3 in order verifies");
    }

    #[test]
    fn validate_multi_sig_rejects_count_mismatch() {
        let (_, accredited, sigs) = multi_sig_fixture();
        // Too few for the threshold.
        let msg = invalid_multi_sig_message(validate_multi_sig(&accredited, 3, PAYLOAD, &sigs));
        assert!(
            msg.contains("count 2 does not match transfer_threshold 3"),
            "{msg}"
        );
        // Too many: the ledger requires exactly `threshold`, not at least.
        let msg = invalid_multi_sig_message(validate_multi_sig(&accredited, 1, PAYLOAD, &sigs));
        assert!(
            msg.contains("count 2 does not match transfer_threshold 1"),
            "{msg}"
        );
    }

    #[test]
    fn validate_multi_sig_rejects_unordered_or_duplicate_indices() {
        let (_, accredited, mut sigs) = multi_sig_fixture();
        sigs.reverse();
        let msg = invalid_multi_sig_message(validate_multi_sig(&accredited, 2, PAYLOAD, &sigs));
        assert!(msg.contains("strictly ascending, got 0 after 2"), "{msg}");

        let dup = vec![sigs[1].clone(), sigs[1].clone()];
        let msg = invalid_multi_sig_message(validate_multi_sig(&accredited, 2, PAYLOAD, &dup));
        assert!(msg.contains("strictly ascending, got 0 after 0"), "{msg}");
    }

    #[test]
    fn validate_multi_sig_rejects_out_of_range_index() {
        let (keys, accredited, _) = multi_sig_fixture();
        let sigs = vec![
            sign_prepared(&keys[0], &accredited, PAYLOAD).unwrap(),
            IndexedSignature::new(7, keys[1].sign_payload(PAYLOAD)),
        ];
        let msg = invalid_multi_sig_message(validate_multi_sig(&accredited, 2, PAYLOAD, &sigs));
        assert!(
            msg.contains("index 7 is outside the 3 accredited keys"),
            "{msg}"
        );
    }

    #[test]
    fn validate_multi_sig_rejects_signature_over_other_payload() {
        let (keys, accredited, _) = multi_sig_fixture();
        // Peer signed a stale/different prepared bundle.
        let sigs = vec![
            sign_prepared(&keys[0], &accredited, PAYLOAD).unwrap(),
            sign_prepared(&keys[2], &accredited, b"some other bundle").unwrap(),
        ];
        let msg = invalid_multi_sig_message(validate_multi_sig(&accredited, 2, PAYLOAD, &sigs));
        assert!(msg.contains("index 2 does not verify"), "{msg}");
    }

    #[test]
    fn validate_multi_sig_reports_every_problem_at_once() {
        let (keys, accredited, _) = multi_sig_fixture();
        // Wrong count (3 for threshold 2), a bad signature, and an
        // out-of-range index — all in one set.
        let sigs = vec![
            sign_prepared(&keys[0], &accredited, PAYLOAD).unwrap(),
            sign_prepared(&keys[1], &accredited, b"other").unwrap(),
            IndexedSignature::new(9, keys[2].sign_payload(PAYLOAD)),
        ];
        let msg = invalid_multi_sig_message(validate_multi_sig(&accredited, 2, PAYLOAD, &sigs));
        assert!(
            msg.contains("count 3 does not match transfer_threshold 2"),
            "{msg}"
        );
        assert!(msg.contains("index 1 does not verify"), "{msg}");
        assert!(msg.contains("index 9 is outside"), "{msg}");
    }

    #[test]
    fn stale_bundle_reasons_covers_config_and_parent() {
        let (_, prepared, _) = multi_sig_fixture();
        let parent = MsgId::root();

        assert!(stale_bundle_reasons(&prepared, 2, &prepared, 2, parent, parent).is_empty());

        // Threshold moved 2 -> 3 while signatures were being collected.
        let reasons = stale_bundle_reasons(&prepared, 2, &prepared, 3, parent, parent);
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("transfer_threshold changed 2 -> 3"));

        // Key at index 2 rotated; same threshold.
        let mut rotated = prepared.clone();
        rotated[2] = Ed25519Key::from_bytes(&[9; 32]).public_key();
        let reasons = stale_bundle_reasons(&prepared, 2, &rotated, 2, parent, parent);
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("accredited keys changed"));

        // A peer inscribed at our parent slot: the publish tip moved.
        let moved = MsgId::from([7u8; 32]);
        let reasons = stale_bundle_reasons(&prepared, 2, &prepared, 2, parent, moved);
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("no longer the publish tip"));

        // Everything at once is reported together.
        let reasons = stale_bundle_reasons(&prepared, 2, &rotated, 3, parent, moved);
        assert_eq!(reasons.len(), 3);
    }
}
