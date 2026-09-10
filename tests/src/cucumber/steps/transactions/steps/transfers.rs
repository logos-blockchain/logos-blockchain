use super::{
    CucumberWorld, HashSet, Step, StepError, StepResult, TARGET, WalletSendReadiness, WalletUtxos,
    create_and_submit_transaction, create_and_submit_transaction_hashes_with_utxo_cache, info,
    wait_wallet_send_ready, warn, when,
};

#[when(expr = "I do a coin split for {string} of {int} UTXOs valued at {int} LGO tokens each")]
async fn step_do_coin_split(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    number_of_outputs: usize,
    output_value: u64,
) -> StepResult {
    let wallet = world.resolve_wallet(&wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    let mut available_utxos = WalletUtxos::new();
    let best_node_info = wait_wallet_send_ready(
        world,
        &step.value,
        &wallet_name,
        180,
        number_of_outputs as u64 * output_value,
        WalletSendReadiness::TotalValueOnly,
        &mut available_utxos,
        &HashSet::new(),
    )
    .await?;

    let self_pk = wallet.public_key().inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;
    let receivers = vec![(self_pk, output_value); number_of_outputs];
    let tx_hash_hex = create_and_submit_transaction(
        world,
        &step.value,
        &wallet_name,
        &receivers,
        Some(&best_node_info),
        Some(&mut available_utxos),
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    info!(
        target: TARGET,
        "Submitted coin split transaction for `{wallet_name}/{}`, outputs: {number_of_outputs}, \
        value: {output_value}, tx hash: {tx_hash_hex}",
        wallet.node_name
    );

    Ok(())
}

/// Tops up a node's funding wallet from a user wallet with `note_count`
/// notes of `note_value` LGO each, in a single transaction.
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
    let funding_wallet = world.funding_wallet(&node_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;
    let funding_pk = funding_wallet.public_key().inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    let mut available_utxos = WalletUtxos::new();
    let best_node_info = wait_wallet_send_ready(
        world,
        &step.value,
        &wallet_name,
        180,
        note_count as u64 * note_value,
        WalletSendReadiness::TotalValueOnly,
        &mut available_utxos,
        &HashSet::new(),
    )
    .await?;

    let receivers = vec![(funding_pk, note_value); note_count];
    let tx_hashes = create_and_submit_transaction_hashes_with_utxo_cache(
        world,
        &step.value,
        &wallet_name,
        &receivers,
        Some(&best_node_info),
        Some(&mut available_utxos),
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;
    let tx_hash = tx_hashes
        .first()
        .copied()
        .ok_or_else(|| StepError::LogicalError {
            message: "funding wallet top-up produced no transaction".to_owned(),
        })?;
    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);

    info!(
        target: TARGET,
        "Submitted funding wallet top-up `{transaction_alias}`: {note_count} notes of \
        {note_value} LGO from `{wallet_name}` to `{}`",
        funding_wallet.wallet_name,
    );

    Ok(())
}
