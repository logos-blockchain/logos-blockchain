use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    hash::BuildHasher,
    time::{Duration, Instant},
};

use lb_core::mantle::{NoteId, Utxo};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_testing_framework::{BlockFeed, NodeHttpClient};
use tokio::time::{sleep, timeout};
use tracing::{debug, info, warn};

use crate::{
    common::wallet::{
        NodeHttpWalletChainSource, TrackedWalletKeysBySource, WalletBalance,
        WalletBlockFeedTrackerError, WalletFeedTrackingBatch, WalletId, WalletOutputState,
        WalletUtxos, wallet_utxos_from_chain,
    },
    cucumber::{
        error::StepError,
        fee_reserve::{SCENARIO_FEE_ACCOUNT_NAME, ScenarioFeeState},
        wallet::{
            TARGET, WalletStateView,
            best_node::{BestNodeInfo, get_best_node_info},
            feed::record_observed_transaction_hashes,
        },
        world::{CucumberWorld, WalletInfo},
    },
};

const CURRENT_WALLET_STATE_CATCH_UP_TIMEOUT: Duration = Duration::from_secs(30);
const WALLET_BLOCK_FEED_SOURCE_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const WALLET_BACKFILL_FALLBACK_SELECTION_TIMEOUT: Duration = Duration::from_secs(2);

struct WalletFeedSourceRequirement {
    node_name: String,
    min_height: u64,
}

pub async fn current_available_utxos_for_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
) -> Result<WalletUtxos, StepError> {
    let wallets = world.all_user_wallets();
    let feed_requirements = wallet_feed_source_requirements(world, &wallets).await?;
    let mut wallet_keys = build_tracked_wallet_keys(world, step, &wallets)?;
    add_scenario_fee_wallet_keys(world, &mut wallet_keys);

    current_available_utxos_for_wallet_keys_with_requirements(
        world,
        &wallet_keys,
        feed_requirements,
    )
    .await
}

pub async fn current_available_utxos_for_funding_wallets(
    world: &mut CucumberWorld,
    step: &str,
) -> Result<WalletUtxos, StepError> {
    current_available_utxos_for_named_wallets(world, step, world.all_funding_wallets()).await
}

pub async fn current_available_utxos_for_all_wallets(
    world: &mut CucumberWorld,
    step: &str,
) -> Result<WalletUtxos, StepError> {
    let mut all_wallet_utxos =
        current_available_utxos_for_named_wallets(world, step, world.all_user_wallets()).await?;
    let funding_wallet_utxos = current_available_utxos_for_funding_wallets(world, step).await?;

    all_wallet_utxos.extend(funding_wallet_utxos);

    Ok(all_wallet_utxos)
}

pub async fn current_available_utxos_for_wallet(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> Result<Vec<Utxo>, StepError> {
    Ok(current_wallet_state(world, step, wallet_name)
        .await?
        .into_available_utxos())
}

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

pub async fn current_wallet_states_for_wallets(
    world: &mut CucumberWorld,
    step: &str,
    wallets: &[WalletInfo],
) -> Result<BTreeMap<WalletId, WalletStateView>, StepError> {
    let feed_requirements = wallet_feed_source_requirements(world, wallets).await?;
    let wallet_keys = build_tracked_wallet_keys(world, step, wallets)?;

    current_wallet_state_views(world, &wallet_keys, feed_requirements).await
}

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
    let feed_requirements = wallet_feed_source_requirements(world, &wallets).await?;
    let wallet_keys = build_tracked_wallet_keys(world, step, &wallets)?;

    current_available_utxos_for_wallet_keys_with_requirements(
        world,
        &wallet_keys,
        feed_requirements,
    )
    .await
}

async fn current_available_utxos_for_wallet_keys_with_requirements(
    world: &mut CucumberWorld,
    wallet_keys: &TrackedWalletKeysBySource,
    feed_requirements: Vec<WalletFeedSourceRequirement>,
) -> Result<WalletUtxos, StepError> {
    let observations = current_wallet_state_views(world, wallet_keys, feed_requirements).await?;

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
    let feed_requirements =
        wallet_feed_source_requirements(world, std::slice::from_ref(wallet)).await?;

    current_wallet_state_for_keys(
        world,
        wallet.wallet_name.as_str(),
        &wallet_keys,
        feed_requirements,
    )
    .await
}

pub async fn current_wallet_state_for_key(
    world: &mut CucumberWorld,
    wallet_name: &str,
    wallet_pk: ZkPublicKey,
) -> Result<WalletStateView, StepError> {
    let wallet_keys = tracked_wallet_keys_for_all_active_sources(world, wallet_name, wallet_pk);
    let feed_requirements = all_wallet_feed_source_requirements(world).await?;

    current_wallet_state_for_keys(world, wallet_name, &wallet_keys, feed_requirements).await
}

async fn current_wallet_state_for_keys(
    world: &mut CucumberWorld,
    wallet_name: &str,
    wallet_keys: &TrackedWalletKeysBySource,
    feed_requirements: Vec<WalletFeedSourceRequirement>,
) -> Result<WalletStateView, StepError> {
    let observations = current_wallet_state_views(world, wallet_keys, feed_requirements).await?;

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
    feed_requirements: Vec<WalletFeedSourceRequirement>,
) -> Result<BTreeMap<WalletId, WalletStateView>, StepError> {
    world.ensure_wallet_block_feed().await?;
    let feed = world.wallet_block_feed()?;
    let genesis_utxos = world.genesis_block_utxos.clone();
    let tracking_batches = wallet_feed_tracking_batches(world, wallet_keys, &feed_requirements);

    track_wallet_feed_batches_with_backfill(world, &tracking_batches, &genesis_utxos).await?;

    wait_for_wallet_feed_sources(world, &feed, feed_requirements).await?;

    let started_at = Instant::now();

    loop {
        apply_latest_wallet_feed_state(world, &feed, &tracking_batches, &genesis_utxos).await?;

        let observations = current_wallet_state_views_from_state(world, wallet_keys)?;

        if !wallet_states_waiting_for_reserved_outputs(&observations)
            || started_at.elapsed() >= CURRENT_WALLET_STATE_CATCH_UP_TIMEOUT
        {
            return Ok(observations);
        }

        wait_for_current_wallet_feed_cycle(&feed).await;
    }
}

fn current_wallet_state_views_from_state(
    world: &CucumberWorld,
    wallet_keys: &TrackedWalletKeysBySource,
) -> Result<BTreeMap<WalletId, WalletStateView>, StepError> {
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

pub(crate) async fn track_wallet_feed_batches_with_backfill(
    world: &CucumberWorld,
    tracking_batches: &[WalletFeedTrackingBatch],
    genesis_utxos: &[Utxo],
) -> Result<(), StepError> {
    if tracking_batches.is_empty() {
        return Ok(());
    }

    let tracking = world
        .with_wallet_feed_state_mut(|tracker, wallets| {
            tracker.track_wallets(wallets, tracking_batches, genesis_utxos)
        })?
        .map_err(wallet_feed_error)?;

    if !tracking.needs_backfill() {
        return Ok(());
    }

    backfill_wallet_feed_batches(world, tracking.backfill_batches(), genesis_utxos).await
}

async fn backfill_wallet_feed_batches(
    world: &CucumberWorld,
    tracking_batches: &[WalletFeedTrackingBatch],
    genesis_utxos: &[Utxo],
) -> Result<(), StepError> {
    for tracking_batch in tracking_batches {
        backfill_wallet_feed_batch(world, tracking_batch, genesis_utxos).await?;
    }

    Ok(())
}

async fn backfill_wallet_feed_batch(
    world: &CucumberWorld,
    tracking_batch: &WalletFeedTrackingBatch,
    genesis_utxos: &[Utxo],
) -> Result<(), StepError> {
    let source_node_name = tracking_batch.source_node_name().to_owned();
    let node = world
        .nodes_info
        .get(&source_node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Wallet block-feed source node `{source_node_name}` not found"),
        })?;
    let consensus = node.started_node.client.consensus_info().await?;
    let tip = consensus.cryptarchia_info.tip;
    let height = consensus.cryptarchia_info.height;
    let mut source = NodeHttpWalletChainSource::from_tip(
        source_node_name.clone(),
        node.started_node.client.clone(),
        tip,
    )
    .with_fallback(
        backfill_fallback_client(world, &source_node_name, &tip.to_string(), height).await?,
    );
    let (wallet_utxos, transaction_hashes, new_blocks) =
        wallet_utxos_from_chain(&mut source, tracking_batch.wallet_keys(), genesis_utxos)
            .await
            .map_err(|error| StepError::LogicalError {
                message: format!(
                    "Wallet chain backfill failed for source `{source_node_name}`: {error}"
                ),
            })?;
    let observed_utxos = wallet_utxos.clone();
    let tracked_utxos = wallet_utxos;
    let tip_string = tip.to_string();

    record_observed_transaction_hashes(
        &world.observed_transaction_hashes,
        &transaction_hashes,
        Some(new_blocks),
    );

    world
        .with_wallet_feed_state_mut(|tracker, wallets| {
            tracker.replace_source_state(
                source_node_name.clone(),
                tracking_batch.wallet_keys(),
                observed_utxos,
                tip,
                height,
            )?;
            wallets.record_header_height(&source_node_name, &tip_string, height);
            wallets.record_observed_wallets_utxos(tip_string, tracked_utxos);

            Ok(())
        })?
        .map_err(wallet_feed_error)
}

async fn backfill_fallback_client(
    world: &CucumberWorld,
    source_node_name: &str,
    expected_tip: &str,
    expected_height: u64,
) -> Result<Option<(String, NodeHttpClient)>, StepError> {
    let mut candidates = world.node_to_group.get(source_node_name).map_or_else(
        || world.all_node_names(),
        |group_name| {
            world
                .node_groups
                .get(group_name)
                .into_iter()
                .flat_map(|nodes| nodes.iter().cloned())
                .collect::<Vec<_>>()
        },
    );
    candidates.sort();

    for candidate_name in candidates {
        if candidate_name == source_node_name {
            continue;
        }

        let Some(candidate) = world.nodes_info.get(&candidate_name) else {
            continue;
        };

        let Ok(Ok(consensus)) = timeout(
            WALLET_BACKFILL_FALLBACK_SELECTION_TIMEOUT,
            candidate.started_node.client.consensus_info(),
        )
        .await
        else {
            continue;
        };

        if consensus.cryptarchia_info.height == expected_height
            && consensus.cryptarchia_info.tip.to_string() == expected_tip
        {
            return Ok(Some((
                candidate_name,
                candidate.started_node.client.clone(),
            )));
        }
    }

    Ok(None)
}

fn wallet_states_waiting_for_reserved_outputs(
    observations: &BTreeMap<WalletId, WalletStateView>,
) -> bool {
    observations.values().any(|observation| {
        observation
            .balance(WalletOutputState::Reserved)
            .output_count
            > 0
            && observation
                .balance(WalletOutputState::Available)
                .output_count
                == 0
    })
}

async fn wait_for_current_wallet_feed_cycle(feed: &BlockFeed) {
    let after_cycle = feed
        .latest_observation()
        .map_or(0, |observation| observation.cycle());

    if let Err(error) = feed
        .wait_for_next_cycle(after_cycle, Duration::from_secs(1))
        .await
    {
        debug!(target: TARGET, "Wallet block feed did not advance while waiting for current state: {error}");
    }
}

async fn wait_for_wallet_feed_sources(
    world: &CucumberWorld,
    feed: &BlockFeed,
    requirements: Vec<WalletFeedSourceRequirement>,
) -> Result<(), StepError> {
    if requirements.is_empty() {
        return Ok(());
    }

    let started_at = Instant::now();
    let mut after_cycle = feed
        .latest_observation()
        .map_or(0, |observation| observation.cycle());

    loop {
        if let Some(status) =
            update_wallet_feed_state_status(world, feed, &world.genesis_block_utxos)?
        {
            debug!(target: TARGET, "{status}");
        }

        if let Some(observation) = feed.latest_observation() {
            let pending_sources = pending_wallet_feed_sources(&observation, &requirements);
            if pending_sources.is_empty() {
                return Ok(());
            }

            if started_at.elapsed() >= WALLET_BLOCK_FEED_SOURCE_WAIT_TIMEOUT {
                return Err(wallet_feed_sources_timeout(
                    feed,
                    &observation,
                    &pending_sources,
                ));
            }

            after_cycle = observation.cycle();
        } else if started_at.elapsed() >= WALLET_BLOCK_FEED_SOURCE_WAIT_TIMEOUT {
            return Err(StepError::Timeout {
                message: wallet_feed_timeout_message(feed, "no block-feed observation yet"),
            });
        }

        if let Ok(observation) = feed
            .wait_for_next_cycle(after_cycle, Duration::from_secs(1))
            .await
        {
            after_cycle = observation.cycle();
        }
    }
}

fn pending_wallet_feed_sources(
    observation: &lb_testing_framework::BlockFeedObservation,
    requirements: &[WalletFeedSourceRequirement],
) -> Vec<String> {
    requirements
        .iter()
        .filter_map(|requirement| {
            let Some(head) = observation.node_head(&requirement.node_name) else {
                return Some(format!("{} missing", requirement.node_name));
            };

            match head.tip_height {
                Some(height) if height >= requirement.min_height => None,
                Some(height) => Some(format!(
                    "{} at {height}/{}",
                    requirement.node_name, requirement.min_height
                )),
                None => Some(format!(
                    "{} height unknown/{}",
                    requirement.node_name, requirement.min_height
                )),
            }
        })
        .collect()
}

fn wallet_feed_sources_timeout(
    feed: &BlockFeed,
    observation: &lb_testing_framework::BlockFeedObservation,
    pending_sources: &[String],
) -> StepError {
    StepError::Timeout {
        message: wallet_feed_timeout_message(
            feed,
            &format!(
                "pending sources [{}]; last observation: {}",
                pending_sources.join(", "),
                observation.summary(),
            ),
        ),
    }
}

fn wallet_feed_timeout_message(feed: &BlockFeed, details: &str) -> String {
    let last_error = feed.last_error().map_or_else(
        || "no observation error recorded".to_owned(),
        |error| error.message,
    );

    format!(
        "Timed out waiting for wallet block feed freshness: {details}; last error: {last_error}"
    )
}

async fn apply_latest_wallet_feed_state(
    world: &CucumberWorld,
    feed: &BlockFeed,
    tracking_batches: &[WalletFeedTrackingBatch],
    genesis_utxos: &[Utxo],
) -> Result<(), StepError> {
    match update_wallet_feed_state(world, feed, genesis_utxos)? {
        Ok(()) => Ok(()),
        Err(error) if error.requires_direct_backfill() => {
            debug!(target: TARGET, "wallet feed state needs backfill: {error}");

            backfill_wallet_feed_batches(world, tracking_batches, genesis_utxos).await
        }
        Err(error) => Err(wallet_feed_error(error)),
    }
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

fn wallet_feed_tracking_batches(
    world: &CucumberWorld,
    wallet_keys: &TrackedWalletKeysBySource,
    requirements: &[WalletFeedSourceRequirement],
) -> Vec<WalletFeedTrackingBatch> {
    requirements
        .iter()
        .filter_map(|requirement| {
            let source_id = wallet_feed_source_id(world, &requirement.node_name);
            let wallet_keys_for_source = wallet_keys
                .batches()
                .find(|batch| batch.source_id().as_str() == source_id)?;

            Some(WalletFeedTrackingBatch::new(
                requirement.node_name.clone(),
                wallet_keys_for_source.wallet_keys().iter().cloned(),
            ))
        })
        .collect()
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

async fn wallet_feed_source_requirements(
    world: &CucumberWorld,
    wallets: &[WalletInfo],
) -> Result<Vec<WalletFeedSourceRequirement>, StepError> {
    let source_nodes = wallets
        .iter()
        .map(|wallet| wallet.node_name.clone())
        .collect::<BTreeSet<_>>();
    let mut requirements = Vec::with_capacity(source_nodes.len());

    for node_name in source_nodes {
        let node = world
            .nodes_info
            .get(&node_name)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Wallet block-feed source node `{node_name}` not found"),
            })?;
        let consensus = node.started_node.client.consensus_info().await?;

        requirements.push(WalletFeedSourceRequirement {
            node_name,
            min_height: consensus.cryptarchia_info.height,
        });
    }

    Ok(requirements)
}

async fn all_wallet_feed_source_requirements(
    world: &CucumberWorld,
) -> Result<Vec<WalletFeedSourceRequirement>, StepError> {
    let mut requirements = Vec::with_capacity(world.nodes_info.len());

    for node_name in world.nodes_info.keys() {
        let node = world
            .nodes_info
            .get(node_name)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Wallet block-feed source node `{node_name}` not found"),
            })?;
        let consensus = node.started_node.client.consensus_info().await?;

        requirements.push(WalletFeedSourceRequirement {
            node_name: node_name.clone(),
            min_height: consensus.cryptarchia_info.height,
        });
    }

    Ok(requirements)
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

fn update_wallet_feed_state_status(
    world: &CucumberWorld,
    feed: &BlockFeed,
    genesis_utxos: &[Utxo],
) -> Result<Option<String>, StepError> {
    match update_wallet_feed_state(world, feed, genesis_utxos)? {
        Ok(()) => Ok(None),
        Err(error) if error.requires_direct_backfill() => {
            Ok(Some(format!("wallet feed state needs backfill: {error}")))
        }
        Err(error) => Err(wallet_feed_error(error)),
    }
}

fn update_wallet_feed_state(
    world: &CucumberWorld,
    feed: &BlockFeed,
    genesis_utxos: &[Utxo],
) -> Result<Result<(), WalletBlockFeedTrackerError>, StepError> {
    world
        .with_wallet_feed_state_mut(|tracker, wallets| {
            tracker.apply_feed(wallets, feed, genesis_utxos)
        })
        .map(|result| result.map(|_| ()))
}

fn wallet_feed_error(error: WalletBlockFeedTrackerError) -> StepError {
    match error {
        WalletBlockFeedTrackerError::TrackedKeys(error) => StepError::LogicalError {
            message: error.to_string(),
        },
        other => StepError::LogicalError {
            message: other.to_string(),
        },
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
