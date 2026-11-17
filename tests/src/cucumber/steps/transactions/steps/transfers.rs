use lb_zktransfer::MAX_OUTPUTS;

use super::{
    CucumberWorld, Duration, HashSet, Step, StepError, StepResult, TARGET, WalletSendReadiness,
    WalletUtxos, create_and_submit_transaction,
    create_and_submit_transaction_hashes_with_utxo_cache, info, wait_wallet_send_ready, warn, when,
};
use crate::cucumber::{utils::lgo_to_units, wallet::submissions::wait_for_transactions_inclusion};

#[when(expr = "I do a coin split for {string} of {int} UTXOs valued at {int} LGO tokens each")]
async fn step_do_coin_split(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    number_of_outputs: usize,
    output_value: u64,
) -> StepResult {
    let output_value = lgo_to_units(output_value);
    let wallet = world.resolve_wallet(&wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    let self_pk = wallet.public_key().inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;
    let receivers = vec![(self_pk, output_value); number_of_outputs];
    // A ZkTransfer creates at most `MAX_OUTPUTS` notes, two of them the changes
    // of the sender and of a fee sponsor, so the split takes one transaction
    // per chunk. Each chunk waits for funds the earlier ones did not encumber,
    // the change of the last one at worst.
    for chunk in receivers.chunks(MAX_OUTPUTS - 2) {
        let mut available_utxos = WalletUtxos::new();
        let best_node_info = wait_wallet_send_ready(
            world,
            &step.value,
            &wallet_name,
            180,
            chunk.len() as u64 * output_value,
            WalletSendReadiness::TotalValueOnly,
            &mut available_utxos,
            &HashSet::new(),
        )
        .await?;

        let tx_hash_hex = create_and_submit_transaction(
            world,
            &step.value,
            &wallet_name,
            chunk,
            Some(&best_node_info),
            Some(&mut available_utxos),
        )
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

        info!(
            target: TARGET,
            "Submitted coin split transaction for `{wallet_name}/{}`, outputs: {}, \
            value: {output_value}, tx hash: {tx_hash_hex}",
            wallet.node_name,
            chunk.len()
        );
    }

    Ok(())
}

/// Tops up a node's funding wallet from a user wallet with `note_count`
/// notes of `note_value` LGO each, in chunked transactions.
///
/// Funding a transaction reserves one wallet note until the transaction is
/// mined, so a funding wallet supports at most `note count` concurrent
/// in-flight funded transactions — and its single `10_000` genesis note cannot
/// pay the fees of scenarios that publish many messages at non-zero gas
/// prices. This mirrors the workflow of a real node operator provisioning
/// their sequencer's funding wallet.
#[when(
    expr = "wallet {string} sends {int} notes of {int} LGO to node {string} funding wallet as {string}"
)]
async fn step_topup_node_funding_wallet(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    note_count: usize,
    note_value: u64,
    node_name: String,
    transaction_alias: String,
) -> StepResult {
    let note_units = lgo_to_units(note_value);
    let funding_wallet = world.funding_wallet(&node_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;
    let funding_pk = funding_wallet.public_key().inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    let receivers = vec![(funding_pk, note_units); note_count];
    // As for a coin split, the top-up takes one transaction per chunk of at
    // most `MAX_OUTPUTS - 2` notes, each waiting for funds the earlier ones did
    // not encumber.
    let mut tx_hashes = Vec::new();
    for chunk in receivers.chunks(MAX_OUTPUTS - 2) {
        let mut available_utxos = WalletUtxos::new();
        let best_node_info = wait_wallet_send_ready(
            world,
            &step.value,
            &wallet_name,
            180,
            chunk.len() as u64 * note_units,
            WalletSendReadiness::TotalValueOnly,
            &mut available_utxos,
            &HashSet::new(),
        )
        .await?;

        tx_hashes.extend(
            create_and_submit_transaction_hashes_with_utxo_cache(
                world,
                &step.value,
                &wallet_name,
                chunk,
                Some(&best_node_info),
                Some(&mut available_utxos),
            )
            .await
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{}` error: {e}", step.value);
            })?,
        );
    }
    // The alias names the last chunk, so its inclusion covers the whole top-up
    // once the earlier chunks are included.
    let (tx_hash, earlier_tx_hashes) =
        tx_hashes
            .split_last()
            .ok_or_else(|| StepError::LogicalError {
                message: "funding wallet top-up produced no transaction".to_owned(),
            })?;
    if !earlier_tx_hashes.is_empty() {
        let sender_node_name = world.resolve_wallet(&wallet_name)?.node_name;
        let sender_node_client = &world
            .nodes_info
            .get(&sender_node_name)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Node '{sender_node_name}' not found in world state"),
            })?
            .client;
        wait_for_transactions_inclusion(
            sender_node_client,
            earlier_tx_hashes,
            Duration::from_mins(3),
        )
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;
    }
    world.remember_submitted_transaction(transaction_alias.clone(), *tx_hash);

    info!(
        target: TARGET,
        "Submitted funding wallet top-up `{transaction_alias}`: {note_count} notes of \
        {note_value} LGO from `{wallet_name}` to `{}`",
        funding_wallet.wallet_name,
    );

    Ok(())
}
