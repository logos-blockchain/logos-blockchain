use super::{
    CucumberWorld, Step, StepResult, WalletOutputState, log_wallet_balances, then,
    wait_for_wallet_output_state, when,
};

#[when(expr = "wallet {string} has {int} or more outputs in {int} seconds")]
#[then(expr = "wallet {string} has {int} or more outputs in {int} seconds")]
async fn step_wallet_has_at_least_coins(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    min_coin_count: usize,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_output_state(
        world,
        &step.value,
        wallet_name,
        Some(&min_coin_count),
        None,
        None,
        None,
        time_out_seconds,
        WalletOutputState::OnChain,
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
    wait_for_wallet_output_state(
        world,
        &step.value,
        wallet_name,
        None,
        Some(&max_coin_count),
        None,
        None,
        time_out_seconds,
        WalletOutputState::OnChain,
    )
    .await
}

#[when(expr = "wallet {string} has exactly {int} outputs in {int} seconds")]
#[then(expr = "wallet {string} has exactly {int} outputs in {int} seconds")]
async fn step_wallet_has_exact_coins(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    coin_count: usize,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_output_state(
        world,
        &step.value,
        wallet_name,
        Some(&coin_count),
        Some(&coin_count),
        None,
        None,
        time_out_seconds,
        WalletOutputState::OnChain,
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
    wait_for_wallet_output_state(
        world,
        &step.value,
        wallet_name,
        None,
        Some(&max_coin_count),
        None,
        None,
        time_out_seconds,
        WalletOutputState::Reserved,
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
    wait_for_wallet_output_state(
        world,
        &step.value,
        wallet_name,
        None,
        None,
        Some(&min_token_value),
        None,
        time_out_seconds,
        WalletOutputState::OnChain,
    )
    .await
}

#[when(expr = "wallet {string} has exactly {int} LGO in {int} seconds")]
#[then(expr = "wallet {string} has exactly {int} LGO in {int} seconds")]
async fn step_wallet_has_exact_value(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    token_value: u64,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_output_state(
        world,
        &step.value,
        wallet_name,
        None,
        None,
        Some(&token_value),
        Some(&token_value),
        time_out_seconds,
        WalletOutputState::OnChain,
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
    wait_for_wallet_output_state(
        world,
        &step.value,
        wallet_name,
        None,
        None,
        None,
        Some(&max_token_value),
        time_out_seconds,
        WalletOutputState::OnChain,
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
    wait_for_wallet_output_state(
        world,
        &step.value,
        wallet_name,
        Some(&min_coin_count),
        None,
        Some(&min_token_value),
        None,
        time_out_seconds,
        WalletOutputState::OnChain,
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
    wait_for_wallet_output_state(
        world,
        &step.value,
        wallet_name,
        None,
        Some(&max_coin_count),
        None,
        Some(&max_token_value),
        time_out_seconds,
        WalletOutputState::OnChain,
    )
    .await
}

#[when(expr = "wallet {string} has exactly {int} outputs and {int} LGO in {int} seconds")]
#[then(expr = "wallet {string} has exactly {int} outputs and {int} LGO in {int} seconds")]
async fn step_wallet_has_exact_coins_and_value(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    coin_count: usize,
    token_value: u64,
    time_out_seconds: u64,
) -> StepResult {
    wait_for_wallet_output_state(
        world,
        &step.value,
        wallet_name,
        Some(&coin_count),
        Some(&coin_count),
        Some(&token_value),
        Some(&token_value),
        time_out_seconds,
        WalletOutputState::OnChain,
    )
    .await
}

#[when(expr = "I log wallet balances for all wallets")]
#[then(expr = "I log wallet balances for all wallets")]
async fn step_wallet_balance_all_wallets(world: &mut CucumberWorld, step: &Step) -> StepResult {
    let mut wallets = world.all_user_wallets();
    wallets.extend(world.all_node_wallets());

    log_wallet_balances(world, &step.value, wallets).await
}
