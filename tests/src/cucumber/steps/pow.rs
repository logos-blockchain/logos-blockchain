use std::time::Duration;

use cucumber::{gherkin::Step, then, when};
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::{
    common::wallet::WalletOutputState,
    cucumber::{
        error::{StepError, StepResult},
        steps::TARGET,
        wallet::sync::current_wallet_output_balance,
        world::CucumberWorld,
    },
};

/// Interval between `PoW` claim attempts while waiting for a mined reward.
const CLAIM_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Interval between balance polls while waiting for a wallet to grow.
const BALANCE_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[when(expr = "I start mining on node {string}")]
async fn step_start_mining(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
) -> StepResult {
    let node = world
        .resolve_node_http_client(&node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

    node.start_mining().await?;

    info!(target: TARGET, "Started PoW mining on node `{node_name}`");
    Ok(())
}

#[when(expr = "I stop mining on node {string}")]
async fn step_stop_mining(world: &mut CucumberWorld, step: &Step, node_name: String) -> StepResult {
    let node = world
        .resolve_node_http_client(&node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

    node.stop_mining().await?;

    info!(target: TARGET, "Stopped PoW mining on node `{node_name}`");
    Ok(())
}

#[when(expr = "I claim PoW rewards on node {string} as {string} within {int} seconds")]
#[cucumber::then(expr = "I claim PoW rewards on node {string} as {string} within {int} seconds")]
async fn step_claim_pow_rewards(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    transaction_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let node = world
        .resolve_node_http_client(&node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

    // The node only produces a claim transaction once it has mined at least one
    // winning ticket, so poll `claim` until it submits one (or we time out).
    let deadline = Duration::from_secs(timeout_seconds);
    let started = Instant::now();
    let tx_hash = loop {
        match node.claim_pow_rewards().await {
            Ok(Some(tx_hash)) => break tx_hash,
            Ok(None) => {}
            Err(e) => {
                warn!(target: TARGET, "PoW claim attempt on node `{node_name}` failed: {e}");
            }
        }

        if started.elapsed() >= deadline {
            return Err(StepError::Timeout {
                message: format!(
                    "node `{node_name}` produced no claimable PoW rewards within {timeout_seconds} seconds"
                ),
            });
        }

        sleep(CLAIM_POLL_INTERVAL).await;
    };

    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);

    info!(
        target: TARGET,
        "Claimed PoW rewards on node `{node_name}` as transaction `{transaction_alias}`"
    );
    Ok(())
}

/// Reads a wallet's current on-chain balance.
async fn wallet_onchain_value(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> Result<u64, StepError> {
    let wallet = world
        .wallet_info
        .get(wallet_name)
        .cloned()
        .ok_or_else(|| StepError::LogicalError {
            message: format!("wallet '{wallet_name}' not found in world state"),
        })?;

    let balance =
        current_wallet_output_balance(world, step, &wallet, WalletOutputState::OnChain).await?;
    Ok(balance.value)
}

#[when(expr = "I record the balance of wallet {string} as {string}")]
#[then(expr = "I record the balance of wallet {string} as {string}")]
async fn step_record_wallet_balance(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    baseline_label: String,
) -> StepResult {
    let value = wallet_onchain_value(world, &step.value, &wallet_name).await?;
    world
        .recorded_wallet_balances
        .insert(baseline_label.clone(), value);

    info!(
        target: TARGET,
        "Recorded balance of wallet `{wallet_name}` as `{baseline_label}` = {value} LGO"
    );
    Ok(())
}

#[when(expr = "wallet {string} balance increased by at least {int} over {string} in {int} seconds")]
#[then(expr = "wallet {string} balance increased by at least {int} over {string} in {int} seconds")]
async fn step_wallet_balance_increased(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    minimum_increase: u64,
    baseline_label: String,
    timeout_seconds: u64,
) -> StepResult {
    let baseline = *world.recorded_wallet_balances.get(&baseline_label).ok_or_else(|| {
        StepError::LogicalError {
            message: format!("no recorded wallet balance found for label '{baseline_label}'"),
        }
    })?;
    let target = baseline.saturating_add(minimum_increase);

    let deadline = Duration::from_secs(timeout_seconds);
    let started = Instant::now();
    loop {
        let latest = wallet_onchain_value(world, &step.value, &wallet_name).await?;
        if latest >= target {
            info!(
                target: TARGET,
                "Wallet `{wallet_name}` grew from {baseline} to {latest} LGO \
                (>= baseline + {minimum_increase})"
            );
            return Ok(());
        }

        if started.elapsed() >= deadline {
            return Err(StepError::StepFail {
                message: format!(
                    "wallet `{wallet_name}` balance {latest} did not reach baseline `{baseline_label}` \
                    ({baseline}) + {minimum_increase} = {target} within {timeout_seconds} seconds"
                ),
            });
        }

        sleep(BALANCE_POLL_INTERVAL).await;
    }
}
