use std::cmp::{Ordering, Reverse};

use lb_core::mantle::{
    Note, Utxo,
    gas::MainnetGasConstants,
    ledger::{Inputs, Outputs},
    ops::{Op, transfer::TransferOp},
    tx_builder::MantleTxBuilder,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_wallet::WalletError;

const ZKSIGN_MAX_INPUTS: usize = 32;

/// Selects the sender-funded portion of a sponsored transaction without yet
/// populating the builder's pending transfer.
///
/// The sponsored path may need to combine sender inputs with fee inputs into
/// multiple transfer chunks, so it cannot eagerly push sender inputs into the
/// builder the way the legacy single-transfer path did.
pub(super) fn select_sender_inputs_and_change(
    tx_builder: MantleTxBuilder,
    output_total: u64,
    mut sender_utxos: Vec<Utxo>,
    sender_change_pk: ZkPublicKey,
) -> Result<(MantleTxBuilder, Vec<Utxo>), WalletError> {
    sender_utxos.sort_by_key(|utxo| Reverse(utxo.note.value));

    let mut sender_input_sum = 0u64;
    let mut sender_inputs = Vec::new();

    for utxo in sender_utxos.iter().copied() {
        sender_input_sum = sender_input_sum.saturating_add(utxo.note.value);
        sender_inputs.push(utxo);
        if sender_input_sum >= output_total {
            break;
        }
    }

    if sender_input_sum < output_total {
        return Err(WalletError::InsufficientFunds {
            available: sender_utxos.iter().map(|utxo| utxo.note.value).sum(),
        });
    }

    let sender_change = sender_input_sum - output_total;
    let builder = if sender_change > 0 {
        tx_builder.add_ledger_output(Note::new(sender_change, sender_change_pk))
    } else {
        tx_builder
    };

    Ok((builder, sender_inputs))
}

/// Builds caller-side transfer chunks for manual wallet-built cucumber
/// transactions.
///
/// This helper exists because `MantleTxBuilder` still exposes only one pending
/// funding transfer, while this scenario needs to build multiple transfer ops
/// once the input count exceeds the `ZkSign` limit.
pub(super) fn build_chunked_funded_tx(
    tx_builder: &MantleTxBuilder,
    funding_utxos: &[Utxo],
    change_pk: ZkPublicKey,
) -> Result<Option<MantleTxBuilder>, WalletError> {
    // This helper expects all funding inputs to arrive through `funding_utxos`.
    // Preloaded builder inputs would be folded into the final pending transfer
    // and break the deterministic <=32-input chunking contract.
    if funding_utxos.len() <= ZKSIGN_MAX_INPUTS || !tx_builder.ledger_inputs().is_empty() {
        return Ok(None);
    }

    let input_sum = funding_utxos
        .iter()
        .map(|utxo| u128::from(utxo.note.value))
        .sum::<u128>();
    let output_sum = pending_transfer_output_sum(tx_builder);

    let chunked_builder = with_transfer_input_chunks(tx_builder, funding_utxos);
    let funding_delta = funding_delta_for_chunked_builder(&chunked_builder, input_sum, output_sum)?;

    match funding_delta.cmp(&0) {
        Ordering::Less => Ok(None),
        Ordering::Equal => Ok(Some(chunked_builder)),
        Ordering::Greater => {
            let builder_with_dummy_change = chunked_builder
                .clone()
                .add_ledger_output(Note::new(0, change_pk));
            let delta_with_change = funding_delta_for_chunked_builder(
                &builder_with_dummy_change,
                input_sum,
                output_sum,
            )?;

            if delta_with_change <= 0 {
                return Ok(None);
            }

            let change = u64::try_from(delta_with_change).expect("Positive delta must fit in u64");
            let tx_with_change = chunked_builder.add_ledger_output(Note::new(change, change_pk));

            assert_eq!(
                funding_delta_for_chunked_builder(
                    &tx_with_change,
                    input_sum,
                    output_sum + u128::from(change),
                )?,
                0
            );

            Ok(Some(tx_with_change))
        }
    }
}

/// Appends transfer chunks with at most 32 inputs, leaving the final chunk in
/// the builder's pending transfer so user outputs and change remain on the last
/// transfer.
///
/// Intermediate chunks intentionally emit no outputs; they only split the
/// funding inputs into proof-sized pieces while preserving deterministic op
/// order.
fn with_transfer_input_chunks(
    tx_builder: &MantleTxBuilder,
    funding_utxos: &[Utxo],
) -> MantleTxBuilder {
    let final_chunk_len = match funding_utxos.len() % ZKSIGN_MAX_INPUTS {
        0 => ZKSIGN_MAX_INPUTS,
        remainder => remainder,
    };
    let split_index = funding_utxos.len() - final_chunk_len;

    let mut builder = tx_builder.clone();
    for chunk in funding_utxos[..split_index].chunks(ZKSIGN_MAX_INPUTS) {
        builder = builder.push_op(Op::Transfer(TransferOp::new(
            Inputs::new(chunk.iter().map(Utxo::id).collect()),
            // Intermediate chunks intentionally emit no outputs so the final
            // transfer can carry the user-visible outputs and any change.
            Outputs::new(vec![]),
        )));
    }

    builder.extend_ledger_inputs(funding_utxos[split_index..].iter().copied())
}

/// Reads the builder-authored pending transfer outputs so caller-side chunking
/// can preserve them on the final transfer chunk.
fn pending_transfer_output_sum(tx_builder: &MantleTxBuilder) -> u128 {
    match tx_builder.clone().build().ops.pop() {
        Some(Op::Transfer(transfer)) => transfer
            .outputs
            .iter()
            .map(|note| u128::from(note.value))
            .sum(),
        _ => 0,
    }
}

/// Recomputes the funding delta for the manual chunked path.
///
/// This intentionally duplicates the builder's balance math because the
/// builder cannot currently express multiple funding transfers. Keep this logic
/// aligned with the wallet crate until the builder has first-class
/// multi-transfer funding support.
fn funding_delta_for_chunked_builder(
    tx_builder: &MantleTxBuilder,
    input_sum: u128,
    output_sum: u128,
) -> Result<i128, WalletError> {
    let gas_cost = u128::from(tx_builder.gas_cost::<MainnetGasConstants>()?.into_inner());
    Ok(i128::try_from(input_sum)
        .expect("Input sum must fit in i128")
        .checked_sub(i128::try_from(output_sum).expect("Output sum must fit in i128"))
        .and_then(|delta| delta.checked_sub(i128::try_from(gas_cost).expect("Gas fits in i128")))
        .expect("Chunked funding delta must fit in i128"))
}
