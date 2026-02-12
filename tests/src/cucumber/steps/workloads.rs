use cucumber::given;

use crate::cucumber::{error::StepResult, world::CucumberWorld};

#[given(expr = "wallets total funds is {int} split across {int} users")]
fn wallets_total_funds(world: &mut CucumberWorld, total_funds: u64, users: usize) -> StepResult {
    world.set_wallets(total_funds, users)
}

#[given(expr = "run duration is {int} seconds")]
fn run_duration(world: &mut CucumberWorld, seconds: u64) -> StepResult {
    world.set_run_duration(seconds)
}

#[given(expr = "transactions rate is {int} per block")]
fn tx_rate(world: &mut CucumberWorld, rate: u64) -> StepResult {
    world.set_transactions_rate(rate, None)
}

#[given(expr = "transactions rate is {int} per block using {int} users")]
fn tx_rate_with_users(world: &mut CucumberWorld, rate: u64, users: usize) -> StepResult {
    world.set_transactions_rate(rate, Some(users))
}

#[given(expr = "expect consensus liveness")]
const fn expect_consensus_liveness(world: &mut CucumberWorld) {
    world.enable_consensus_liveness();
}

#[given(expr = "consensus liveness lag allowance is {int}")]
fn liveness_lag_allowance(world: &mut CucumberWorld, blocks: u64) -> StepResult {
    world.set_consensus_liveness_lag_allowance(blocks)
}
