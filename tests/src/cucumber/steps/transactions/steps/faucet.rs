use super::{
    CucumberWorld, Step, StepError, StepResult, TARGET, WalletInfo, given, non_zero, utils, warn,
    when,
};

#[given(expr = "I have a faucet with URL {string}")]
#[when(expr = "I have a faucet with URL {string}")]
fn step_faucet_details(world: &mut CucumberWorld, base_url: String) {
    world.wallet_registry.faucet_base_url = Some(base_url);
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
    let wallet = world.resolve_wallet(&wallet_name).inspect_err(|error| {
        warn!(target: TARGET, "Step `{}` error: {error}", step.value);
    })?;

    let wallet_pk_hex = wallet.public_key_hex();

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
        .wallet_registry
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
        .wallet_registry
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
        .wallet_registry
        .wallet_info
        .values()
        .filter(|wallet| wallet.is_node_funding_wallet())
        .map(WalletInfo::public_key_hex)
        .collect::<Vec<_>>();

    utils::request_faucet_funds(
        world,
        &step.value,
        non_zero!("number of rounds", number_of_rounds)?,
        &all_wallets_pk_hex,
    )
}
