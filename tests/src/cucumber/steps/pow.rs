use std::time::Duration;

use cucumber::{gherkin::Step, when};
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::TARGET,
    world::CucumberWorld,
};

/// Interval between `PoW` claim attempts while waiting for a mined reward.
const CLAIM_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[when(expr = "I start mining on node {string}")]
async fn step_start_mining(world: &mut CucumberWorld, step: &Step, node_name: String) -> StepResult {
    let node = world.resolve_node_http_client(&node_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    node.start_mining().await?;

    info!(target: TARGET, "Started PoW mining on node `{node_name}`");
    Ok(())
}

#[when(expr = "I stop mining on node {string}")]
async fn step_stop_mining(world: &mut CucumberWorld, step: &Step, node_name: String) -> StepResult {
    let node = world.resolve_node_http_client(&node_name).inspect_err(|e| {
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
    let node = world.resolve_node_http_client(&node_name).inspect_err(|e| {
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
