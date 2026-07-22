use std::{
    collections::{BTreeMap, HashSet},
    hash::BuildHasher,
    time::Duration,
};

use lb_core::mantle::{NoteId, Utxo};
use lb_key_management_system_service::keys::ZkPublicKey;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::{
    common::wallet::{
        TrackedWalletKeysBySource, WalletBalance, WalletId, WalletOutputState, WalletUtxos,
    },
    cucumber::{
        error::StepError,
        fee_reserve::{SCENARIO_FEE_ACCOUNT_NAME, ScenarioFeeState},
        wallet::{
            TARGET, WalletStateView,
            best_node::{BestNodeInfo, get_best_node_info},
        },
        world::{CucumberWorld, WalletInfo},
    },
};

const CURRENT_WALLET_STATE_CATCH_UP_TIMEOUT: Duration = Duration::from_secs(30);

/// Return currently available UTXOs for all user wallets.
///
/// The result comes from TF wallet tracking, not directly from the node wallet
/// API. Before reading it, this waits for wallet-feed sources to reach the
/// minimum heights required by the wallets' connected nodes.
pub async fn current_available_utxos_for_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
) -> Result<WalletUtxos, StepError> {
    let wallets = world.all_user_wallets();
    let mut wallet_keys = build_tracked_wallet_keys(world, step, &wallets)?;
    add_scenario_fee_wallet_keys(world, &mut wallet_keys);

    current_available_utxos_for_wallet_keys_with_requirements(world, &wallet_keys).await
}

/// Return currently available UTXOs for funding wallets.
///
/// Funding wallets use the same tracked-wallet state as user wallets, but are
/// selected from the funding-wallet namespace.
pub async fn current_available_utxos_for_funding_wallets(
    world: &mut CucumberWorld,
    step: &str,
) -> Result<WalletUtxos, StepError> {
    current_available_utxos_for_named_wallets(world, step, world.all_funding_wallets()).await
}

/// Return currently available UTXOs for one named wallet.
pub async fn current_available_utxos_for_wallet(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> Result<Vec<Utxo>, StepError> {
    Ok(current_wallet_state(world, step, wallet_name)
        .await?
        .into_available_utxos())
}

/// Return one balance view for a resolved wallet.
pub async fn current_wallet_output_balance(
    world: &mut CucumberWorld,
    _step: &str,
    wallet: &WalletInfo,
    wallet_state_type: WalletOutputState,
) -> Result<WalletBalance, StepError> {
    current_wallet_state_for_wallet(world, wallet)
        .await
        .map(|observation| observation.balance(wallet_state_type))
}

/// Return one balance view for a wallet name.
pub async fn current_wallet_balance(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    wallet_state_type: WalletOutputState,
) -> Result<WalletBalance, StepError> {
    Ok(current_wallet_state(world, step, wallet_name)
        .await?
        .balance(wallet_state_type))
}

/// Return wallet-state views for the provided wallets.
///
/// This waits only for the feed sources needed by these wallets, then reads the
/// tracked state for their keys.
pub async fn current_wallet_states_for_wallets(
    world: &mut CucumberWorld,
    step: &str,
    wallets: &[WalletInfo],
) -> Result<BTreeMap<WalletId, WalletStateView>, StepError> {
    let wallet_keys = build_tracked_wallet_keys(world, step, wallets)?;

    current_wallet_state_views(world, &wallet_keys).await
}

/// Return the current tracked state for one wallet, including reserved outputs.
pub async fn current_wallet_available_state(
    world: &mut CucumberWorld,
    wallet_name: &str,
) -> Result<WalletStateView, StepError> {
    let wallet = world.resolve_wallet(wallet_name)?;
    current_wallet_state_for_wallet(world, &wallet).await
}

async fn current_available_utxos_for_named_wallets(
    world: &mut CucumberWorld,
    step: &str,
    wallets: Vec<WalletInfo>,
) -> Result<WalletUtxos, StepError> {
    let wallet_keys = build_tracked_wallet_keys(world, step, &wallets)?;

    current_available_utxos_for_wallet_keys_with_requirements(world, &wallet_keys).await
}

async fn current_available_utxos_for_wallet_keys_with_requirements(
    world: &mut CucumberWorld,
    wallet_keys: &TrackedWalletKeysBySource,
) -> Result<WalletUtxos, StepError> {
    let observations = current_wallet_state_views(world, wallet_keys).await?;

    Ok(observations
        .into_iter()
        .map(|(wallet_name, observation)| (wallet_name, observation.into_available_utxos()))
        .collect())
}

pub async fn current_wallet_state(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> Result<WalletStateView, StepError> {
    let wallet = world.resolve_wallet(wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    current_wallet_state_for_wallet(world, &wallet).await
}

async fn current_wallet_state_for_wallet(
    world: &mut CucumberWorld,
    wallet: &WalletInfo,
) -> Result<WalletStateView, StepError> {
    let wallet_keys = build_tracked_wallet_keys(world, "", std::slice::from_ref(wallet))?;

    current_wallet_state_for_keys(world, wallet.wallet_name.as_str(), &wallet_keys).await
}

pub async fn current_wallet_state_for_key(
    world: &mut CucumberWorld,
    wallet_name: &str,
    wallet_pk: ZkPublicKey,
) -> Result<WalletStateView, StepError> {
    let wallet_keys = tracked_wallet_keys_for_all_active_sources(world, wallet_name, wallet_pk);

    current_wallet_state_for_keys(world, wallet_name, &wallet_keys).await
}

async fn current_wallet_state_for_keys(
    world: &mut CucumberWorld,
    wallet_name: &str,
    wallet_keys: &TrackedWalletKeysBySource,
) -> Result<WalletStateView, StepError> {
    let observations = current_wallet_state_views(world, wallet_keys).await?;

    observations
        .into_values()
        .next()
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Wallet state for `{wallet_name}` is not tracked"),
        })
}

async fn current_wallet_state_views(
    world: &mut CucumberWorld,
    wallet_keys: &TrackedWalletKeysBySource,
) -> Result<BTreeMap<WalletId, WalletStateView>, StepError> {
    world
        .wait_for_wallet_scanner_catch_up(CURRENT_WALLET_STATE_CATCH_UP_TIMEOUT)
        .await?;

    let mut observations = world.with_wallets(|wallets| {
        wallets.current_wallet_states(wallet_keys.wallet_keys().cloned())
    })?;
    let on_chain_utxos = observations
        .iter()
        .map(|(wallet_id, observation)| (wallet_id.clone(), observation.on_chain_utxos().to_vec()))
        .collect();

    apply_scenario_fee_observations(world, &mut observations, &on_chain_utxos);

    Ok(observations)
}

fn build_tracked_wallet_keys(
    world: &CucumberWorld,
    step: &str,
    wallets: &[WalletInfo],
) -> Result<TrackedWalletKeysBySource, StepError> {
    let mut wallet_keys = TrackedWalletKeysBySource::new();

    for wallet in wallets {
        let group_key = world
            .node_to_group
            .get(&wallet.node_name)
            .cloned()
            .unwrap_or_default();
        let wallet_pk = wallet.public_key().inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

        wallet_keys.add_wallet(group_key, wallet.wallet_name.clone(), wallet_pk);
    }

    Ok(wallet_keys)
}

fn tracked_wallet_keys_for_all_active_sources(
    world: &CucumberWorld,
    wallet_name: &str,
    wallet_pk: ZkPublicKey,
) -> TrackedWalletKeysBySource {
    let mut wallet_keys = TrackedWalletKeysBySource::new();

    for source_id in active_wallet_feed_source_ids(world) {
        wallet_keys.add_wallet(source_id, wallet_name, wallet_pk);
    }

    if wallet_keys.batches().next().is_none() {
        wallet_keys.add_wallet("", wallet_name, wallet_pk);
    }

    wallet_keys
}

fn active_wallet_feed_source_ids(world: &CucumberWorld) -> Vec<String> {
    world
        .nodes_info
        .keys()
        .map(|node_name| wallet_feed_source_id(world, node_name).to_owned())
        .collect()
}

fn wallet_feed_source_id<'a>(world: &'a CucumberWorld, node_name: &str) -> &'a str {
    world
        .node_to_group
        .get(node_name)
        .map_or("", String::as_str)
}

fn add_scenario_fee_wallet_keys(
    world: &CucumberWorld,
    wallet_keys: &mut TrackedWalletKeysBySource,
) {
    let Some(fee_wallet_account) = world.fee_state.wallet_account.clone() else {
        return;
    };

    wallet_keys
        .add_wallet_for_each_source(SCENARIO_FEE_ACCOUNT_NAME, fee_wallet_account.public_key());
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

/// Wait until a wallet can safely produce a transaction with the requested
/// value/output shape.
///
/// The check combines majority-tip selection, tracked-wallet state, already
/// reserved inputs, and optional per-transaction UTXO size requirements.
#[expect(clippy::too_many_arguments, reason = "Need all args")]
pub async fn wait_wallet_send_ready<S: BuildHasher + Sync>(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    timeout_seconds: u64,
    required_available: u64,
    readiness: WalletSendReadiness,
    available_utxos: &mut WalletUtxos,
    used_input_note_ids: &HashSet<NoteId, S>,
) -> Result<BestNodeInfo, StepError> {
    let start = tokio::time::Instant::now();

    let mut last_available_value = 0u64;
    let mut last_available_outputs = 0usize;
    let mut last_eligible_value = 0u64;
    let mut last_eligible_outputs = 0usize;

    let (min_required_outputs, min_value_per_transaction) = readiness.get_minimums();
    let mut last_msg = String::new();

    while start.elapsed() < Duration::from_secs(timeout_seconds) {
        // Ensure we have a converged majority before proceeding
        let best_node_info = get_best_node_info(world, wallet_name, Some(&mut last_msg)).await?;
        let fresh_wallet_state = current_wallet_state(world, step, wallet_name).await?;
        let available = fresh_wallet_state.balance(WalletOutputState::Available);

        let raw_available_utxos = fresh_wallet_state.into_available_utxos();
        let filtered_count = raw_available_utxos
            .iter()
            .filter(|utxo| used_input_note_ids.contains(&utxo.id()))
            .count();
        if filtered_count > 0 {
            warn!(
                target: TARGET,
                "Wallet '{wallet_name}' readiness filtered {filtered_count} previously used UTXO(s) \
                from {} available candidate(s)", available.output_count
            );
        }
        let mut fresh_available_utxos = raw_available_utxos
            .into_iter()
            .filter(|utxo| !used_input_note_ids.contains(&utxo.id()))
            .collect::<Vec<_>>();

        // Prefer smaller UTXOs first. The transaction builder selects sufficient
        // inputs, so this helps consume smaller/dustier outputs before large
        // change-like outputs.
        fresh_available_utxos.sort_by_key(|utxo| utxo.note.value);

        last_available_outputs = fresh_available_utxos.len();
        last_available_value = fresh_available_utxos
            .iter()
            .map(|utxo| utxo.note.value)
            .sum();

        let mut eligible_outputs = 0usize;
        let mut eligible_value = 0u64;

        for utxo in fresh_available_utxos
            .iter()
            .filter(|utxo| utxo.note.value >= min_value_per_transaction)
        {
            eligible_outputs += 1;
            eligible_value += utxo.note.value;
        }

        let is_ready =
            eligible_outputs >= min_required_outputs && eligible_value >= required_available;

        if is_ready {
            available_utxos.insert(wallet_name.to_owned().into(), fresh_available_utxos);

            update_fee_wallet_cache(world, step, wallet_name, available_utxos).await?;
            log_available_utxos(available_utxos, wallet_name);

            return Ok(best_node_info);
        }

        last_eligible_value = eligible_value;
        last_eligible_outputs = eligible_outputs;

        sleep(Duration::from_millis(300)).await;
    }

    Err(StepError::StepFail {
        message: format!(
            "Timed out waiting for wallet '{wallet_name}' send readiness: \
            required {min_required_outputs} eligible UTXO(s) with value >= \
            {min_value_per_transaction} and eligible value >= {required_available}; \
            last observed: available={last_available_outputs}/{last_available_value}, \
            eligible={last_eligible_outputs}/{last_eligible_value}"
        ),
    })
}

async fn update_fee_wallet_cache(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    available_utxos: &mut WalletUtxos,
) -> Result<(), StepError> {
    if world.fee_state.wallet_account.is_some() {
        let wallet = world.resolve_wallet(wallet_name)?;
        let group_key = world
            .node_to_group
            .get(&wallet.node_name)
            .cloned()
            .unwrap_or_default();
        let fee_wallet_name = ScenarioFeeState::wallet_name_for_group(&group_key);
        let fresh_available_utxos = current_available_utxos_for_user_wallets(world, step).await?;
        let fee_wallet_utxos = fresh_available_utxos.get(fee_wallet_name.as_str()).cloned().ok_or_else(|| {
            StepError::LogicalError {
                message: format!(
                    "Scenario fee account state for wallet '{wallet_name}' is invalid: \
                            scenario fee account state `{fee_wallet_name}` not found in grouped scan"
                ),
            }
        })?;
        available_utxos.insert(fee_wallet_name.clone().into(), fee_wallet_utxos);
        log_available_utxos(available_utxos, &fee_wallet_name);
    }
    Ok(())
}

fn log_available_utxos(available_utxos: &WalletUtxos, wallet_name: &str) {
    if let Some(wallet_available) = available_utxos.get(wallet_name) {
        info!(
            target: TARGET,
            "Wallet `{wallet_name}` has {} available UTXOs with total value of {} LGO",
            wallet_available.len(), wallet_available.iter()
                .map(|utxo| utxo.note.value)
                .sum::<u64>()
        );
    }
}

/// Indicates the readiness of a wallet to send transactions, either by having a
/// sufficient batch of eligible UTXOs or by having a total value available.
#[derive(Clone, Copy, Debug)]
pub enum WalletSendReadiness {
    EligibleUtxoBatch {
        min_required_outputs: usize,
        min_value_per_transaction: u64,
    },
    TotalValueOnly,
}

impl WalletSendReadiness {
    /// Helper function to destructure minimum readiness requirements
    #[must_use]
    pub const fn get_minimums(&self) -> (usize, u64) {
        match self {
            Self::EligibleUtxoBatch {
                min_required_outputs,
                min_value_per_transaction,
            } => (*min_required_outputs, *min_value_per_transaction),
            Self::TotalValueOnly => (1, 1),
        }
    }
}
