use std::time::Duration;

use cucumber::{gherkin::Step, when};
use lb_core::mantle::Transaction as _;
use lb_key_management_system_service::keys::Ed25519Key;
use tracing::{info, warn};

use crate::{
    common::{
        chain::wait_for_transactions_inclusion,
        mantle_inscription::{build_funded_inscription_transaction, channel_id_for_payload_size},
    },
    cucumber::{
        error::{StepError, StepResult},
        steps::TARGET,
        world::{CucumberWorld, WalletType},
    },
};

#[when(expr = "I submit inscription transaction {string} of {int} KiB from wallet {string}")]
async fn step_submit_inscription_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    payload_kib: usize,
    wallet_name: String,
) -> StepResult {
    let wallet = world.resolve_wallet(&wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    let wallet_account = match &wallet.wallet_type {
        WalletType::User { wallet_account } => wallet_account,
        WalletType::Funding { .. } => {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "Wallet `{wallet_name}` must be a user wallet to submit inscriptions"
                ),
            });
        }
    };

    let node = world
        .resolve_node_http_client(&wallet.node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

    let payload_size = payload_kib * 1024;

    let signing_key = Ed25519Key::from_bytes(&[0u8; 32]);

    let transaction = build_funded_inscription_transaction(
        &node,
        &world.genesis_block_utxos,
        &wallet_account.secret_key,
        vec![0xAB; payload_size],
        &signing_key,
        channel_id_for_payload_size(payload_size),
        None,
    )
    .await;

    let tx_hash = transaction.hash();

    world
        .submit_transaction(&wallet, &transaction, &node)
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);

    info!(
        target: TARGET,
        "Submitted inscription transaction `{transaction_alias}` from `{wallet_name}` with payload {payload_size} bytes"
    );

    Ok(())
}

#[cucumber::when(expr = "transaction {string} is included on node {string} in {int} seconds")]
#[cucumber::then(expr = "transaction {string} is included on node {string} in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_transaction_is_included_on_node(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    node_name: String,
    timeout_seconds: u64,
) -> StepResult {
    let tx_hash = world.resolve_submitted_transaction(&transaction_alias)?;

    let node = world
        .resolve_node_http_client(&node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

    let included =
        wait_for_transactions_inclusion(&node, &[tx_hash], Duration::from_secs(timeout_seconds))
            .await;

    if included {
        Ok(())
    } else {
        Err(StepError::LogicalError {
            message: format!(
                "Transaction `{transaction_alias}` was not included on node `{node_name}` within {timeout_seconds} seconds"
            ),
        })
    }
}
