use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use lb_core::mantle::{SignedMantleTx, Transaction as _, TxHash};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_tx_service::{backend::PoolRecoveryState, tx::state::TxMempoolState};
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use crate::{
    common::wallet::{WalletTransactionError, WalletTransactionIntent},
    cucumber::{
        error::StepError,
        fee_reserve::DEFAULT_STORAGE_GAS_PRICE,
        steps::{TARGET, manual_transactions::tracked_transactions::create_invalid_transaction},
        wallet::submissions::{
            SignedUserWalletSubmission, prepare_user_wallet_transaction_submission,
            record_signed_user_wallet_submission, sign_prepared_user_wallet_transaction,
        },
        world::{CucumberWorld, NodeInfo},
    },
};

const RECOVERY_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);
const RECOVERY_FLUSH_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub async fn prepare_transfer_transaction(
    world: &mut CucumberWorld,
    step: &str,
    transaction_alias: String,
    amount: u64,
    sender_wallet_name: String,
    receiver_wallet_name: String,
) -> Result<(), StepError> {
    let receiver_pk = receiver_public_key(world, step, &receiver_wallet_name)?;
    let intent = transfer_intent(receiver_pk, amount)?;
    let prepared = prepare_user_wallet_transaction_submission(
        world,
        step,
        &sender_wallet_name,
        intent,
        None,
        None,
    )
    .await?;
    let signed = sign_prepared_user_wallet_transaction(step, prepared, Vec::new())?;
    let tx_hash = record_prepared_transaction(world, transaction_alias.clone(), &signed)?;

    report_prepared_transaction(
        &transaction_alias,
        tx_hash,
        &sender_wallet_name,
        &receiver_wallet_name,
    );

    Ok(())
}

pub async fn submit_prepared_transaction_to_nodes(
    world: &CucumberWorld,
    step: &str,
    transaction_alias: String,
    node_names: Vec<String>,
) -> Result<(), StepError> {
    let signed_tx = world.resolve_prepared_transaction(&transaction_alias)?;
    let tx_hash = signed_tx.hash();

    for node_name in node_names {
        submit_prepared_transaction_to_node(
            world,
            step,
            &transaction_alias,
            &signed_tx,
            tx_hash,
            &node_name,
        )
        .await?;
    }

    Ok(())
}

pub async fn try_submit_invalid_transaction(
    world: &mut CucumberWorld,
    step: &str,
    transaction_alias: String,
    node_name: String,
) -> Result<(), StepError> {
    let node = world
        .resolve_node_http_client(&node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;
    let signed_tx = create_invalid_transaction();
    let tx_hash = signed_tx.hash();

    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);

    match node.submit_transaction(&signed_tx).await {
        Ok(()) => {
            info!(
                target: TARGET,
                "Submitted invalid transaction `{transaction_alias}` ({:?}) to `{node_name}`",
                tx_hash
            );
        }
        Err(error) => {
            info!(
                target: TARGET,
                "Invalid transaction `{transaction_alias}` ({:?}) was rejected by `{node_name}`: {error}",
                tx_hash
            );
        }
    }

    Ok(())
}

pub async fn wait_for_mempool_recovery_flush(
    world: &CucumberWorld,
    node_name: &str,
    transaction_alias: &str,
) -> Result<(), StepError> {
    let node_info = world
        .nodes_info
        .get(node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node `{node_name}` is not started"),
        })?;
    let tx_hash = world.resolve_submitted_transaction(transaction_alias)?;

    let pending_hashes = collect_pending_mempool_hashes(node_info).await?;

    if !pending_hashes.contains(&tx_hash) {
        return Err(StepError::LogicalError {
            message: format!(
                "Transaction `{transaction_alias}` ({tx_hash:?}) is not pending in node `{node_name}` mempool"
            ),
        });
    }

    let recovery_file = mempool_recovery_file(node_info);
    wait_for_transaction_in_recovery_file(node_name, transaction_alias, tx_hash, recovery_file)
        .await
}

async fn collect_pending_mempool_hashes(
    node_info: &NodeInfo,
) -> Result<BTreeSet<TxHash>, StepError> {
    Ok(node_info
        .started_node
        .client
        .test_mempool_view()
        .await?
        .into_iter()
        .collect())
}

fn mempool_recovery_file(node_info: &NodeInfo) -> PathBuf {
    node_info
        .runtime_dir
        .join("recovery")
        .join("mempool")
        .join("recovery.json")
}

async fn wait_for_transaction_in_recovery_file(
    node_name: &str,
    transaction_alias: &str,
    tx_hash: TxHash,
    recovery_file: PathBuf,
) -> Result<(), StepError> {
    let wait_result = timeout(RECOVERY_FLUSH_TIMEOUT, async {
        loop {
            let recovered_hashes = read_recovered_mempool_pending_hashes(&recovery_file)?;

            if recovered_hashes
                .as_ref()
                .is_some_and(|hashes| hashes.contains(&tx_hash))
            {
                return Ok(());
            }

            sleep(RECOVERY_FLUSH_POLL_INTERVAL).await;
        }
    })
    .await;

    wait_result.unwrap_or_else(
        |_| Err(StepError::Timeout {
            message: format!(
                "Timed out waiting for node `{node_name}` to flush transaction `{transaction_alias}` ({tx_hash:?}) to '{}'",
                recovery_file.display()
            ),
        }),
    )
}

fn read_recovered_mempool_pending_hashes(
    recovery_file: &Path,
) -> Result<Option<BTreeSet<TxHash>>, StepError> {
    let contents = match fs::read_to_string(recovery_file) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };

    let recovery_state: TxMempoolState<PoolRecoveryState<TxHash>, (), ()> =
        serde_json::from_str(&contents).map_err(|error| StepError::LogicalError {
            message: format!(
                "Failed to parse mempool recovery file '{}': {error}",
                recovery_file.display()
            ),
        })?;

    Ok(recovery_state.pool().map(|pool| pool.pending_items.clone()))
}

fn wallet_transaction_error(error: &WalletTransactionError) -> StepError {
    StepError::LogicalError {
        message: error.to_string(),
    }
}

fn receiver_public_key(
    world: &CucumberWorld,
    step: &str,
    receiver_wallet_name: &str,
) -> Result<ZkPublicKey, StepError> {
    let receiver = world
        .resolve_wallet(receiver_wallet_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;

    receiver.public_key().inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })
}

fn transfer_intent(
    receiver_pk: ZkPublicKey,
    amount: u64,
) -> Result<WalletTransactionIntent, StepError> {
    WalletTransactionIntent::transfer(&[(receiver_pk, amount)], DEFAULT_STORAGE_GAS_PRICE)
        .map_err(|error| wallet_transaction_error(&error))
}

fn record_prepared_transaction(
    world: &mut CucumberWorld,
    transaction_alias: String,
    signed: &SignedUserWalletSubmission,
) -> Result<TxHash, StepError> {
    let tx_hash = signed.tx_hash();

    record_signed_user_wallet_submission(world, signed)?;
    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);
    world.remember_prepared_transaction(transaction_alias, signed.signed_tx().clone());

    Ok(tx_hash)
}

async fn submit_prepared_transaction_to_node(
    world: &CucumberWorld,
    step: &str,
    transaction_alias: &str,
    signed_tx: &SignedMantleTx,
    tx_hash: TxHash,
    node_name: &str,
) -> Result<(), StepError> {
    let node = world.resolve_node_http_client(node_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    node.submit_transaction(signed_tx).await.inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    info!(
        target: TARGET,
        "Submitted prepared transaction `{transaction_alias}` ({:?}) to `{node_name}`",
        tx_hash
    );

    Ok(())
}

fn report_prepared_transaction(
    transaction_alias: &str,
    tx_hash: TxHash,
    sender_wallet_name: &str,
    receiver_wallet_name: &str,
) {
    info!(
        target: TARGET,
        "Prepared transfer transaction `{transaction_alias}` ({:?}) from `{sender_wallet_name}` to `{receiver_wallet_name}`",
        tx_hash
    );
}
