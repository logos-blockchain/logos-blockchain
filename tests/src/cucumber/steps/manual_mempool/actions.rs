use std::{collections::BTreeSet, time::Duration};

use lb_core::{
    codec::DeserializeOp as _,
    mantle::{
        SignedOps, TxHash,
        ledger::verification_mode::StandardMode,
        traits::Hashable as _,
        transactions::{OpProofs, states::Preverified},
    },
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_storage_service::{
    backends::rocksdb::RocksBackendSettings,
    recovery::{load_recovery_data, recovery_key},
};
use lb_testing_framework::USER_CONFIG_FILE;
use lb_tx_service::{
    backend::PoolRecoveryState,
    tx::{settings::RECOVERY_KEY_SUFFIX, state::TxMempoolState},
};
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use crate::{
    common::wallet::{WalletTransactionError, WalletTransactionIntent},
    cucumber::{
        error::StepError,
        steps::{
            TARGET, manual_transactions::tracked_transactions::create_stateless_invalid_transaction,
        },
        utils::{tx_hash_to_hex, user_config_from_node_yaml},
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
    let prepared =
        prepare_user_wallet_transaction_submission(world, step, &sender_wallet_name, intent, None)
            .await?;
    let signed = sign_prepared_user_wallet_transaction(step, prepared, OpProofs::empty())?;
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

/// Hands a prepared transaction to one node's Blend service rather than its
/// mempool.
///
/// Only that node is told, and it does not gossip the transaction itself, so
/// anything that later observes it anywhere came back through Blend.
pub async fn submit_prepared_transaction_through_blend(
    world: &CucumberWorld,
    step: &str,
    transaction_alias: &str,
    node_name: &str,
) -> Result<(), StepError> {
    let signed_tx = world.resolve_prepared_transaction(transaction_alias)?;
    let node = world.resolve_node_http_client(node_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    let tx_hash = node.blend_transaction(signed_tx).await.inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    info!(
        target: TARGET,
        "Submitted prepared transaction `{transaction_alias}` ({}) through Blend on `{node_name}`",
        tx_hash_to_hex(&tx_hash)
    );

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
    let signed_tx = create_stateless_invalid_transaction();
    let tx_hash = signed_tx.hash();

    world.remember_submitted_transaction(transaction_alias.clone(), tx_hash);

    match node.submit_transaction(&signed_tx).await {
        Ok(()) => {
            info!(
                target: TARGET,
                "Submitted invalid transaction `{transaction_alias}` ({}) to `{node_name}`",
                tx_hash_to_hex(&tx_hash)
            );
        }
        Err(error) => {
            info!(
                target: TARGET,
                "Invalid transaction `{transaction_alias}` ({}) was rejected by `{node_name}`: {error}",
                tx_hash_to_hex(&tx_hash)
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
                "Transaction `{transaction_alias}` ({}) is not pending in node `{node_name}` mempool",
                tx_hash_to_hex(&tx_hash)
            ),
        });
    }

    wait_for_transaction_in_recovery_data(node_name, transaction_alias, tx_hash, node_info).await
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

async fn wait_for_transaction_in_recovery_data(
    node_name: &str,
    transaction_alias: &str,
    tx_hash: TxHash,
    node_info: &NodeInfo,
) -> Result<(), StepError> {
    let storage_settings = recovery_storage_settings(node_info)?;
    let wait_result = timeout(RECOVERY_FLUSH_TIMEOUT, async {
        loop {
            let recovered_hashes = read_recovered_mempool_pending_hashes(storage_settings.clone())?;

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

    wait_result.unwrap_or_else(|_| {
        Err(StepError::Timeout {
            message: format!(
                "Timed out waiting for node `{node_name}` to flush transaction \
                `{transaction_alias}` ({}) to '{}'",
                tx_hash_to_hex(&tx_hash),
                storage_settings.db_path.display()
            ),
        })
    })
}

fn recovery_storage_settings(node_info: &NodeInfo) -> Result<RocksBackendSettings, StepError> {
    let config = user_config_from_node_yaml(&node_info.runtime_dir.join(USER_CONFIG_FILE))?;

    Ok(RocksBackendSettings {
        db_path: node_info
            .runtime_dir
            .join(&config.storage.backend.folder_name),
        read_only: true,
        column_family: config.storage.backend.column_family,
    })
}

fn read_recovered_mempool_pending_hashes(
    storage_settings: RocksBackendSettings,
) -> Result<Option<BTreeSet<TxHash>>, StepError> {
    if !storage_settings.db_path.exists() {
        return Ok(None);
    }

    let recovery_data =
        load_recovery_data(storage_settings).map_err(|error| StepError::LogicalError {
            message: format!("Failed to read mempool recovery data: {error}"),
        })?;

    let Some(bytes) = recovery_data
        .take(&recovery_key(RECOVERY_KEY_SUFFIX))
        .map_err(|error| StepError::LogicalError {
            message: format!("Failed to access mempool recovery data: {error}"),
        })?
    else {
        return Ok(None);
    };

    let recovery_state: TxMempoolState<PoolRecoveryState<TxHash>, (), ()> =
        TxMempoolState::from_bytes(&bytes).map_err(|error| StepError::LogicalError {
            message: format!("Failed to decode mempool recovery data: {error}"),
        })?;

    Ok(recovery_state
        .pool()
        .map(|pool| pool.pending_items.keys().copied().collect()))
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
    WalletTransactionIntent::transfer(&[(receiver_pk, amount)])
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
    signed_tx: &SignedOps<Preverified, StandardMode>,
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
        "Submitted prepared transaction `{transaction_alias}` ({}) to `{node_name}`",
        tx_hash_to_hex(&tx_hash)
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
        "Prepared transfer transaction `{transaction_alias}` ({}) from `{sender_wallet_name}` to `{receiver_wallet_name}`",
        tx_hash_to_hex(&tx_hash)
    );
}
