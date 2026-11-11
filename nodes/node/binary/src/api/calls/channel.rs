use lb_api_common::bodies::channel::{ChannelDepositRequestBody, ChannelDepositResponseBody};
use lb_core::{
    era::ForkDigest,
    header::HeaderId,
    mantle::{
        Note, NoteId, Op, OpProof, SignedOps, Value,
        channel::ChannelState,
        ledger::{BoundedInputs, Inputs, Outputs},
        ops::{
            channel::{
                ChannelId,
                deposit::{DepositOp, Metadata},
            },
            transfer::TransferOp,
        },
        traits::Hashable as _,
        transactions::{MantleTxBuilder, OpProofs, Ops},
    },
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_wallet_service::TipResponse;

use super::{
    Error, Handle,
    cryptarchia::ledger_state,
    mempool,
    time::fork_digest_in_force,
    wallet::{check_max_tx_fee, wallet_api},
};

pub async fn state(handle: &Handle, id: ChannelId) -> Result<ChannelState, Error> {
    ledger_state(handle)
        .await?
        .mantle_ledger()
        .channels()
        .channel_state(&id)
        .cloned()
        .ok_or_else(|| Error::NotFound("Channel not found".into()))
}

/// Funds, signs and submits the deposit in `request`, whose notes the caller
/// picked.
pub async fn deposit(
    handle: &Handle,
    request: ChannelDepositRequestBody,
) -> Result<ChannelDepositResponseBody, Error> {
    let wallet = wallet_api(handle).await?;

    let tx_builder = MantleTxBuilder::new()
        .push_op(Op::ChannelDeposit(request.deposit))
        .map_err(|error| Error::InvalidInput(error.to_string()))?;
    let TipResponse {
        tip,
        response: funded_tx_builder,
    } = wallet
        .fund_tx(
            request.tip,
            tx_builder,
            request.change_public_key,
            request.funding_public_keys,
            0,
        )
        .await?;

    let tx_fee = funded_tx_builder.tx_fee().map_err(Error::internal)?;
    check_max_tx_fee(tx_fee, request.max_tx_fee)?;

    let signed_tx = wallet.sign_tx(Some(tip), funded_tx_builder).await?.response;
    let hash = signed_tx.hash();
    mempool::add_tx(handle, signed_tx).await?;

    Ok(ChannelDepositResponseBody { hash })
}

fn select_notes_covering(
    mut notes: Vec<(NoteId, Value)>,
    amount: Value,
) -> Option<(Vec<NoteId>, Value)> {
    notes.sort_by_key(|(_, value)| std::cmp::Reverse(*value));
    let mut selected = Vec::new();
    let mut total: Value = 0;
    for (id, value) in notes {
        if total >= amount {
            break;
        }
        selected.push(id);
        total = total.saturating_add(value);
    }
    (total >= amount).then_some((selected, total))
}

/// Deposits `amount` out of the notes `funding_public_key` holds, picking the
/// notes itself: one transaction that transfers the exact amount into a fresh
/// note and deposits that note.
pub async fn deposit_amount(
    handle: &Handle,
    fork_digests: &[ForkDigest],
    tip: Option<HeaderId>,
    channel_id: ChannelId,
    funding_public_key: ZkPublicKey,
    amount: Value,
    metadata: Metadata,
) -> Result<ChannelDepositResponseBody, Error> {
    let wallet = wallet_api(handle).await?;

    let notes = wallet
        .get_balance(tip, funding_public_key)
        .await?
        .response
        .ok_or_else(|| Error::NotFound("Unknown funding address.".into()))?
        .notes
        .into_iter()
        .collect();
    let (selected_ids, selected_total) = select_notes_covering(notes, amount)
        .ok_or_else(|| Error::internal("Insufficient funds to cover deposit amount."))?;
    let change = selected_total - amount;

    // Output 0 is the exact deposit note; any remainder goes back to the same
    // key as change.
    let mut outputs = vec![Note::new(amount, funding_public_key)];
    if change > 0 {
        outputs.push(Note::new(change, funding_public_key));
    }
    let transfer = TransferOp {
        inputs: Inputs::try_new(selected_ids)
            .map_err(|error| Error::internal(format!("Invalid transfer inputs: {error:?}")))?,
        outputs: Outputs::try_new(outputs)
            .map_err(|error| Error::internal(format!("Invalid transfer outputs: {error:?}")))?,
    };

    let deposit_note_id = transfer
        .outputs
        .utxo_by_index(0, &transfer)
        .ok_or_else(|| Error::internal("Transfer did not produce a deposit note."))?
        .id();
    let deposit = DepositOp {
        channel_id,
        inputs: BoundedInputs::from(deposit_note_id).into(),
        metadata,
    };

    // Signed with explicit keys rather than `WalletApi::sign_tx`, which
    // resolves each op's input keys from the committed ledger state: the
    // deposit consumes the note this same transaction's transfer creates, so
    // it is not on-chain yet and `sign_tx` would fail with `MissingInputNote`.
    // `funding_public_key` owns every input, so one signature over the
    // transaction hash satisfies both op proofs.
    let tx = Ops::new(
        fork_digest_in_force(handle, fork_digests).await?,
        [Op::Transfer(transfer), Op::ChannelDeposit(deposit)],
    );
    let signature = wallet
        .sign_tx_with_zk(tx.hash(), vec![funding_public_key])
        .await?;
    let op_proofs = OpProofs::from([OpProof::ZkSig(signature.clone()), OpProof::ZkSig(signature)]);
    let signed_tx = SignedOps::from_parts(tx, op_proofs)
        .map_err(|error| Error::internal(format!("Failed to assemble signed tx: {error:?}")))?
        .preverify()
        .map_err(|error| Error::internal(format!("Failed to assemble signed tx: {error:?}")))?;

    let hash = signed_tx.hash();
    mempool::add_tx(handle, signed_tx).await?;

    Ok(ChannelDepositResponseBody { hash })
}
