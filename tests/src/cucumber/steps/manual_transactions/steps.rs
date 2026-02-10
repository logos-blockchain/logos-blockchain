use cucumber::{gherkin::Step, then, when};
use tracing::{info, warn};

use crate::cucumber::{
    error::StepResult,
    steps::{TARGET, manual_transactions::utils},
    world::CucumberWorld,
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

    let self_pk = wallet.wallet_account.public_key();
    let receivers = vec![(self_pk, output_value); number_of_outputs];
    let tx_hash_hex =
        utils::create_and_submit_transaction(world, &step.value, &wallet_name, &receivers)
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
    utils::wait_for_wallet_state(
        world,
        &step.value,
        wallet_name,
        Some(min_coin_count),
        None,
        time_out_seconds,
    )
    .await
}

#[when(expr = "wallet {string} has {int} or more LGO in {int} seconds")]
#[then(expr = "wallet {string} has {int} or more LGO in {int} seconds")]
async fn step_wallet_has_at_least_value(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    min_value: u64,
    time_out_seconds: u64,
) -> StepResult {
    utils::wait_for_wallet_state(
        world,
        &step.value,
        wallet_name,
        None,
        Some(min_value),
        time_out_seconds,
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
    min_value: u64,
    time_out_seconds: u64,
) -> StepResult {
    utils::wait_for_wallet_state(
        world,
        &step.value,
        wallet_name,
        Some(min_coin_count),
        Some(min_value),
        time_out_seconds,
    )
    .await
}
