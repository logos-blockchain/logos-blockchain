use std::collections::BTreeMap;

use lb_common_http_client::Error as HttpClientError;
use lb_core::mantle::Utxo;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_testing_framework::is_truthy_env;
use tracing::{info, warn};

use crate::{
    common::wallet::{
        NodeHttpWalletChainSource, WalletBalance, WalletId, WalletOutputState, WalletSyncBatch,
        WalletSyncError, WalletSyncRequests, WalletSyncedBlock, WalletSyncedOutput,
        WalletSyncedSpend, WalletUtxos, sync_batches_from_chain,
    },
    cucumber::{
        defaults::CUCUMBER_VERBOSE_CONSOLE,
        error::StepError,
        fee_reserve::{SCENARIO_FEE_ACCOUNT_NAME, ScenarioFeeState},
        wallet::{
            TARGET, WalletStateView,
            best_node::{BestNodeInfo, sanitize_best_node_info_for_group},
        },
        world::{CucumberWorld, WalletInfo},
    },
};

pub async fn sync_available_utxos_for_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    let wallets = world.all_user_wallets();
    let mut requests = build_wallet_sync_requests(world, step, &wallets)?;
    add_scenario_fee_wallet_request(world, &mut requests);
    scan_available_utxos(world, step, &requests, best_node_info).await
}

pub async fn sync_available_utxos_for_funding_wallets(
    world: &mut CucumberWorld,
    step: &str,
    _best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    let mut funding_wallet_utxos = WalletUtxos::new();
    for wallet in world.all_funding_wallets() {
        let utxos = sync_wallet(world, step, &wallet.wallet_name)
            .await?
            .into_available_utxos();
        funding_wallet_utxos.insert(wallet.wallet_name.clone().into(), utxos);
    }
    Ok(funding_wallet_utxos)
}

pub async fn sync_available_utxos_for_all_wallets(
    world: &mut CucumberWorld,
    step: &str,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    let mut all_wallet_utxos =
        sync_available_utxos_for_wallets(world, step, world.all_user_wallets(), best_node_info)
            .await?;
    for wallet in world.all_funding_wallets() {
        let utxos = sync_wallet(world, step, &wallet.wallet_name)
            .await?
            .into_available_utxos();
        all_wallet_utxos.insert(wallet.wallet_name.clone().into(), utxos);
    }
    Ok(all_wallet_utxos)
}

pub async fn sync_available_utxos_for_wallets(
    world: &mut CucumberWorld,
    step: &str,
    wallets: Vec<WalletInfo>,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    for wallet in &wallets {
        if wallet.is_funding_wallet() {
            return Err(StepError::LogicalError {
                message: format!(
                    "Funding wallet {} should be updated individually due to their strict coupling \
                    with their node's state.",
                    wallet.wallet_name
                ),
            });
        }
    }

    let requests = build_wallet_sync_requests(world, step, &wallets)?;
    scan_available_utxos(world, step, &requests, best_node_info).await
}

pub async fn sync_available_utxos_for_wallet(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> Result<Vec<Utxo>, StepError> {
    Ok(sync_wallet(world, step, wallet_name)
        .await?
        .into_available_utxos())
}

pub async fn sync_wallet_output_balance(
    world: &mut CucumberWorld,
    _step: &str,
    wallet: &WalletInfo,
    wallet_name: &str,
    wallet_state_type: WalletOutputState,
) -> Result<WalletBalance, StepError> {
    sync_wallet_state_from_chain(world, wallet_name, &wallet.node_name, wallet.public_key()?)
        .await
        .map(|observation| observation.balance(wallet_state_type))
}

pub async fn sync_wallet_balance(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    wallet_state_type: WalletOutputState,
) -> Result<WalletBalance, StepError> {
    Ok(sync_wallet(world, step, wallet_name)
        .await?
        .balance(wallet_state_type))
}

pub async fn sync_wallet_state_from_chain(
    world: &mut CucumberWorld,
    wallet_name: &str,
    wallet_node_name: &str,
    wallet_pk: ZkPublicKey,
) -> Result<WalletStateView, StepError> {
    let source = wallet_source_from_node(world, wallet_name, wallet_node_name).await?;
    let mut requests = WalletSyncRequests::new();
    requests.add_wallet("", wallet_name, wallet_pk);

    let sync_batch = requests
        .sync_batches()
        .next()
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Cannot scan wallet `{wallet_name}` without a wallet sync request"),
        })?;
    let genesis_utxos = world.genesis_block_utxos.clone();
    let sync_results = sync_batches_from_chain(
        &mut world.wallets,
        [(sync_batch.clone(), source)],
        &genesis_utxos,
    )
    .await
    .map_err(wallet_sync_error)?;

    for synced_block in sync_results.synced_blocks() {
        log_wallet_synced_block(synced_block);
    }
    let on_chain_utxos = sync_results.into_wallet_utxos();

    let mut observations = world.wallets.observe_synced_wallets(&requests);
    apply_scenario_fee_observations(world, &mut observations, &on_chain_utxos);

    observations
        .remove(wallet_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Wallet state observation for `{wallet_name}` was not returned"),
        })
}

fn build_wallet_sync_requests(
    world: &CucumberWorld,
    step: &str,
    wallets: &[WalletInfo],
) -> Result<WalletSyncRequests, StepError> {
    let mut sync_batches = WalletSyncRequests::new();

    for wallet in wallets {
        let group_key = world
            .node_to_group
            .get(&wallet.node_name)
            .cloned()
            .unwrap_or_default();
        let wallet_pk = wallet.public_key().inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

        sync_batches.add_wallet(group_key, wallet.wallet_name.clone(), wallet_pk);
    }

    Ok(sync_batches)
}

fn add_scenario_fee_wallet_request(world: &CucumberWorld, requests: &mut WalletSyncRequests) {
    let Some(fee_wallet_account) = world.fee_state.wallet_account.clone() else {
        return;
    };

    requests.add_wallet_for_each_source(SCENARIO_FEE_ACCOUNT_NAME, fee_wallet_account.public_key());
}

async fn scan_available_utxos(
    world: &mut CucumberWorld,
    step: &str,
    requests: &WalletSyncRequests,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    let mut sync_sources = Vec::new();
    for sync_batch in requests.sync_batches() {
        let source = wallet_source_from_best_node(world, &sync_batch, best_node_info)
            .await
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{}` error: {e}", step);
            })?;

        sync_sources.push((sync_batch, source));
    }

    let genesis_utxos = world.genesis_block_utxos.clone();
    let sync_results = sync_batches_from_chain(&mut world.wallets, sync_sources, &genesis_utxos)
        .await
        .map_err(wallet_sync_error)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

    for synced_block in sync_results.synced_blocks() {
        log_wallet_synced_block(synced_block);
    }
    let on_chain_utxos = sync_results.into_wallet_utxos();

    let mut observations = world.wallets.observe_synced_wallets(requests);
    apply_scenario_fee_observations(world, &mut observations, &on_chain_utxos);

    Ok(observations
        .into_iter()
        .map(|(wallet_name, observation)| (wallet_name, observation.into_available_utxos()))
        .collect())
}

fn log_wallet_balance(wallet_name: &str, observation: &WalletStateView) {
    let available = observation.balance(WalletOutputState::Available);
    let reserved = observation.balance(WalletOutputState::Reserved);
    let on_chain = observation.balance(WalletOutputState::OnChain);
    info!(
        target: TARGET,
        "Wallet `{wallet_name}` [Available] {}/{} LGO, \
        [Encumbered] {}/{} LGO, \
        [On-chain] {}/{} LGO",
        available.output_count,
        available.value,
        reserved.output_count,
        reserved.value,
        on_chain.output_count,
        on_chain.value,
    );
}

async fn sync_wallet(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> Result<WalletStateView, StepError> {
    let wallet = world.resolve_wallet(wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    let state_observation =
        sync_wallet_state_from_chain(world, wallet_name, &wallet.node_name, wallet.public_key()?)
            .await
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{}` error: {e}", step);
            })?;
    log_wallet_balance(wallet_name, &state_observation);

    Ok(state_observation)
}

async fn wallet_source_from_node(
    world: &CucumberWorld,
    wallet_name: &str,
    node_name: &str,
) -> Result<NodeHttpWalletChainSource, StepError> {
    let client = world
        .nodes_info
        .get(node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{node_name}' for wallet '{wallet_name}' not found"),
        })?
        .started_node
        .client
        .clone();

    Ok(NodeHttpWalletChainSource::from_client(node_name.to_owned(), client).await?)
}

async fn wallet_source_from_best_node(
    world: &CucumberWorld,
    sync_batch: &WalletSyncBatch,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<NodeHttpWalletChainSource, StepError> {
    let (node_name, client, consensus) =
        sanitize_best_node_info_for_group(world, sync_batch.source_id(), best_node_info).await?;

    Ok(NodeHttpWalletChainSource::from_tip(
        node_name,
        client.clone(),
        consensus.tip,
    ))
}

fn log_wallet_synced_block(block: &WalletSyncedBlock) {
    if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
        info!(
            target: TARGET,
            "Evaluating block {height_prefix}{height} for {} wallets on `{}`: {header_id}, \
            transactions len: {}",
            block.wallet_count(),
            block.source_node_name(),
            block.transaction_count(),
            height_prefix = block.height_log_prefix(),
            height = block.height_value(),
            header_id = block.header_id(),
        );
    }

    log_wallet_synced_outputs(block.synced_outputs());
    log_wallet_synced_spends(block.synced_spends());
}

fn wallet_sync_error(error: WalletSyncError<HttpClientError>) -> StepError {
    match error {
        WalletSyncError::Request(error) => StepError::LogicalError {
            message: error.to_string(),
        },
        WalletSyncError::FetchBlock(error) => StepError::from(error),
    }
}

fn log_wallet_synced_outputs(synced_outputs: &[WalletSyncedOutput]) {
    if !is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
        return;
    }

    for found in synced_outputs {
        info!(
            target: TARGET,
            "Found UTXO for `{}`: value: {}, id: {:?}",
            found.wallet_id,
            found.utxo.note.value,
            found.utxo.id(),
        );
    }
}

fn log_wallet_synced_spends(synced_spends: &[WalletSyncedSpend]) {
    for spent in synced_spends {
        if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
            info!(
                target: TARGET,
                "Found spent UTXO for `{}`: id: {:?}",
                spent.wallet_id,
                spent.note_id,
            );
        }
    }
}

fn apply_scenario_fee_observations(
    world: &CucumberWorld,
    observations: &mut BTreeMap<WalletId, WalletStateView>,
    on_chain_utxos: &WalletUtxos,
) {
    let fee_wallet_ids = observations
        .keys()
        .filter(|wallet_id| ScenarioFeeState::owns_wallet_name(wallet_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();

    for wallet_id in fee_wallet_ids {
        let wallet_on_chain_utxos = on_chain_utxos
            .get(wallet_id.as_str())
            .cloned()
            .unwrap_or_default();
        observations.insert(
            wallet_id.clone(),
            world
                .fee_state
                .state_observation_for(wallet_id, wallet_on_chain_utxos),
        );
    }
}
