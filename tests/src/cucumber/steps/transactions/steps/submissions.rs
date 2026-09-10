use super::{
    CucumberWorld, HashSet, Step, StepResult, TARGET, WalletSendReadiness, WalletUtxos,
    create_and_submit_transaction, info, submit_funded_transfer_transaction,
    submit_invalid_transfer_transaction, submit_stateless_invalid_transfer_transaction, then,
    transaction_is_not_included_in_seconds, transaction_is_rejected_during_preverification,
    wait_wallet_send_ready, warn, when,
};

#[when(
    expr = "I send {int} transactions of {int} LGO each from wallet {string} to wallet {string}"
)]
async fn step_send_multiple_transactions_to_single_wallet(
    world: &mut CucumberWorld,
    step: &Step,
    number_of_transactions: usize,
    output_value: u64,
    sender_wallet_name: String,
    receiver_wallet_name: String,
) -> StepResult {
    let sender_wallet = world.resolve_wallet(&sender_wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;
    let receiver = world.resolve_recipient(&receiver_wallet_name)?;
    let receiver_wallet_pk = receiver.public_key;

    let mut available_utxos = WalletUtxos::new();
    let best_node_info = wait_wallet_send_ready(
        world,
        &step.value,
        &sender_wallet_name,
        180,
        number_of_transactions as u64 * output_value,
        WalletSendReadiness::TotalValueOnly,
        &mut available_utxos,
        &HashSet::new(),
    )
    .await?;

    for _ in 0..number_of_transactions {
        let tx_hash_hex = create_and_submit_transaction(
            world,
            &step.value,
            &sender_wallet_name,
            &[(receiver_wallet_pk, output_value)],
            Some(&best_node_info),
            Some(&mut available_utxos),
        )
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

        info!(
            target: TARGET,
            "Sent normal transaction from `{sender_wallet_name}/{}` to {}, \
            value: {output_value}, tx hash: {tx_hash_hex}",
            sender_wallet.node_name,
            receiver.label
        );
    }

    Ok(())
}

#[when(expr = "I submit invalid transfer transaction {string} to node {string}")]
async fn step_submit_invalid_transfer_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    node_name: String,
) -> StepResult {
    submit_invalid_transfer_transaction(world, &step.value, transaction_alias, node_name).await
}

#[when(expr = "I submit a stateless-invalid transfer transaction {string} to node {string}")]
async fn step_submit_stateless_invalid_transfer_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    node_name: String,
) -> StepResult {
    submit_stateless_invalid_transfer_transaction(world, &step.value, transaction_alias, node_name)
        .await
}

#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Cucumber step captures are always owned `String`s, even when the step only needs to borrow them"
)]
#[then(expr = "transaction {string} is rejected during preverification")]
fn step_transaction_is_rejected_during_preverification(
    world: &mut CucumberWorld,
    transaction_alias: String,
) -> StepResult {
    transaction_is_rejected_during_preverification(world, &transaction_alias)
}

#[when(
    expr = "I submit funded transfer transaction {string} of {int} LGO from wallet {string} to wallet {string}"
)]
async fn step_submit_funded_transfer_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    amount: u64,
    sender_wallet_name: String,
    receiver_wallet_name: String,
) -> StepResult {
    submit_funded_transfer_transaction(
        world,
        &step.value,
        transaction_alias,
        amount,
        sender_wallet_name,
        receiver_wallet_name,
    )
    .await
}

#[when(expr = "transaction {string} is not included in {int} seconds")]
#[then(expr = "transaction {string} is not included in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_transaction_is_not_included_in_seconds(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    transaction_is_not_included_in_seconds(world, &step.value, transaction_alias, timeout_seconds)
        .await
}

#[when(
    expr = "I send one transaction with {int} outputs of {int} LGO each from wallet {string} to wallet {string}"
)]
async fn step_send_single_transaction_multiple_outputs_to_single_wallet(
    world: &mut CucumberWorld,
    step: &Step,
    number_of_outputs: usize,
    output_value: u64,
    sender_wallet_name: String,
    receiver_wallet_name: String,
) -> StepResult {
    let sender_wallet = world.resolve_wallet(&sender_wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;
    let receiver = world.resolve_recipient(&receiver_wallet_name)?;
    let receiver_wallet_pk = receiver.public_key;

    let receivers = vec![(receiver_wallet_pk, output_value); number_of_outputs];
    let tx_hash_hex = create_and_submit_transaction(
        world,
        &step.value,
        &sender_wallet_name,
        &receivers,
        None,
        None,
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    info!(
        target: TARGET,
        "Sent normal transaction from `{sender_wallet_name}/{}` to {}, \
        number_of_outputs: {number_of_outputs}, value: {output_value}, tx hash: {tx_hash_hex}",
        sender_wallet.node_name,
        receiver.label
    );

    Ok(())
}
