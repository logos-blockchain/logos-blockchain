use std::{collections::HashSet, time::Duration};

use cucumber::{gherkin::Step, then, when};
use lb_core::mantle::{Op, traits::Hashable as _, transactions::hash::TxHash};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_testing_framework::NodeHttpClient;
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::{
    common::{chain::scan_chain_until, wallet::WalletOutputState},
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
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
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
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
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

#[when(expr = "node {string} has at least {int} claimable PoW rewards within {int} seconds")]
#[then(expr = "node {string} has at least {int} claimable PoW rewards within {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_node_has_claimable_rewards(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    minimum: usize,
    timeout_seconds: u64,
) -> StepResult {
    let node = world
        .resolve_node_http_client(&node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

    let deadline = Duration::from_secs(timeout_seconds);
    let started = Instant::now();
    loop {
        match node.pow_claimable_tickets().await {
            Ok(tickets) if tickets >= minimum => {
                info!(
                    target: TARGET,
                    "Node `{node_name}` reports {tickets} claimable PoW rewards (>= {minimum})"
                );
                return Ok(());
            }
            Ok(_) => {}
            Err(e) => {
                warn!(target: TARGET, "Claimable-rewards query on node `{node_name}` failed: {e}");
            }
        }

        if started.elapsed() >= deadline {
            return Err(StepError::Timeout {
                message: format!(
                    "node `{node_name}` did not report at least {minimum} claimable PoW rewards \
                    within {timeout_seconds} seconds"
                ),
            });
        }

        sleep(CLAIM_POLL_INTERVAL).await;
    }
}

#[when(expr = "I claim PoW rewards on node {string} as {string} within {int} seconds")]
#[then(expr = "I claim PoW rewards on node {string} as {string} within {int} seconds")]
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
    let wallet =
        world
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

/// Sums the values of a transaction's transfer outputs paid to `claim_address`,
/// by locating the transaction on the chain served by `node`. This is the exact
/// amount the reward-claim transaction credited to that account.
async fn claim_reward_credited_to(
    node: &NodeHttpClient,
    tx_hash: TxHash,
    claim_address: ZkPublicKey,
) -> Result<u64, StepError> {
    let tip = node.consensus_info().await?.cryptarchia_info.tip;
    let mut scanned_blocks = HashSet::new();

    let credited = scan_chain_until(
        tip,
        &mut scanned_blocks,
        |header_id| {
            let node = node.clone();
            async move { node.block(&header_id).await.ok().flatten() }
        },
        |block| {
            let tx = block.transactions.iter().find(|tx| tx.hash() == tx_hash)?;
            let credited = tx
                .ops_with_proof()
                .filter_map(|(op, _proof)| match op {
                    Op::Transfer(transfer) => Some(transfer),
                    _ => None,
                })
                .flat_map(|transfer| transfer.outputs.iter())
                .filter(|note| note.pk == claim_address)
                .map(|note| note.value)
                .sum::<u64>();
            Some(credited)
        },
    )
    .await;

    credited.ok_or_else(|| StepError::LogicalError {
        message: format!("claim transaction {tx_hash:?} was not found on the chain"),
    })
}

#[when(
    expr = "wallet {string} balance increased by exactly the reward from claim {string} over {string} in {int} seconds"
)]
#[then(
    expr = "wallet {string} balance increased by exactly the reward from claim {string} over {string} in {int} seconds"
)]
async fn step_wallet_balance_increased_by_claim_reward(
    world: &mut CucumberWorld,
    step: &Step,
    wallet_name: String,
    claim_alias: String,
    baseline_label: String,
    timeout_seconds: u64,
) -> StepResult {
    let baseline = *world
        .recorded_wallet_balances
        .get(&baseline_label)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("no recorded wallet balance found for label '{baseline_label}'"),
        })?;
    let tx_hash = world.resolve_submitted_transaction(&claim_alias)?;

    let wallet = world
        .wallet_info
        .get(&wallet_name)
        .cloned()
        .ok_or_else(|| StepError::LogicalError {
            message: format!("wallet '{wallet_name}' not found in world state"),
        })?;
    // The reward beneficiary is this wallet's public key, which is what the
    // miner's `claim_address` was pointed at.
    let claim_address = wallet.public_key()?;
    let node = world.resolve_node_http_client(&wallet.node_name)?;

    let reward = claim_reward_credited_to(&node, tx_hash, claim_address).await?;
    if reward == 0 {
        return Err(StepError::LogicalError {
            message: format!(
                "claim transaction `{claim_alias}` credited nothing to wallet `{wallet_name}`"
            ),
        });
    }
    let expected = baseline.saturating_add(reward);

    let deadline = Duration::from_secs(timeout_seconds);
    let started = Instant::now();
    loop {
        let latest = wallet_onchain_value(world, &step.value, &wallet_name).await?;
        if latest == expected {
            info!(
                target: TARGET,
                "Wallet `{wallet_name}` grew from {baseline} to {latest} LGO, exactly the \
                {reward} LGO credited by claim `{claim_alias}`"
            );
            return Ok(());
        }
        // Overshooting the claimed reward means something other than the claim
        // credited the account — the increase is then not strictly the reward.
        if latest > expected {
            return Err(StepError::StepFail {
                message: format!(
                    "wallet `{wallet_name}` balance {latest} exceeded baseline `{baseline_label}` \
                    ({baseline}) + claim reward ({reward}) = {expected}; increase is not strictly \
                    the claimed reward"
                ),
            });
        }

        if started.elapsed() >= deadline {
            return Err(StepError::StepFail {
                message: format!(
                    "wallet `{wallet_name}` balance {latest} did not reach baseline `{baseline_label}` \
                    ({baseline}) + claim reward ({reward}) = {expected} within {timeout_seconds} seconds"
                ),
            });
        }

        sleep(BALANCE_POLL_INTERVAL).await;
    }
}
