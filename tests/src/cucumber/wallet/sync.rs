use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};

use lb_core::mantle::Utxo;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_testing_framework::{BlockFeed, BlockFeedObservation, is_truthy_env};
use tracing::{info, warn};

use crate::{
    common::wallet::{
        NodeHttpWalletChainSource, TrackedWalletKeysBySource, TrackedWalletKeysForSource,
        WalletBalance, WalletBlockFeedTracker, WalletBlockFeedTrackerError, WalletFeedStateResult,
        WalletFeedStateResults, WalletFeedTrackingBatch, WalletFeedTrackingResult, WalletId,
        WalletObservedBlock, WalletObservedOutput, WalletObservedSpend, WalletOutputState,
        WalletUtxos, wallet_utxos_from_chain,
    },
    cucumber::{
        defaults::CUCUMBER_VERBOSE_CONSOLE,
        error::StepError,
        fee_reserve::{SCENARIO_FEE_ACCOUNT_NAME, ScenarioFeeState},
        wallet::{
            TARGET, WalletStateView,
            best_node::{BestNodeInfo, sanitize_best_node_info_for_group_with_feed},
        },
        world::{CucumberWorld, WalletInfo},
    },
};

pub async fn sync_available_utxos_for_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    observe_available_utxos_for_user_wallets(world, step, best_node_info).await
}

pub async fn observe_available_utxos_for_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    let wallets = world.all_user_wallets();
    let mut wallet_keys = build_tracked_wallet_keys(world, step, &wallets)?;
    add_scenario_fee_wallet_keys(world, &mut wallet_keys);
    observe_available_utxos_from_feed(world, step, &wallet_keys, best_node_info).await
}

pub async fn sync_available_utxos_for_funding_wallets(
    world: &mut CucumberWorld,
    step: &str,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    observe_available_utxos_for_funding_wallets(world, step, best_node_info).await
}

pub async fn observe_available_utxos_for_funding_wallets(
    world: &mut CucumberWorld,
    step: &str,
    _best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    let mut funding_wallet_utxos = WalletUtxos::new();
    for wallet in world.all_funding_wallets() {
        let utxos = observe_wallet(world, step, &wallet.wallet_name)
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
    observe_available_utxos_for_all_wallets(world, step, best_node_info).await
}

pub async fn observe_available_utxos_for_all_wallets(
    world: &mut CucumberWorld,
    step: &str,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    let mut all_wallet_utxos =
        observe_available_utxos_for_wallets(world, step, world.all_user_wallets(), best_node_info)
            .await?;
    for wallet in world.all_funding_wallets() {
        let utxos = observe_wallet(world, step, &wallet.wallet_name)
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
    observe_available_utxos_for_wallets(world, step, wallets, best_node_info).await
}

pub async fn observe_available_utxos_for_wallets(
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

    let wallet_keys = build_tracked_wallet_keys(world, step, &wallets)?;
    observe_available_utxos_from_feed(world, step, &wallet_keys, best_node_info).await
}

pub async fn sync_available_utxos_for_wallet(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> Result<Vec<Utxo>, StepError> {
    observe_available_utxos_for_wallet(world, step, wallet_name).await
}

pub async fn observe_available_utxos_for_wallet(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> Result<Vec<Utxo>, StepError> {
    Ok(observe_wallet(world, step, wallet_name)
        .await?
        .into_available_utxos())
}

pub async fn sync_wallet_output_balance(
    world: &mut CucumberWorld,
    step: &str,
    wallet: &WalletInfo,
    wallet_name: &str,
    wallet_state_type: WalletOutputState,
) -> Result<WalletBalance, StepError> {
    observe_wallet_output_balance(world, step, wallet, wallet_name, wallet_state_type).await
}

pub async fn observe_wallet_output_balance(
    world: &mut CucumberWorld,
    _step: &str,
    wallet: &WalletInfo,
    wallet_name: &str,
    wallet_state_type: WalletOutputState,
) -> Result<WalletBalance, StepError> {
    observe_wallet_state(world, wallet_name, &wallet.node_name, wallet.public_key()?)
        .await
        .map(|observation| observation.balance(wallet_state_type))
}

pub async fn sync_wallet_balance(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    wallet_state_type: WalletOutputState,
) -> Result<WalletBalance, StepError> {
    observe_wallet_balance(world, step, wallet_name, wallet_state_type).await
}

pub async fn observe_wallet_balance(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
    wallet_state_type: WalletOutputState,
) -> Result<WalletBalance, StepError> {
    Ok(observe_wallet(world, step, wallet_name)
        .await?
        .balance(wallet_state_type))
}

pub async fn sync_wallet_state_from_feed(
    world: &mut CucumberWorld,
    wallet_name: &str,
    wallet_node_name: &str,
    wallet_pk: ZkPublicKey,
) -> Result<WalletStateView, StepError> {
    observe_wallet_state(world, wallet_name, wallet_node_name, wallet_pk).await
}

pub async fn observe_wallet_state(
    world: &mut CucumberWorld,
    wallet_name: &str,
    wallet_node_name: &str,
    wallet_pk: ZkPublicKey,
) -> Result<WalletStateView, StepError> {
    world.ensure_wallet_block_feed().await?;
    let feed = world.wallet_block_feed()?;
    let mut wallet_keys = TrackedWalletKeysBySource::new();
    wallet_keys.add_wallet("", wallet_name, wallet_pk);

    let plan =
        build_wallet_feed_source_plan(world, wallet_name, wallet_node_name, &wallet_keys).await?;
    let on_chain_utxos = observe_wallet_feed_plan(world, &feed, plan).await?;

    let mut observations = observe_wallet_states(world, &wallet_keys, &on_chain_utxos)?;

    observations
        .remove(wallet_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Wallet state observation for `{wallet_name}` was not returned"),
        })
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

async fn build_wallet_feed_source_plan(
    world: &CucumberWorld,
    wallet_name: &str,
    wallet_node_name: &str,
    wallet_keys: &TrackedWalletKeysBySource,
) -> Result<WalletFeedTrackingPlan, StepError> {
    let source_wallet_keys =
        wallet_keys
            .batches()
            .next()
            .ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "Cannot observe wallet `{wallet_name}` without tracked wallet keys"
                ),
            })?;
    let min_height = wallet_feed_source_height(world, wallet_name, wallet_node_name).await?;

    let mut plan = WalletFeedTrackingPlan::default();
    plan.add_wallet_keys(wallet_node_name, min_height, &source_wallet_keys);

    Ok(plan)
}

async fn build_wallet_feed_best_node_plan(
    world: &CucumberWorld,
    step: &str,
    wallet_keys: &TrackedWalletKeysBySource,
    feed: &BlockFeed,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletFeedTrackingPlan, StepError> {
    let mut plan = WalletFeedTrackingPlan::default();

    for source_wallet_keys in wallet_keys.batches() {
        let source =
            wallet_feed_source_from_best_node(world, feed, &source_wallet_keys, best_node_info)
                .await
                .inspect_err(|e| {
                    warn!(target: TARGET, "Step `{}` error: {e}", step);
                })?;

        plan.add_wallet_keys(source.node_name, source.min_height, &source_wallet_keys);
    }

    Ok(plan)
}

async fn observe_available_utxos_from_feed(
    world: &mut CucumberWorld,
    step: &str,
    wallet_keys: &TrackedWalletKeysBySource,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletUtxos, StepError> {
    world.ensure_wallet_block_feed().await?;
    let feed = world.wallet_block_feed()?;
    let plan =
        build_wallet_feed_best_node_plan(world, step, wallet_keys, &feed, best_node_info).await?;
    let on_chain_utxos = observe_wallet_feed_plan(world, &feed, plan)
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step);
        })?;

    let observations = observe_wallet_states(world, wallet_keys, &on_chain_utxos)?;

    Ok(observations
        .into_iter()
        .map(|(wallet_name, observation)| (wallet_name, observation.into_available_utxos()))
        .collect())
}

async fn observe_wallet_feed_plan(
    world: &CucumberWorld,
    feed: &BlockFeed,
    plan: WalletFeedTrackingPlan,
) -> Result<WalletUtxos, StepError> {
    let genesis_utxos = world.genesis_block_utxos.clone();
    let (tracking_batches, feed_requirements) = plan.into_parts();

    track_wallet_feed_batches_with_backfill(world, &tracking_batches, &genesis_utxos).await?;
    wait_for_wallet_feed_sources(world, feed, feed_requirements, &genesis_utxos).await?;

    let state_results =
        observe_wallet_feed_batches(world, feed, tracking_batches, &genesis_utxos).await?;

    for observed_block in state_results.observed_blocks() {
        log_wallet_observed_block(observed_block);
    }

    Ok(state_results.into_wallet_utxos())
}

fn observe_wallet_states(
    world: &CucumberWorld,
    wallet_keys: &TrackedWalletKeysBySource,
    on_chain_utxos: &WalletUtxos,
) -> Result<BTreeMap<WalletId, WalletStateView>, StepError> {
    let mut observations = world
        .with_wallets(|wallets| wallets.observe_wallets(wallet_keys.wallet_keys().cloned()))?;

    apply_scenario_fee_observations(world, &mut observations, on_chain_utxos);

    Ok(observations)
}

#[derive(Default)]
struct WalletFeedTrackingPlan {
    sources: BTreeMap<String, WalletFeedSourceTrackingPlan>,
}

impl WalletFeedTrackingPlan {
    fn add_wallet_keys(
        &mut self,
        source_node_name: impl Into<String>,
        min_height: u64,
        source_wallet_keys: &TrackedWalletKeysForSource,
    ) {
        let source_node_name = source_node_name.into();
        self.sources
            .entry(source_node_name.clone())
            .and_modify(|source| source.add_wallet_keys(min_height, source_wallet_keys))
            .or_insert_with(|| {
                WalletFeedSourceTrackingPlan::new(source_node_name, min_height, source_wallet_keys)
            });
    }

    fn into_parts(self) -> (Vec<WalletFeedTrackingBatch>, BTreeMap<String, u64>) {
        let mut tracking_batches = Vec::new();
        let mut feed_requirements = BTreeMap::new();

        for (source_node_name, source) in self.sources {
            if source.tracking_batch.is_empty() {
                continue;
            }

            feed_requirements.insert(source_node_name, source.min_height);
            tracking_batches.push(source.tracking_batch);
        }

        (tracking_batches, feed_requirements)
    }
}

struct WalletFeedSourceTrackingPlan {
    min_height: u64,
    tracking_batch: WalletFeedTrackingBatch,
}

impl WalletFeedSourceTrackingPlan {
    fn new(
        source_node_name: String,
        min_height: u64,
        source_wallet_keys: &TrackedWalletKeysForSource,
    ) -> Self {
        Self {
            min_height,
            tracking_batch: WalletFeedTrackingBatch::new(
                source_node_name,
                source_wallet_keys.wallet_keys().iter().cloned(),
            ),
        }
    }

    fn add_wallet_keys(
        &mut self,
        min_height: u64,
        source_wallet_keys: &TrackedWalletKeysForSource,
    ) {
        self.min_height = self.min_height.min(min_height);
        self.tracking_batch
            .extend_wallet_keys(source_wallet_keys.wallet_keys().iter().cloned());
    }
}

async fn observe_wallet_feed_batches(
    world: &CucumberWorld,
    feed: &BlockFeed,
    tracking_batches: Vec<WalletFeedTrackingBatch>,
    genesis_utxos: &[Utxo],
) -> Result<WalletFeedStateResults, StepError> {
    let feed_result = world.with_wallet_feed_state_mut(|tracker, wallets| {
        let observed_blocks = tracker.apply_feed(wallets, feed, genesis_utxos)?;

        observed_wallet_results(tracker, &tracking_batches, observed_blocks)
    })?;

    match feed_result {
        Ok(results) => Ok(results),
        Err(error) if should_backfill_wallet_state(&error) => {
            backfill_wallet_feed_batches(world, &tracking_batches, genesis_utxos).await
        }
        Err(error) => Err(wallet_feed_error(error)),
    }
}

fn observed_wallet_results(
    tracker: &WalletBlockFeedTracker,
    tracking_batches: &[WalletFeedTrackingBatch],
    observed_blocks: Vec<WalletObservedBlock>,
) -> Result<WalletFeedStateResults, WalletBlockFeedTrackerError> {
    let mut observed_blocks_by_source = observed_blocks_by_source(observed_blocks);
    let mut results = Vec::new();

    for batch in tracking_batches {
        if batch.is_empty() {
            continue;
        }

        results.push(WalletFeedStateResult::from_parts(
            tracker.observed_wallet_utxos(batch.source_node_name())?,
            observed_blocks_by_source
                .remove(batch.source_node_name())
                .unwrap_or_default(),
        ));
    }

    Ok(WalletFeedStateResults::new(results))
}

fn observed_blocks_by_source(
    observed_blocks: Vec<WalletObservedBlock>,
) -> HashMap<String, Vec<WalletObservedBlock>> {
    let mut by_source = HashMap::new();

    for observed_block in observed_blocks {
        by_source
            .entry(observed_block.source_node_name().to_owned())
            .or_insert_with(Vec::new)
            .push(observed_block);
    }

    by_source
}

async fn backfill_wallet_feed_batches(
    world: &CucumberWorld,
    tracking_batches: &[WalletFeedTrackingBatch],
    genesis_utxos: &[Utxo],
) -> Result<WalletFeedStateResults, StepError> {
    let mut results = Vec::new();

    for tracking_batch in tracking_batches {
        let result = backfill_wallet_feed_batch(world, tracking_batch, genesis_utxos).await?;
        results.push(result);
    }

    Ok(WalletFeedStateResults::new(results))
}

async fn backfill_wallet_feed_batch(
    world: &CucumberWorld,
    tracking_batch: &WalletFeedTrackingBatch,
    genesis_utxos: &[Utxo],
) -> Result<WalletFeedStateResult, StepError> {
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
    );
    let wallet_utxos =
        wallet_utxos_from_chain(&mut source, tracking_batch.wallet_keys(), genesis_utxos)
            .await
            .map_err(|error| StepError::LogicalError {
                message: format!(
                    "Wallet chain backfill failed for source `{source_node_name}`: {error}"
                ),
            })?;
    let observed_utxos = wallet_utxos.clone();
    let tracked_utxos = wallet_utxos.clone();
    let tip_string = tip.to_string();

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
        .map_err(wallet_feed_error)?;

    Ok(WalletFeedStateResult::from_parts(wallet_utxos, Vec::new()))
}

const fn should_backfill_wallet_state(error: &WalletBlockFeedTrackerError) -> bool {
    error.requires_direct_backfill()
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

async fn observe_wallet(
    world: &mut CucumberWorld,
    step: &str,
    wallet_name: &str,
) -> Result<WalletStateView, StepError> {
    let wallet = world.resolve_wallet(wallet_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step);
    })?;

    let state_observation =
        observe_wallet_state(world, wallet_name, &wallet.node_name, wallet.public_key()?)
            .await
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{}` error: {e}", step);
            })?;
    log_wallet_balance(wallet_name, &state_observation);

    Ok(state_observation)
}

struct WalletFeedSource {
    node_name: String,
    min_height: u64,
}

async fn wallet_feed_source_from_best_node(
    world: &CucumberWorld,
    feed: &BlockFeed,
    source_wallet_keys: &TrackedWalletKeysForSource,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<WalletFeedSource, StepError> {
    let (node_name, _, consensus) = sanitize_best_node_info_for_group_with_feed(
        world,
        source_wallet_keys.source_id(),
        best_node_info,
        Some(feed),
    )
    .await?;

    Ok(WalletFeedSource {
        node_name,
        min_height: consensus.height,
    })
}

async fn wallet_feed_source_height(
    world: &CucumberWorld,
    wallet_name: &str,
    node_name: &str,
) -> Result<u64, StepError> {
    let node = world
        .nodes_info
        .get(node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{node_name}' for wallet '{wallet_name}' not found"),
        })?;

    Ok(node
        .started_node
        .client
        .consensus_info()
        .await?
        .cryptarchia_info
        .height)
}

async fn wait_for_wallet_feed_sources(
    world: &CucumberWorld,
    feed: &BlockFeed,
    feed_requirements: impl IntoIterator<Item = (String, u64)>,
    genesis_utxos: &[Utxo],
) -> Result<(), StepError> {
    const WALLET_BLOCK_FEED_WAIT_TIMEOUT: Duration = Duration::from_secs(15);

    let feed_requirements = feed_requirements.into_iter().collect::<BTreeMap<_, _>>();
    if feed_requirements.is_empty() {
        return Ok(());
    }

    let started_at = Instant::now();
    let requirements = WalletFeedWaitRequirements::new(feed_requirements);
    let mut wait = WalletFeedWait::new(feed, requirements);

    loop {
        refresh_wallet_feed_wait(world, feed, genesis_utxos, &mut wait)?;
        if wait.is_complete() {
            return Ok(());
        }

        if started_at.elapsed() >= WALLET_BLOCK_FEED_WAIT_TIMEOUT {
            return Err(wait.timeout_error(feed));
        }

        wait_for_next_wallet_feed_cycle(feed, &mut wait).await;
    }
}

fn refresh_wallet_feed_wait(
    world: &CucumberWorld,
    feed: &BlockFeed,
    genesis_utxos: &[Utxo],
    wait: &mut WalletFeedWait,
) -> Result<(), StepError> {
    let Some(observation) = feed.latest_observation() else {
        return Ok(());
    };

    let state_status = update_wallet_feed_state_status(world, feed, genesis_utxos)?;
    wait.refresh(&observation, state_status);

    Ok(())
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

async fn wait_for_next_wallet_feed_cycle(feed: &BlockFeed, wait: &mut WalletFeedWait) {
    if let Ok(observation) = feed
        .wait_for_next_cycle(wait.after_cycle(), Duration::from_secs(1))
        .await
    {
        wait.set_after_cycle(observation.cycle());
    }
}

struct WalletFeedWaitRequirements {
    min_heights: BTreeMap<String, u64>,
}

impl WalletFeedWaitRequirements {
    const fn new(min_heights: BTreeMap<String, u64>) -> Self {
        Self { min_heights }
    }

    fn source_names(&self) -> impl Iterator<Item = String> + '_ {
        self.min_heights.keys().cloned()
    }

    fn pending_sources(&self, observation: &BlockFeedObservation) -> Vec<String> {
        self.min_heights
            .iter()
            .filter_map(|(node_name, min_height)| {
                let Some(head) = observation.node_head(node_name) else {
                    return Some(format!("{node_name} missing"));
                };

                match head.tip_height {
                    Some(height) if height >= *min_height => None,
                    Some(height) => Some(format!("{node_name} at {height}/{min_height}")),
                    None => Some(format!("{node_name} height unknown/{min_height}")),
                }
            })
            .collect()
    }
}

struct WalletFeedWait {
    requirements: WalletFeedWaitRequirements,
    after_cycle: u64,
    pending_sources: Vec<String>,
    last_summary: String,
}

impl WalletFeedWait {
    fn new(feed: &BlockFeed, requirements: WalletFeedWaitRequirements) -> Self {
        let after_cycle = feed
            .latest_observation()
            .map_or(0, |observation| observation.cycle());
        let pending_sources = requirements.source_names().collect();

        Self {
            requirements,
            after_cycle,
            pending_sources,
            last_summary: "no block-feed observation yet".to_owned(),
        }
    }

    const fn after_cycle(&self) -> u64 {
        self.after_cycle
    }

    const fn set_after_cycle(&mut self, after_cycle: u64) {
        self.after_cycle = after_cycle;
    }

    fn refresh(&mut self, observation: &BlockFeedObservation, state_status: Option<String>) {
        self.after_cycle = observation.cycle();
        self.pending_sources = self.requirements.pending_sources(observation);
        self.last_summary = observation.summary();

        if let Some(state_status) = state_status {
            self.last_summary = format!("{}; {}", self.last_summary, state_status);
        }
    }

    const fn is_complete(&self) -> bool {
        self.pending_sources.is_empty()
    }

    fn timeout_error(&self, feed: &BlockFeed) -> StepError {
        let last_error = feed.last_error().map_or_else(
            || "no observation error recorded".to_owned(),
            |error| error.message,
        );

        StepError::Timeout {
            message: format!(
                "Timed out waiting for wallet block feed sources [{}]; last observation: {}; \
                last error: {}",
                self.pending_sources.join(", "),
                self.last_summary,
                last_error
            ),
        }
    }
}

fn track_wallet_feed_batches(
    world: &CucumberWorld,
    tracking_batches: &[WalletFeedTrackingBatch],
    genesis_utxos: &[Utxo],
) -> Result<WalletFeedTrackingResult, StepError> {
    world
        .with_wallet_feed_state_mut(|tracker, wallets| {
            tracker.track_wallets(wallets, tracking_batches, genesis_utxos)
        })?
        .map_err(wallet_feed_error)
}

async fn track_wallet_feed_batches_with_backfill(
    world: &CucumberWorld,
    tracking_batches: &[WalletFeedTrackingBatch],
    genesis_utxos: &[Utxo],
) -> Result<(), StepError> {
    let tracking = track_wallet_feed_batches(world, tracking_batches, genesis_utxos)?;
    if !tracking.needs_backfill() {
        return Ok(());
    }

    backfill_wallet_feed_batches(world, tracking.backfill_batches(), genesis_utxos).await?;

    Ok(())
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

fn log_wallet_observed_block(block: &WalletObservedBlock) {
    if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
        info!(
            target: TARGET,
            "Evaluating block {height} for {} wallets on `{}`: {header_id}, \
            transactions len: {}",
            block.wallet_count(),
            block.source_node_name(),
            block.transaction_count(),
            height = block.height(),
            header_id = block.header_id(),
        );
    }

    log_wallet_observed_outputs(block.observed_outputs());
    log_wallet_observed_spends(block.observed_spends());
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

fn log_wallet_observed_outputs(observed_outputs: &[WalletObservedOutput]) {
    if !is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
        return;
    }

    for found in observed_outputs {
        info!(
            target: TARGET,
            "Found UTXO for `{}`: value: {}, id: {:?}",
            found.wallet_id,
            found.utxo.note.value,
            found.utxo.id(),
        );
    }
}

fn log_wallet_observed_spends(observed_spends: &[WalletObservedSpend]) {
    for spent in observed_spends {
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
