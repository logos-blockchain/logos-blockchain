use lb_core::mantle::transactions::Ops;

use super::*;

/// Generous fee margin: the mandatory fee varies with input count and change,
/// and an underfunded atomic deposit tx is silently evicted at block assembly.
const ATOMIC_DEPOSIT_FEE_MARGIN: u64 = 10_000;

pub(super) fn build_atomic_deposit_transfer(
    available_utxos: Vec<Utxo>,
    funding_public_key: ZkPublicKey,
    amount: Value,
) -> Result<(TransferOp, Vec<Utxo>), ZoneTestError> {
    let deposit_note = Note::new(amount, funding_public_key);
    let funded_transfer = build_wallet_funded_transfer(
        available_utxos,
        vec![deposit_note],
        funding_public_key,
        ATOMIC_DEPOSIT_FEE_MARGIN,
    )
    .map_err(|error| ZoneTestError::BuildAtomicDeposit {
        message: error.to_string(),
    })?;

    Ok(funded_transfer.into_parts())
}

/// Points the channel deposit at the note created by the funding transfer.
pub(super) fn build_atomic_deposit_op(
    channel_id: ChannelId,
    metadata: Metadata,
    transfer: &TransferOp,
) -> Result<DepositOp, ZoneTestError> {
    let deposit_note_id = transfer
        .outputs
        .utxo_by_index(0, transfer)
        .ok_or_else(|| ZoneTestError::BuildAtomicDeposit {
            message: "transfer did not produce the deposit note".to_owned(),
        })?
        .id();

    Ok(DepositOp {
        channel_id,
        inputs: Inputs::new([deposit_note_id]),
        metadata,
    })
}

/// Self-withdraw of a single note of `amount` back to `funding_public_key`,
/// published with its inscription in one SDK flow. Inputs auto-selected.
pub async fn submit_zone_withdraw(
    client: &SequencerClient,
    _channel_id: ChannelId,
    funding_public_key: ZkPublicKey,
    amount: Value,
    inscription_data: Inscription,
) -> Result<ZoneWithdrawSubmission, ZoneTestError> {
    let (result, _cp) = client
        .publish_atomic_withdraw(
            inscription_data,
            vec![WithdrawArg {
                outputs: Outputs::new([Note::new(amount, funding_public_key)]),
            }],
            WithdrawInputs::Auto,
        )
        .await
        .map_err(|error| ZoneTestError::SubmitWithdraw {
            message: error.to_string(),
        })?;

    let PendingTx::AtomicWithdraw(info) = result.tx else {
        return Err(ZoneTestError::SubmitWithdraw {
            message: "publish_atomic_withdraw returned a non-AtomicWithdraw publish result"
                .to_owned(),
        });
    };
    let withdraw = info
        .withdraws
        .first()
        .ok_or_else(|| ZoneTestError::SubmitWithdraw {
            message: "atomic withdraw bundle had no withdraw ops".to_owned(),
        })?
        .op
        .clone();

    Ok(ZoneWithdrawSubmission {
        withdraw,
        publish: PublishResult {
            tx: PendingTx::AtomicWithdraw(info),
        },
    })
}

/// Carries every withdraw op the SDK produced (one per `WithdrawArg`, in
/// submission order) so a multi-withdraw scenario can match each by its
/// outputs.
pub struct ZoneAtomicWithdrawSubmission {
    pub withdraws: Vec<ChannelWithdrawOp>,
    pub publish: PublishResult,
}

/// `outputs_per_arg` carries one entry per `WithdrawArg`; each inner `Vec`
/// becomes that arg's `Outputs` (one `Note::new(amount, funding_pk)` per listed
/// amount), exercising the SDK API at full width.
pub async fn publish_atomic_zone_withdraw(
    client: &SequencerClient,
    funding_public_key: ZkPublicKey,
    outputs_per_arg: Vec<Vec<Value>>,
    inscription_data: Inscription,
    _deadline: PublishDeadline,
) -> Result<ZoneAtomicWithdrawSubmission, ZoneTestError> {
    if outputs_per_arg.is_empty() {
        return Err(ZoneTestError::SubmitWithdraw {
            message: "publish_atomic_zone_withdraw requires at least one withdraw arg".to_owned(),
        });
    }
    let withdraw_args: Vec<WithdrawArg> = outputs_per_arg
        .iter()
        .map(|amounts| {
            Ok::<WithdrawArg, ZoneTestError>(WithdrawArg {
                outputs: Outputs::try_new(
                    amounts
                        .iter()
                        .map(|amount| Note::new(*amount, funding_public_key))
                        .collect::<Vec<_>>(),
                )?,
            })
        })
        .collect::<Result<Vec<_>, ZoneTestError>>()?;

    let (result, _cp) = client
        .publish_atomic_withdraw(inscription_data, withdraw_args, WithdrawInputs::Auto)
        .await
        .map_err(|error| ZoneTestError::SubmitWithdraw {
            message: error.to_string(),
        })?;

    let PendingTx::AtomicWithdraw(info) = result.tx else {
        return Err(ZoneTestError::SubmitWithdraw {
            message: "publish_atomic_withdraw returned a non-AtomicWithdraw publish result"
                .to_owned(),
        });
    };
    if info.withdraws.is_empty() {
        return Err(ZoneTestError::SubmitWithdraw {
            message: "atomic withdraw bundle had no withdraw ops".to_owned(),
        });
    }
    Ok(ZoneAtomicWithdrawSubmission {
        withdraws: info.withdraws.iter().map(|w| w.op.clone()).collect(),
        publish: PublishResult {
            tx: PendingTx::AtomicWithdraw(info),
        },
    })
}

pub(super) async fn sign_tx_zk(
    node_url: &Url,
    ops: &Ops,
    public_keys: Vec<ZkPublicKey>,
) -> Result<ZkSignature, ZoneTestError> {
    let request_url =
        node_url
            .join("wallet/sign/zk")
            .map_err(|error| ZoneTestError::SignTransaction {
                message: error.to_string(),
            })?;
    let response: WalletSignTxZkResponseBody = CommonHttpClient::new(None)
        .post(
            request_url,
            &WalletSignTxZkRequestBody {
                tx_hash: ops.hash(),
                pks: ZkPublicKeys::try_from(public_keys)?,
            },
        )
        .await
        .map_err(|error| ZoneTestError::SignTransaction {
            message: error.to_string(),
        })?;

    Ok(response.sig)
}
