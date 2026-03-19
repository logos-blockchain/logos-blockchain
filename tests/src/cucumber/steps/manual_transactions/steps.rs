use cucumber::{gherkin::Step, given, then, when};
use tracing::{info, warn};

use crate::{
    cucumber::{
        error::{StepError, StepResult},
        steps::{
            TARGET,
            manual_transactions::{
                command_file_utils::perform_manual_step_control,
                utils,
                utils::{
                    WalletStateType, create_and_submit_transaction,
                    wait_for_wallet_or_encumbered_state,
                },
            },
        },
        utils::resolve_literal_or_env,
        world::{CucumberWorld, WalletInfo},
    },
    non_zero,
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

    let self_pk = wallet.public_key().inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;
    let receivers = vec![(self_pk, output_value); number_of_outputs];
    let tx_hash_hex = create_and_submit_transaction(world, &step.value, &wallet_name, &receivers)
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

#[when(expr = "wallet {string} has {int} or more outputs in {int} seconds")]
#[then(expr = "wallet {string} has {int} or more outputs in {int} seconds")]
async fn step_wallet_has_at_least_coins(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    min_coin_count: usize,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_or_encumbered_state(
        world,
        &step.value,
        wallet_name,
        Some(&min_coin_count),
        None,
        None,
        None,
        time_out_seconds,
        WalletStateType::OnChain,
    )
    .await
}

#[when(expr = "wallet {string} has {int} or less outputs in {int} seconds")]
#[then(expr = "wallet {string} has {int} or less outputs in {int} seconds")]
async fn step_wallet_has_at_most_coins(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    max_coin_count: usize,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_or_encumbered_state(
        world,
        &step.value,
        wallet_name,
        None,
        Some(&max_coin_count),
        None,
        None,
        time_out_seconds,
        WalletStateType::OnChain,
    )
    .await
}

#[when(expr = "wallet {string} has {int} or less encumbered outputs in {int} seconds")]
#[then(expr = "wallet {string} has {int} or less encumbered outputs in {int} seconds")]
async fn step_wallet_has_at_most_encumbered_coins(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    max_coin_count: usize,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_or_encumbered_state(
        world,
        &step.value,
        wallet_name,
        None,
        Some(&max_coin_count),
        None,
        None,
        time_out_seconds,
        WalletStateType::Encumbered,
    )
    .await
}

#[when(expr = "wallet {string} has {int} or more LGO in {int} seconds")]
#[then(expr = "wallet {string} has {int} or more LGO in {int} seconds")]
async fn step_wallet_has_at_least_value(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    min_token_value: u64,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_or_encumbered_state(
        world,
        &step.value,
        wallet_name,
        None,
        None,
        Some(&min_token_value),
        None,
        time_out_seconds,
        WalletStateType::OnChain,
    )
    .await
}

#[when(expr = "wallet {string} has {int} or less LGO in {int} seconds")]
#[then(expr = "wallet {string} has {int} or less LGO in {int} seconds")]
async fn step_wallet_has_at_most_value(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    max_token_value: u64,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_or_encumbered_state(
        world,
        &step.value,
        wallet_name,
        None,
        None,
        None,
        Some(&max_token_value),
        time_out_seconds,
        WalletStateType::OnChain,
    )
    .await
}

#[when(expr = "wallet {string} has {int} or more outputs and {int} or more LGO in {int} seconds")]
#[then(expr = "wallet {string} has {int} or more outputs and {int} or more LGO in {int} seconds")]
async fn step_wallet_has_at_least_coins_and_value(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    min_coin_count: usize,
    min_token_value: u64,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_or_encumbered_state(
        world,
        &step.value,
        wallet_name,
        Some(&min_coin_count),
        None,
        Some(&min_token_value),
        None,
        time_out_seconds,
        WalletStateType::OnChain,
    )
    .await
}

#[when(expr = "wallet {string} has {int} or less outputs and {int} or less LGO in {int} seconds")]
#[then(expr = "wallet {string} has {int} or less outputs and {int} or less LGO in {int} seconds")]
async fn step_wallet_has_at_most_coins_and_value(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    max_coin_count: usize,
    max_token_value: u64,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_or_encumbered_state(
        world,
        &step.value,
        wallet_name,
        None,
        Some(&max_coin_count),
        None,
        Some(&max_token_value),
        time_out_seconds,
        WalletStateType::OnChain,
    )
    .await
}

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
    let wallets = world
        .resolve_wallets(&[sender_wallet_name.clone(), receiver_wallet_name.clone()])
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;
    let (sender_wallet, receiver_wallet) = (wallets[0].clone(), wallets[1].clone());

    let receiver_wallet_pk = receiver_wallet.public_key()?;

    for _ in 0..number_of_transactions {
        let tx_hash_hex = create_and_submit_transaction(
            world,
            &step.value,
            &sender_wallet_name,
            &[(receiver_wallet_pk, output_value)],
        )
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

        info!(
            target: TARGET,
            "Sent normal transaction from `{sender_wallet_name}/{}` to {receiver_wallet_name}, \
            value: {output_value}, tx hash: {tx_hash_hex}",
            sender_wallet.node_name
        );
    }

    Ok(())
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
    let wallets = world
        .resolve_wallets(&[sender_wallet_name.clone(), receiver_wallet_name.clone()])
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;
    let (sender_wallet, receiver_wallet) = (wallets[0].clone(), wallets[1].clone());

    let receiver_wallet_pk = receiver_wallet.public_key().inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    let receivers = vec![(receiver_wallet_pk, output_value); number_of_outputs];
    let tx_hash_hex =
        create_and_submit_transaction(world, &step.value, &sender_wallet_name, &receivers)
            .await
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{}` error: {e}", step.value);
            })?;

    info!(
        target: TARGET,
        "Sent normal transaction from `{sender_wallet_name}/{}` to {receiver_wallet_name}, \
        number_of_outputs: {number_of_outputs}, value: {output_value}, tx hash: {tx_hash_hex}",
        sender_wallet.node_name
    );

    Ok(())
}

#[when(expr = "I perform manual control of transactions for all wallets for {int} seconds")]
async fn step_manual_control_transactions(
    world: &mut CucumberWorld,
    step: &Step,
    timeout_seconds: u64,
) -> StepResult {
    perform_manual_step_control(world, &step.value, timeout_seconds).await
}

#[when(expr = "I perform manual control of transactions for all wallets no time-out")]
async fn step_manual_control_transactions_no_time_out(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    perform_manual_step_control(world, &step.value, u64::MAX).await
}

#[given(expr = "I have a faucet with URL {string} username {string} and password {string}")]
#[when(expr = "I have a faucet with URL {string} username {string} and password {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Required by cucumber expression"
)]
fn step_faucet_details(
    world: &mut CucumberWorld,
    step: &Step,
    base_url: String,
    username: String,
    password: String,
) -> StepResult {
    let username = resolve_literal_or_env(&username, "faucet username").inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;
    let password = resolve_literal_or_env(&password, "faucet password").inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    world.faucet_base_url = Some(base_url);
    world.faucet_username = Some(username);
    world.faucet_password = Some(password);

    Ok(())
}

#[given(expr = "I request {int} rounds of faucet funds for wallet {string}")]
#[when(expr = "I request {int} rounds of faucet funds for wallet {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Required by cucumber expression"
)]
fn step_request_faucet_funds_for_wallet(
    world: &mut CucumberWorld,
    step: &Step,
    number_of_rounds: usize,
    wallet_name: String,
) -> StepResult {
    let wallet_pk_hex = if let Ok(wallet) = world.resolve_wallet(&wallet_name) {
        wallet.public_key_hex()
    } else {
        warn!(
            target: TARGET,
            "Step `{}` error: Wallet `{wallet_name}` not found.",
            step.value
        );
        return Err(StepError::LogicalError {
            message: format!("Wallet `{wallet_name}` not found"),
        });
    };

    utils::request_faucet_funds(
        world,
        &step.value,
        non_zero!("number of rounds", number_of_rounds)?,
        &[wallet_pk_hex],
    )
}

#[given(expr = "I request {int} rounds of faucet funds for all wallets")]
#[when(expr = "I request {int} rounds of faucet funds for all wallets")]
fn step_request_faucet_funds_for_all_wallets(
    world: &mut CucumberWorld,
    step: &Step,
    number_of_rounds: usize,
) -> StepResult {
    let all_wallets_pk_hex = world
        .wallet_info
        .values()
        .map(WalletInfo::public_key_hex)
        .collect::<Vec<_>>();

    utils::request_faucet_funds(
        world,
        &step.value,
        non_zero!("number of rounds", number_of_rounds)?,
        &all_wallets_pk_hex,
    )
}

#[given(expr = "I request {int} rounds of faucet funds for all user wallets")]
#[when(expr = "I request {int} rounds of faucet funds for all user wallets")]
fn step_request_faucet_funds_for_all_user_wallets(
    world: &mut CucumberWorld,
    step: &Step,
    number_of_rounds: usize,
) -> StepResult {
    let all_wallets_pk_hex = world
        .wallet_info
        .values()
        .filter(|w| w.is_user_wallet())
        .map(WalletInfo::public_key_hex)
        .collect::<Vec<_>>();

    utils::request_faucet_funds(
        world,
        &step.value,
        non_zero!("number of rounds", number_of_rounds)?,
        &all_wallets_pk_hex,
    )
}

#[given(expr = "I request {int} rounds of faucet funds for all funding wallets")]
#[when(expr = "I request {int} rounds of faucet funds for all funding wallets")]
fn step_request_faucet_funds_for_all_funding_wallets(
    world: &mut CucumberWorld,
    step: &Step,
    number_of_rounds: usize,
) -> StepResult {
    let all_wallets_pk_hex = world
        .wallet_info
        .values()
        .filter(|w| w.is_funding_wallet())
        .map(WalletInfo::public_key_hex)
        .collect::<Vec<_>>();

    utils::request_faucet_funds(
        world,
        &step.value,
        non_zero!("number of rounds", number_of_rounds)?,
        &all_wallets_pk_hex,
    )
}
