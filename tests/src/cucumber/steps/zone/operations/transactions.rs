use lb_core::mantle::{MantleTransaction, transactions::OpProofs};

use super::*;

/// Builds a regular channel deposit for an existing funding note with the
/// exact deposit value.
pub fn build_zone_deposit(
    available_utxos: Vec<Utxo>,
    channel_id: ChannelId,
    amount: Value,
    metadata: Metadata,
) -> Result<ZoneDeposit, ZoneTestError> {
    let note = available_utxos
        .into_iter()
        .find(|utxo| utxo.note.value == amount)
        .ok_or(ZoneTestError::MissingExactFundingNote { value: amount })?;

    let deposit = DepositOp {
        channel_id,
        inputs: Inputs::new([note.id()]),
        metadata,
    };
    let reserved_inputs = vec![note];
    let channel_notes = recreated_channel_notes(&deposit, &reserved_inputs);
    Ok(ZoneDeposit {
        deposit,
        reserved_inputs,
        channel_notes,
    })
}

/// Build a deposit that consumes one wallet note per listed value, in order.
/// A deposit re-creates its inputs 1:1 as channel notes, so this yields a
/// multi-input deposit whose recreated notes carry those exact per-note values
/// — used to cover the channel wallet's per-note tracking.
pub fn build_zone_deposit_from_values(
    available_utxos: Vec<Utxo>,
    channel_id: ChannelId,
    input_values: &[Value],
    metadata: Metadata,
) -> Result<ZoneDeposit, ZoneTestError> {
    let mut remaining = available_utxos;
    let mut reserved_inputs = Vec::new();
    for &value in input_values {
        let index = remaining
            .iter()
            .position(|utxo| utxo.note.value == value)
            .ok_or(ZoneTestError::MissingExactFundingNote { value })?;
        reserved_inputs.push(remaining.remove(index));
    }

    let input_ids: Vec<_> = reserved_inputs.iter().map(Utxo::id).collect();
    let deposit = DepositOp {
        channel_id,
        inputs: Inputs::try_new(input_ids).map_err(|error| ZoneTestError::SubmitDeposit {
            message: format!("deposit input set exceeds bound: {error:?}"),
        })?,
        metadata,
    };
    let channel_notes = recreated_channel_notes(&deposit, &reserved_inputs);
    Ok(ZoneDeposit {
        deposit,
        reserved_inputs,
        channel_notes,
    })
}

/// Generous cap on channel transaction fees at genesis gas prices; actual
/// fees are a few hundred gas units for these small transactions.
const MAX_ZONE_DEPOSIT_TX_FEE: u64 = 10_000;

/// Submits a regular channel deposit through the node wallet API.
pub async fn submit_zone_deposit(
    node_url: &Url,
    deposit: &DepositOp,
    funding_public_key: ZkPublicKey,
) -> Result<InscriptionId, ZoneTestError> {
    let body = ChannelDepositRequestBody {
        tip: None,
        deposit: deposit.clone(),
        change_public_key: funding_public_key,
        funding_public_keys: vec![funding_public_key],
        max_tx_fee: MAX_ZONE_DEPOSIT_TX_FEE.into(),
    };

    let request_url =
        node_url
            .join("/channel/deposit")
            .map_err(|error| ZoneTestError::SubmitDeposit {
                message: error.to_string(),
            })?;

    let response: ChannelDepositResponseBody = CommonHttpClient::new(None)
        .post(request_url, &body)
        .await
        .map_err(|error| ZoneTestError::SubmitDeposit {
            message: error.to_string(),
        })?;

    Ok(response.hash)
}

/// Splits one channel note into `dust_count` value-1 dust notes via a raw,
/// node-funded `ChannelTransfer`, submitted straight to the node mempool (no
/// zone-sdk involvement — mirrors how deposits are submitted).
///
/// The transfer is value-preserving, so the input note's value must equal
/// `dust_count` (every output is value 1). The channel authorizes the transfer
/// with the sequencer's accredited key at index 0 (single-signer channel,
/// `transfer_threshold == 1`); the node appends and proves the fee transfer.
pub async fn submit_zone_channel_split(
    node_url: &Url,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    funding_pk: ZkPublicKey,
    input_note: Utxo,
    dust_count: usize,
) -> Result<InscriptionId, ZoneTestError> {
    let input_value = input_note.note.value;
    if input_value != dust_count as u64 {
        return Err(ZoneTestError::SplitTransfer {
            message: format!(
                "channel note value {input_value} must equal dust count {dust_count} \
                 (each dust note is value 1)"
            ),
        });
    }

    let outputs =
        Outputs::try_new(vec![Note::new(1, funding_pk); dust_count]).map_err(|error| {
            ZoneTestError::SplitTransfer {
                message: format!("dust outputs exceed bound: {error:?}"),
            }
        })?;
    let transfer = ChannelTransferOp {
        channel_id,
        inputs: Inputs::new([input_note.id()]),
        outputs,
    };

    let tx_builder = MantleTxBuilder::new()
        .push_op(Op::ChannelTransfer(transfer))
        .map_err(|error| ZoneTestError::SplitTransfer {
            message: format!("too many ops: {error}"),
        })?;

    let node = NodeHttpClient::from_url(node_url.clone());
    let response = node
        .fund_tx(WalletFundRequestBody {
            tip: None,
            priority_fee_percent: 0,
            tx_builder,
            change_public_key: funding_pk,
            funding_public_keys: vec![funding_pk],
            max_tx_fee: GasCost::new(u64::MAX),
        })
        .await
        .map_err(|error| ZoneTestError::SplitTransfer {
            message: format!("funding failed: {error}"),
        })?;

    // The channel multi-sig proves the transfer over the funded tx hash; the
    // funding appends its own fee transfer proof as the last op.
    let funded_tx = response.funded_tx;
    let tx_hash = funded_tx.hash();
    let signature = signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref());
    let proof = ChannelMultiSigProof::try_new([IndexedSignature::new(0, signature)].into())
        .map_err(|error| ZoneTestError::SplitTransfer {
            message: format!("multi-sig proof assembly failed: {error:?}"),
        })?;
    let mut ops_proofs = OpProofs::from([OpProof::ChannelMultiSigProof(proof)]);
    if let Some(transfer_proof) = response.transfer_proof {
        ops_proofs
            .try_push(transfer_proof)
            .map_err(|error| ZoneTestError::SplitTransfer {
                message: format!("too many operation proofs: {error:?}"),
            })?;
    }

    let signed_tx = MantleTransaction::new(funded_tx, ops_proofs);
    node.submit_transaction(&signed_tx)
        .await
        .map_err(|error| ZoneTestError::SplitTransfer {
            message: format!("submit failed: {error}"),
        })?;

    Ok(tx_hash)
}

/// Builds and submits a single transaction that both creates the deposit note
/// and publishes the zone inscription that consumes it.
pub async fn submit_atomic_zone_deposit(
    node_url: &Url,
    client: &SequencerClient,
    request: AtomicZoneDepositRequest,
) -> Result<AtomicZoneDepositSubmission, ZoneTestError> {
    let AtomicZoneDepositRequest {
        channel_id,
        funding_public_key,
        available_utxos,
        amount,
        metadata,
        inscription_data,
    } = request;
    let (transfer, reserved_inputs) =
        build_atomic_deposit_transfer(available_utxos, funding_public_key, amount)?;
    let deposit = build_atomic_deposit_op(channel_id, metadata, &transfer)?;

    let (tx, msg_id, sequencer_sig) = client
        .prepare_tx(
            [Op::Transfer(transfer), Op::ChannelDeposit(deposit.clone())].into(),
            inscription_data,
        )
        .await
        .map_err(|error| ZoneTestError::BuildAtomicDeposit {
            message: error.to_string(),
        })?;

    let user_sig = sign_tx_zk(node_url, &tx, vec![funding_public_key]).await?;
    let op_proofs = OpProofs::from([
        OpProof::ZkSig(user_sig.clone()),
        OpProof::ZkSig(user_sig),
        OpProof::Ed25519Sig(sequencer_sig),
    ]);
    let signed_tx = MantleTransaction::new(tx, op_proofs);

    let (result, _cp) = client
        .submit_signed_tx(signed_tx, msg_id)
        .await
        .map_err(|error| ZoneTestError::SubmitAtomicDeposit {
            message: error.to_string(),
        })?;

    Ok(AtomicZoneDepositSubmission {
        deposit,
        publish: result,
        reserved_inputs,
    })
}

pub(super) async fn build_funded_custom_tx(
    node_client: &NodeHttpClient,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    funding_pk: ZkPublicKey,
    payloads: &[Inscription],
    mut parent: MsgId,
) -> Result<(MantleTransaction<Unverified>, MsgId), ZoneTestError> {
    let signer = signing_key.public_key();
    let mut tx_builder = MantleTxBuilder::new();
    for payload in payloads {
        let op = InscriptionOp {
            channel_id,
            inscription: payload.clone(),
            parent,
            signer,
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
    let signature = signing_key.sign_payload(funded_tx.hash().as_signing_bytes().as_ref());
    let mut op_proofs =
        OpProofs::new_unchecked(vec![OpProof::Ed25519Sig(signature); payloads.len()]);
    if let Some(proof) = response.transfer_proof {
        op_proofs
            .try_push(proof)
            .map_err(|error| ZoneTestError::BuildCustomTx {
                message: format!("too many operation proofs: {error:?}"),
            })?;
    }
    let signed_tx = MantleTransaction::new(funded_tx, op_proofs);

    Ok((signed_tx, parent))
}
