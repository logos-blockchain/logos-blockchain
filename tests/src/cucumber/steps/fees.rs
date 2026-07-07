use std::str::FromStr;

use cucumber::{Parameter, gherkin::Step, then};
use lb_core::mantle::{
    gas::GasPrice,
    transactions::genesis_tx::{GENESIS_EXECUTION_GAS_PRICE, GENESIS_STORAGE_GAS_PRICE},
};
use tracing::info;

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::TARGET,
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
    assert_gas_prices(
        world,
        step,
        &node_name,
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
    assert_gas_prices(
        world,
        step,
        &node_name,
        GasPrice::new(execution),
        GasPrice::new(storage),
    )
    .await
}

async fn assert_gas_prices(
    world: &CucumberWorld,
    step: &Step,
    node_name: &str,
    expected_execution: GasPrice,
    expected_storage: GasPrice,
) -> StepResult {
    let client = world.resolve_node_http_client(node_name)?;

    let gas_prices = client
        .gas_prices()
        .await
        .map_err(|source| StepError::StepFail {
            message: format!(
                "Step `{}` error: gas prices request failed: {source}",
                step.value
            ),
        })?;

    if gas_prices.execution_base_gas_price != expected_execution
        || gas_prices.storage_gas_price != expected_storage
    {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: node '{node_name}' reports gas prices execution={:?} \
                 storage={:?}, expected execution={expected_execution:?} \
                 storage={expected_storage:?}",
                step.value, gas_prices.execution_base_gas_price, gas_prices.storage_gas_price,
            ),
        });
    }

    info!(
        target: TARGET,
        "Node '{node_name}' reports the expected gas prices at tip {}", gas_prices.tip
    );

    Ok(())
}
