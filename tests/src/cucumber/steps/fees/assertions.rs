//! Assertions behind the fee-market steps: gas price checks, the spec
//! reference comparison, and on-chain fee accounting.

use std::time::Duration;

use cucumber::gherkin::Step;
use lb_core::{
    header::HeaderId,
    mantle::gas::{GasPrice, MainnetGasProfile, TxGasCalculator as _},
};
use tracing::info;

use crate::{
    common::fee_spec::{self, ExecutionMarketReference, GasPriceRecord},
    cucumber::{
        error::{StepError, StepResult},
        steps::{TARGET, fees::actions},
        world::CucumberWorld,
    },
};

/// Checks the node reports the expected gas prices, at the given tip or the
/// current one.
pub async fn assert_gas_prices(
    world: &CucumberWorld,
    step: &Step,
    node_name: &str,
    tip: Option<HeaderId>,
    expected_execution: GasPrice,
    expected_storage: GasPrice,
) -> StepResult {
    let client = world.resolve_node_http_client(node_name)?;

    let gas_prices = client
        .gas_prices(tip)
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

/// Polls the wallet balance until it equals the wallet's genesis funds minus
/// the fee its transaction burned on chain.
pub async fn wallet_debited_exactly_fee(
    world: &CucumberWorld,
    step: &Step,
    wallet_name: &str,
    transaction_alias: &str,
    timeout_secs: u64,
) -> StepResult {
    let account = actions::user_wallet_account(world, step, wallet_name)?;
    let wallet_info = world
        .wallet_registry
        .wallet_info
        .get(wallet_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!(
                "Step `{}` error: unknown wallet `{wallet_name}`",
                step.value
            ),
        })?;
    let client = world.resolve_node_http_client(&wallet_info.node_name.clone())?;
    let signed_tx = world.resolve_prepared_transaction(transaction_alias)?;

    let genesis_funds: u64 = world
        .chain
        .genesis_block_utxos
        .iter()
        .filter(|utxo| utxo.note.pk == account.public_key())
        .map(|utxo| utxo.note.value)
        .sum();

    let burned = fee_spec::net_balance_against(&world.chain.genesis_block_utxos, &signed_tx)
        .map_err(|message| StepError::StepFail {
            message: format!("Step `{}` error: {message}", step.value),
        })?;

    let expected = genesis_funds - burned;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut last = "not yet queried".to_owned();
    while tokio::time::Instant::now() < deadline {
        match client.wallet_balance(account.public_key(), None).await {
            Ok(response) if response.balance == expected => {
                info!(
                    target: TARGET,
                    "Wallet '{wallet_name}' was debited exactly the {burned} LGO fee of \
                     `{transaction_alias}` (balance {expected})",
                );
                return Ok(());
            }
            Ok(response) => last = format!("balance {}", response.balance),
            Err(error) => last = format!("error: {error}"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    Err(StepError::StepFail {
        message: format!(
            "Step `{}` error: wallet `{wallet_name}` did not reach balance {expected} \
             (genesis funds {genesis_funds} minus fee {burned}), last result: {last}",
            step.value
        ),
    })
}

/// Checks that a prepared transaction remains funded with a smaller tip.
///
/// This proves that a fee increase consumed some of its original headroom
/// without invalidating it.
pub async fn prepared_transaction_tip_absorbed_fee_increase(
    world: &CucumberWorld,
    step: &Step,
    transaction_alias: &str,
    original_tip: u64,
    node_name: &str,
) -> StepResult {
    let signed_tx = world.resolve_prepared_transaction(transaction_alias)?;
    let client = world.resolve_node_http_client(node_name)?;
    let prices = actions::live_gas_prices(&client, step).await?;
    let remaining_tip =
        fee_spec::fee_surplus_at(&world.chain.genesis_block_utxos, &signed_tx, &prices).map_err(
            |message| StepError::StepFail {
                message: format!("Step `{}` error: {message}", step.value),
            },
        )?;

    if !(0 < remaining_tip && remaining_tip < i128::from(original_tip)) {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: transaction `{transaction_alias}` was prepared with tip \
                 {original_tip}, but its effective tip at current prices is {remaining_tip}; \
                 expected the fee increase to consume part, but not all, of the original tip",
                step.value
            ),
        });
    }

    info!(
        target: TARGET,
        "Transaction `{transaction_alias}` remains funded after its effective tip fell from \
         {original_tip} to {remaining_tip}",
    );

    Ok(())
}

/// Recalculates the mandatory fee of a percentage-funded transaction at the
/// current prices and verifies that the price increase consumed part of its
/// original reserve without making it underfunded.
pub async fn prepared_transaction_percentage_reserve_absorbed_fee_increase(
    world: &CucumberWorld,
    step: &Step,
    transaction_alias: &str,
    configured_percent: u64,
    node_name: &str,
) -> StepResult {
    let prepared_fee = world.resolve_prepared_priority_fee(transaction_alias)?;
    if prepared_fee.percent != configured_percent {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: transaction `{transaction_alias}` was prepared with {}%, \
                 expected {configured_percent}%",
                step.value, prepared_fee.percent
            ),
        });
    }

    let signed_tx = world.resolve_prepared_transaction(transaction_alias)?;
    let client = world.resolve_node_http_client(node_name)?;
    let prices = actions::live_gas_prices(&client, step).await?;
    let current_mandatory_fee = signed_tx
        .total_gas_cost::<MainnetGasProfile>(&prices)
        .map_err(|source| StepError::StepFail {
            message: format!(
                "Step `{}` error: current mandatory fee calculation failed: {source}",
                step.value
            ),
        })?
        .into_inner();
    let remaining_reserve = prepared_fee
        .funded_fee
        .checked_sub(current_mandatory_fee)
        .ok_or_else(|| StepError::StepFail {
            message: format!(
                "Step `{}` error: transaction `{transaction_alias}` is underfunded: \
                 funded fee {}, current mandatory fee {}",
                step.value, prepared_fee.funded_fee, current_mandatory_fee
            ),
        })?;

    if remaining_reserve >= prepared_fee.initial_reserve {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: transaction `{transaction_alias}` reserve did not shrink: \
                 initial mandatory={}, initial reserve={}, funded={}, current mandatory={}, \
                 remaining reserve={}",
                step.value,
                prepared_fee.initial_mandatory_fee,
                prepared_fee.initial_reserve,
                prepared_fee.funded_fee,
                current_mandatory_fee,
                remaining_reserve,
            ),
        });
    }

    info!(
        target: TARGET,
        "Transaction `{transaction_alias}` retained a smaller effective reserve: \
         {}% of initial mandatory {} => reserve {}, funded {}; prices execution {} -> {}, \
         storage {} -> {}; current mandatory {}, remaining reserve {}",
        prepared_fee.percent,
        prepared_fee.initial_mandatory_fee,
        prepared_fee.initial_reserve,
        prepared_fee.funded_fee,
        prepared_fee.initial_execution_price,
        prices.execution_base_gas_price.into_inner(),
        prepared_fee.initial_storage_price,
        prices.storage_gas_price.into_inner(),
        current_mandatory_fee,
        remaining_reserve,
    );

    Ok(())
}

/// Replays the recorded prices through the spec's price calculation and
/// checks the node reported the same price after every block.
pub fn execution_prices_follow_spec_reference(world: &CucumberWorld, step: &Step) -> StepResult {
    let records = recorded_gas_prices(world, step)?;

    let mut reference = ExecutionMarketReference::at_genesis();
    for record in records {
        reference.apply_block(record.block_execution_gas);
        if u128::from(record.execution_price) != reference.base_fee {
            return Err(StepError::StepFail {
                message: format!(
                    "Step `{}` error: execution base fee diverged from the spec reference \
                     at slot {}: node reports {}, reference expects {} (block gas {}, \
                     reference EMA {})",
                    step.value,
                    record.slot,
                    record.execution_price,
                    reference.base_fee,
                    record.block_execution_gas,
                    reference.gas_ema,
                ),
            });
        }
    }

    info!(
        target: TARGET,
        "Execution base fee followed the spec reference for {} blocks (final price {})",
        records.len(),
        reference.base_fee,
    );

    Ok(())
}

/// Checks the recorded slot range crosses at least one configured epoch
/// boundary.
pub fn gas_prices_cross_epoch_boundary(
    world: &CucumberWorld,
    step: &Step,
    slots_per_epoch: u64,
) -> StepResult {
    if slots_per_epoch == 0 {
        return Err(StepError::InvalidArgument {
            message: "slots per epoch must be greater than 0".to_owned(),
        });
    }

    let records = recorded_gas_prices(world, step)?;
    let first_slot = records[0].slot;
    let last_slot = records[records.len() - 1].slot;
    let first_epoch = first_slot / slots_per_epoch;
    let last_epoch = last_slot / slots_per_epoch;

    if first_epoch == last_epoch {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: recorded slots {first_slot}..={last_slot} stayed within epoch \
                 {first_epoch}; expected to cross a boundary with {slots_per_epoch} slots per \
                 epoch",
                step.value,
            ),
        });
    }

    info!(
        target: TARGET,
        "Recorded gas prices crossed from epoch {first_epoch} to {last_epoch} over slots \
         {first_slot}..={last_slot}",
    );

    Ok(())
}

/// With traffic on chain, the storage price must move at an epoch boundary.
///
/// Per the [1.0.0] Storage Market spec, usage recorded during an epoch
/// drives the price update at its boundary. Guards that the live storage
/// market remains connected to network usage.
pub fn storage_prices_respond_to_usage(world: &CucumberWorld, step: &Step) -> StepResult {
    let records = recorded_gas_prices(world, step)?;

    let first_price = records[0].storage_price;
    if !records
        .iter()
        .any(|record| record.storage_price > first_price)
    {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: the storage gas price never rose above {first_price} over {} \
                 recorded blocks despite storage usage and epoch boundaries",
                step.value,
                records.len(),
            ),
        });
    }

    info!(
        target: TARGET,
        "Storage gas price rose in response to usage over {} blocks", records.len(),
    );

    Ok(())
}

fn recorded_gas_prices<'world>(
    world: &'world CucumberWorld,
    step: &Step,
) -> Result<&'world [GasPriceRecord], StepError> {
    if world.recorded_gas_prices.is_empty() {
        return Err(StepError::LogicalError {
            message: format!(
                "Step `{}` error: no recorded gas prices; run the recording step first",
                step.value
            ),
        });
    }

    Ok(&world.recorded_gas_prices)
}
