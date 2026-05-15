//! Applies wallet funding decisions to Mantle transaction builders.

use std::cmp::Ordering;

use lb_core::mantle::{
    Note, Op, Utxo,
    gas::MainnetGasConstants,
    ledger::{Inputs, Outputs},
    ops::transfer::TransferOp,
    tx_builder::MantleTxBuilder,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_mmr::MerkleMountainRange;
use lb_wallet::{WalletError, WalletState};
use rpds::{HashTrieMapSync, HashTrieSetSync};

use super::intent::WalletTransactionIntent;
use crate::common::wallet::{
    WalletFundingOutcome, WalletFundingPlan, WalletFundingResources, WalletFundingSource,
    WalletFundingUtxos, WalletSelectedInputs,
};

pub fn fund_builder_from_wallet_source(
    source: &WalletFundingSource,
    tx_builder: &MantleTxBuilder,
) -> Result<MantleTxBuilder, WalletError> {
    wallet_state_from_utxos(source.available_utxos().to_vec()).fund_tx::<MainnetGasConstants>(
        tx_builder,
        source.public_key(),
        [source.public_key()],
    )
}

pub fn wallet_state_from_utxos(utxos: Vec<Utxo>) -> WalletState {
    let mut utxo_map = HashTrieMapSync::new_sync();
    let mut pk_index = HashTrieMapSync::new_sync();

    for utxo in utxos {
        let note_id = utxo.id();
        let pk = utxo.note.pk;
        utxo_map = utxo_map.insert(note_id, utxo);

        let note_set = pk_index
            .get(&pk)
            .cloned()
            .unwrap_or_else(HashTrieSetSync::new_sync)
            .insert(note_id);
        pk_index = pk_index.insert(pk, note_set);
    }

    WalletState {
        utxos: utxo_map,
        pk_index,
        locked_notes: HashTrieSetSync::new_sync(),
        epoch: 0.into(),
        vouchers: MerkleMountainRange::new(),
        voucher_paths: HashTrieMapSync::new_sync(),
        voucher_paths_snapshot: HashTrieMapSync::new_sync(),
    }
}

pub(super) fn fund_wallet_transaction(
    intent: WalletTransactionIntent,
    resources: WalletFundingResources,
) -> Result<MantleTxBuilder, WalletError> {
    let (sender, fee_sponsor) = resources.into_parts();
    let (tx_builder, sender_output_total) = intent.into_parts();

    match fee_sponsor {
        None => fund_unsponsored_wallet_transaction(&tx_builder, sender.into_funding_utxos()),
        Some(fee_sponsor) => fund_sponsored_wallet_transaction(
            tx_builder,
            sender_output_total,
            fee_sponsor.into_funding_utxos(),
            sender.into_funding_utxos(),
        ),
    }
}

fn fund_unsponsored_wallet_transaction(
    tx_builder: &MantleTxBuilder,
    sender: WalletFundingUtxos,
) -> Result<MantleTxBuilder, WalletError> {
    let sender_change_pk = sender.change_pk();

    fund_builder_from_plan(
        tx_builder,
        sender_change_pk,
        &WalletFundingPlan::largest_first(sender.into_available_utxos(), Vec::new()),
    )
}

fn fund_sponsored_wallet_transaction(
    tx_builder: MantleTxBuilder,
    output_total: u64,
    fee_sponsor: WalletFundingUtxos,
    sender: WalletFundingUtxos,
) -> Result<MantleTxBuilder, WalletError> {
    let sender_change_pk = sender.change_pk();
    let fee_sponsor_change_pk = fee_sponsor.change_pk();
    let sender_inputs =
        WalletSelectedInputs::largest_first_covering(sender.into_available_utxos(), output_total)?;
    let builder_with_sender_outputs = add_sender_change_output(
        tx_builder,
        sender_change_pk,
        output_total,
        sender_inputs.total(),
    );

    fund_builder_from_plan(
        &builder_with_sender_outputs,
        fee_sponsor_change_pk,
        &WalletFundingPlan::smallest_first(
            fee_sponsor.into_available_utxos(),
            sender_inputs.into_utxos(),
        ),
    )
}

fn add_sender_change_output(
    tx_builder: MantleTxBuilder,
    change_pk: ZkPublicKey,
    output_total: u64,
    input_total: u64,
) -> MantleTxBuilder {
    let change = input_total - output_total;
    if change > 0 {
        tx_builder.add_ledger_output(Note::new(change, change_pk))
    } else {
        tx_builder
    }
}

fn fund_builder_from_plan(
    tx_builder: &MantleTxBuilder,
    change_pk: ZkPublicKey,
    plan: &WalletFundingPlan,
) -> Result<MantleTxBuilder, WalletError> {
    plan.fund_with(|selected_inputs| {
        evaluate_funding_inputs(tx_builder, selected_inputs, change_pk)
    })
}

fn evaluate_funding_inputs(
    tx_builder: &MantleTxBuilder,
    selected_inputs: &[Utxo],
    change_pk: ZkPublicKey,
) -> Result<WalletFundingOutcome<MantleTxBuilder>, WalletError> {
    if selected_inputs.is_empty() && tx_builder.ledger_inputs().is_empty() {
        return Ok(WalletFundingOutcome::NeedsMoreInputs);
    }

    if selected_inputs.len() <= super::signing::ZKSIGN_MAX_INPUTS {
        return evaluate_standard_funding_inputs(tx_builder, selected_inputs, change_pk);
    }

    Ok(
        build_chunked_funded_tx(tx_builder, selected_inputs, change_pk)?.map_or(
            WalletFundingOutcome::NeedsMoreInputs,
            WalletFundingOutcome::Funded,
        ),
    )
}

fn evaluate_standard_funding_inputs(
    tx_builder: &MantleTxBuilder,
    selected_inputs: &[Utxo],
    change_pk: ZkPublicKey,
) -> Result<WalletFundingOutcome<MantleTxBuilder>, WalletError> {
    let funded_builder = tx_builder
        .clone()
        .extend_ledger_inputs(selected_inputs.iter().copied());

    match funded_builder
        .funding_delta::<MainnetGasConstants>()?
        .cmp(&0)
    {
        Ordering::Less => Ok(WalletFundingOutcome::NeedsMoreInputs),
        Ordering::Equal => Ok(WalletFundingOutcome::Funded(funded_builder)),
        Ordering::Greater => Ok(funded_builder
            .return_change::<MainnetGasConstants>(change_pk)?
            .map_or(
                WalletFundingOutcome::NeedsMoreInputs,
                WalletFundingOutcome::Funded,
            )),
    }
}

fn build_chunked_funded_tx(
    tx_builder: &MantleTxBuilder,
    funding_utxos: &[Utxo],
    change_pk: ZkPublicKey,
) -> Result<Option<MantleTxBuilder>, WalletError> {
    if funding_utxos.len() <= super::signing::ZKSIGN_MAX_INPUTS
        || !tx_builder.ledger_inputs().is_empty()
    {
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
        Ordering::Greater => add_chunked_change_output(
            chunked_builder,
            change_pk,
            input_sum,
            output_sum,
            funding_delta,
        ),
    }
}

fn add_chunked_change_output(
    chunked_builder: MantleTxBuilder,
    change_pk: ZkPublicKey,
    input_sum: u128,
    output_sum: u128,
    funding_delta: i128,
) -> Result<Option<MantleTxBuilder>, WalletError> {
    let builder_with_dummy_change = chunked_builder
        .clone()
        .add_ledger_output(Note::new(0, change_pk));
    let delta_with_change =
        funding_delta_for_chunked_builder(&builder_with_dummy_change, input_sum, output_sum)?;

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
    debug_assert!(funding_delta > 0);

    Ok(Some(tx_with_change))
}

fn with_transfer_input_chunks(
    tx_builder: &MantleTxBuilder,
    funding_utxos: &[Utxo],
) -> MantleTxBuilder {
    let final_chunk_len = match funding_utxos.len() % super::signing::ZKSIGN_MAX_INPUTS {
        0 => super::signing::ZKSIGN_MAX_INPUTS,
        remainder => remainder,
    };
    let split_index = funding_utxos.len() - final_chunk_len;

    let mut builder = tx_builder.clone();
    for chunk in funding_utxos[..split_index].chunks(super::signing::ZKSIGN_MAX_INPUTS) {
        builder = builder.push_op(Op::Transfer(TransferOp::new(
            Inputs::new(chunk.iter().map(Utxo::id).collect()),
            Outputs::new(vec![]),
        )));
    }

    builder.extend_ledger_inputs(funding_utxos[split_index..].iter().copied())
}

fn pending_transfer_output_sum(tx_builder: &MantleTxBuilder) -> u128 {
    match tx_builder.clone().build().0.iter().last() {
        Some(Op::Transfer(transfer)) => transfer
            .outputs
            .iter()
            .map(|note| u128::from(note.value))
            .sum(),
        _ => 0,
    }
}

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

#[cfg(test)]
mod tests {
    use lb_core::mantle::ops::channel::inscribe::Inscription;
    use lb_key_management_system_service::keys::Ed25519Key;
    use lb_testing_framework::configs::wallet::WalletAccount;

    use super::*;
    use crate::common::mantle_inscription::{
        build_inscription_tx_builder, channel_id_for_payload_size,
    };

    #[test]
    fn zero_cost_wallet_transaction_still_uses_funding_input() {
        let payload_size = 1024;
        let signing_key = Ed25519Key::from_bytes(&[0u8; 32]);
        let tx_builder = build_inscription_tx_builder(
            Inscription::new_unchecked(vec![0xab; payload_size]),
            &signing_key,
            channel_id_for_payload_size(payload_size),
            None,
        );
        assert_eq!(
            tx_builder
                .funding_delta::<MainnetGasConstants>()
                .expect("zero-gas inscription funding delta should calculate"),
            0
        );

        let account = WalletAccount::deterministic(1, 2_000_000, false)
            .expect("test wallet account should build");
        let funding_utxo = Utxo::new([7u8; 32], 0, Note::new(2_000_000, account.public_key()));
        let funding_source = WalletFundingSource::new(account, vec![funding_utxo]);

        let funded_builder =
            fund_unsponsored_wallet_transaction(&tx_builder, funding_source.into_funding_utxos())
                .expect("zero-cost wallet transaction should still select a funding input");

        assert_eq!(funded_builder.ledger_inputs(), &[funding_utxo]);
        assert_eq!(
            funded_builder
                .funding_delta::<MainnetGasConstants>()
                .expect("funded inscription delta should calculate"),
            0
        );

        let funded_tx = funded_builder.build();
        let Some(Op::Transfer(transfer)) = funded_tx.ops().last() else {
            panic!("wallet funding should leave a transfer op at the end");
        };
        assert!(!transfer.inputs.is_empty());
    }
}
