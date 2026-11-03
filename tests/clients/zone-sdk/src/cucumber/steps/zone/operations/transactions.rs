use lb_core::mantle::{SignedOps, ledger::verification_mode::StandardMode, transactions::OpProofs};

use super::*;

pub(super) async fn build_funded_custom_tx(
    node_client: &NodeHttpClient,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    funding_pk: ZkPublicKey,
    payloads: &[Inscription],
    mut parent: MsgId,
) -> Result<(SignedOps<Unverified, StandardMode>, MsgId), ZoneTestError> {
    let signer = signing_key.public_key().into_unverified();
    let mut tx_builder = MantleTxBuilder::new();
    for payload in payloads {
        let op = InscriptionOp {
            channel_id,
            inscription: payload.clone(),
            parent,
            signer,
            steps: ChannelSteps::default(),
        };
        parent = op.id();
        tx_builder = tx_builder
            .push_op(Op::ChannelInscribe(op))
            .map_err(|error| ZoneTestError::BuildCustomTx {
                message: format!("too many ops: {error}"),
            })?;
    }

    let response = node_client
        .fund_tx(WalletFundRequestBody {
            tip: None,
            priority_fee_percent: 0,
            tx_builder,
            change_public_key: funding_pk,
            funding_public_keys: vec![funding_pk],
            max_tx_fee: GasCost::new(u64::MAX),
        })
        .await
        .map_err(|error| ZoneTestError::SubmitCustomTx {
            message: format!("funding failed: {error}"),
        })?;

    // Funding appends the fee transfer as the last op; every inscription is
    // proven by the sequencer key over the funded tx hash.
    let funded_tx = response.funded_tx;
    let signature = signing_key.sign_payload(funded_tx.hash().as_signing_bytes());
    let mut op_proofs = OpProofs::try_from(vec![
        OpProof::ChannelInscribe(
            ChannelInscribeProof::without_steps(signature)
        );
        payloads.len()
    ])
    .map_err(|error| ZoneTestError::BuildCustomTx {
        message: format!("too many operation proofs: {error:?}"),
    })?;

    if let Some(proof) = response.transfer_proof {
        op_proofs
            .try_push(proof)
            .map_err(|error| ZoneTestError::BuildCustomTx {
                message: format!("too many operation proofs: {error:?}"),
            })?;
    }

    let signed_ops = SignedOps::from_parts(funded_tx, op_proofs).map_err(|error| {
        ZoneTestError::BuildCustomTx {
            message: format!("signed ops assembly failed: {error}"),
        }
    })?;

    Ok((signed_ops, parent))
}
