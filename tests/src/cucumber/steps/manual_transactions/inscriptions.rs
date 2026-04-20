use std::time::Duration;

use cucumber::{gherkin::Step, when};
use lb_key_management_system_service::keys::Ed25519Key;
use tracing::{info, warn};

use crate::{
    common::{
        chain::wait_for_transactions_inclusion,
        mantle_inscription::{
            build_inscription_tx_builder, channel_id_for_payload_size, inscription_signature_proof,
        },
    },
    cucumber::{
        error::{StepError, StepResult},
        steps::{
            TARGET,
            manual_transactions::utils::{
                prepare_user_wallet_built_transaction_submission,
                submit_prepared_user_wallet_transaction,
            },
        },
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
    let payload_size = payload_kib * 1024;
    submit_inscription_transaction(
        world,
        step,
        transaction_alias,
        vec![0xAB; payload_size],
        wallet_name,
    )
    .await
}

#[when(
    expr = "I submit inscription transaction {string} with payload {string} from wallet {string}"
)]
async fn step_submit_inscription_transaction_with_payload(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    payload: String,
    wallet_name: String,
) -> StepResult {
    submit_inscription_transaction(
        world,
        step,
        transaction_alias,
        payload.into_bytes(),
        wallet_name,
    )
    .await
}

async fn submit_inscription_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    payload: Vec<u8>,
    wallet_name: String,
) -> StepResult {
    let wallet = world.resolve_wallet(&wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    match &wallet.wallet_type {
        WalletType::User { .. } => {}
        WalletType::Funding { .. } => {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "Wallet `{wallet_name}` must be a user wallet to submit inscriptions"
                ),
            });
        }
    }

    let payload_size = payload.len();
    let signing_key = Ed25519Key::from_bytes(&[0u8; 32]);

    let tx_builder = build_inscription_tx_builder(
        payload,
        &signing_key,
        channel_id_for_payload_size(payload_size),
        None,
    );
    let prepared = prepare_user_wallet_built_transaction_submission(
        world,
        &step.value,
        &wallet_name,
        tx_builder,
        0,
        None,
    )
    .await;
    let prepared = prepared.inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;
    let tx_hash = prepared.tx_hash;

    let tx_hash = submit_prepared_user_wallet_transaction(
        world,
        &step.value,
        prepared,
        vec![inscription_signature_proof(tx_hash, &signing_key)],
        None,
    )
    .await;
    let tx_hash = tx_hash.inspect_err(|e| {
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
