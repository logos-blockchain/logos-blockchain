//! Cucumber entrypoints for the fee-market steps. The logic lives in
//! [`super::actions`] and [`super::assertions`].

use std::str::FromStr;

use cucumber::{Parameter, gherkin::Step, then, when};
use lb_core::mantle::{
    gas::GasPrice,
    transactions::{GENESIS_EXECUTION_GAS_PRICE, GENESIS_STORAGE_GAS_PRICE},
};

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::fees::{actions, assertions},
    world::CucumberWorld,
};

/// Named gas price expectations resolvable from code, so feature files don't
/// hardcode price values that change with protocol constants.
#[derive(Debug, Parameter)]
#[param(name = "gas_prices_reference", regex = "genesis")]
pub enum GasPricesReference {
    Genesis,
}

impl GasPricesReference {
    const fn expected(&self) -> (GasPrice, GasPrice) {
        match self {
            Self::Genesis => (GENESIS_EXECUTION_GAS_PRICE, GENESIS_STORAGE_GAS_PRICE),
        }
    }
}

impl FromStr for GasPricesReference {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "genesis" => Ok(Self::Genesis),
            other => Err(format!("unknown gas prices reference `{other}`")),
        }
    }
}

#[then(expr = "gas prices on node {string} equal the {gas_prices_reference} gas prices")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber step entrypoints must take `&mut World`"
)]
async fn step_gas_prices_equal_reference(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    reference: GasPricesReference,
) -> StepResult {
    let (expected_execution, expected_storage) = reference.expected();
    assertions::assert_gas_prices(
        world,
        step,
        &node_name,
        None,
        expected_execution,
        expected_storage,
    )
    .await
}

#[then(
    expr = "gas prices on node {string} at the genesis block equal the {gas_prices_reference} \
            gas prices"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber step entrypoints must take `&mut World`"
)]
async fn step_gas_prices_at_genesis_equal_reference(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    reference: GasPricesReference,
) -> StepResult {
    let genesis_block_id = world
        .genesis_block_id
        .ok_or_else(|| StepError::LogicalError {
            message: format!(
                "Step `{}` error: genesis block id is not available for this cluster",
                step.value
            ),
        })?;

    let (expected_execution, expected_storage) = reference.expected();

    assertions::assert_gas_prices(
        world,
        step,
        &node_name,
        Some(genesis_block_id),
        expected_execution,
        expected_storage,
    )
    .await
}

#[then(expr = "gas prices on node {string} are execution {int} and storage {int}")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber step entrypoints must take `&mut World`"
)]
async fn step_gas_prices_are(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    execution: u64,
    storage: u64,
) -> StepResult {
    assertions::assert_gas_prices(
        world,
        step,
        &node_name,
        None,
        GasPrice::new(execution),
        GasPrice::new(storage),
    )
    .await
}

#[when(
    expr = "I submit an exactly funded self-transfer from wallet {string} via node {string} \
            as {string}"
)]
async fn step_submit_exactly_funded_self_transfer(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    node_name: String,
    transaction_alias: String,
) -> StepResult {
    actions::submit_self_transfer_with_tip(
        world,
        step,
        &wallet_name,
        &node_name,
        transaction_alias,
        0,
    )
    .await
}

#[when(
    expr = "I submit a self-transfer underfunded by {int} LGO from wallet {string} via node \
            {string} as {string}"
)]
async fn step_submit_underfunded_self_transfer(
    world: &mut CucumberWorld,
    step: &Step,
    shortfall: u64,
    wallet_name: String,
    node_name: String,
    transaction_alias: String,
) -> StepResult {
    actions::submit_self_transfer_with_tip(
        world,
        step,
        &wallet_name,
        &node_name,
        transaction_alias,
        -i128::from(shortfall),
    )
    .await
}

#[then(
    expr = "wallet {string} is debited exactly the fee of transaction {string} in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber step entrypoints must take `&mut World`"
)]
async fn step_wallet_debited_exactly_fee(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    transaction_alias: String,
    timeout_secs: u64,
) -> StepResult {
    assertions::wallet_debited_exactly_fee(
        world,
        step,
        &wallet_name,
        &transaction_alias,
        timeout_secs,
    )
    .await
}

#[when(expr = "I record per-block gas prices on node {string} for {int} slots in {int} seconds")]
async fn step_record_per_block_gas_prices(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    slots: u64,
    timeout_secs: u64,
) -> StepResult {
    actions::record_per_block_gas_prices(world, step, &node_name, slots, timeout_secs).await
}

#[then(expr = "recorded execution gas prices follow the execution market spec reference")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber step entrypoints must take `&mut World`"
)]
fn step_execution_prices_follow_spec_reference(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    assertions::execution_prices_follow_spec_reference(world, step)
}

#[then(expr = "recorded storage gas prices respond to network usage")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber step entrypoints must take `&mut World`"
)]
fn step_storage_prices_respond_to_usage(world: &mut CucumberWorld, step: &Step) -> StepResult {
    assertions::storage_prices_respond_to_usage(world, step)
}

#[when(
    expr = "I concurrently fund transfers of {int} LGO from wallets {string} and {string} to \
            wallets {string} and {string} via node {string} as {string} and {string}"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "cucumber step signatures mirror the step expression's captures"
)]
async fn step_concurrently_fund_transfers(
    world: &mut CucumberWorld,
    step: &Step,
    amount: u64,
    funder_a_name: String,
    funder_b_name: String,
    recipient_a_name: String,
    recipient_b_name: String,
    node_name: String,
    alias_a: String,
    alias_b: String,
) -> StepResult {
    actions::concurrently_fund_transfers(
        world,
        step,
        amount,
        [&funder_a_name, &funder_b_name],
        [&recipient_a_name, &recipient_b_name],
        &node_name,
        [alias_a, alias_b],
    )
    .await
}

#[then(expr = "transactions {string} and {string} spent disjoint ledger inputs on node {string}")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber step entrypoints must take `&mut World`"
)]
async fn step_transactions_spent_disjoint_inputs(
    world: &mut CucumberWorld,
    step: &Step,
    alias_a: String,
    alias_b: String,
    node_name: String,
) -> StepResult {
    assertions::transactions_spent_disjoint_inputs(world, step, &alias_a, &alias_b, &node_name)
        .await
}
