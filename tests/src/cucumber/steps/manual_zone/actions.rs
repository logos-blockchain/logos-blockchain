use std::{collections::HashMap, sync::Arc, time::Duration};

use cucumber::gherkin::Step;
use futures::future::join_all;
use lb_common_http_client::CommonHttpClient;
use lb_core::mantle::{
    TxHash, Utxo,
    ops::channel::{config::Keys, deposit::Metadata, inscribe::Inscription},
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_testing_framework::{LbcManualCluster, NodeHttpClient};
use lb_zone_sdk::{
    adapter::NodeHttpClient as ZoneNodeHttpClient,
    indexer::ZoneIndexer,
    sequencer::{Event, PublishResult, SequencerCheckpoint, SequencerHandle, ZoneSequencer},
};
use tokio::{
    sync::mpsc::Receiver,
    task::JoinHandle,
    time::{error::Elapsed, timeout},
};
use tracing::info;

use super::{
    errors::{log_step_error, zone_step_error},
    steps::DEFAULT_ZONE_SEQUENCER,
    support::{
        AtomicZoneDepositRequest, DiscardedPayloads, PublishDeadline, StartedZoneNode,
        ZoneAccountBalances, ZoneDeposit, build_zone_deposit, ensure_zone_transactions_included,
        keygen, prepare_zone_cluster, publish_atomic_zone_withdraw, publish_message_with_retry,
        sequencer_config, sequencer_config_with_pending_submit_depth, start_balance_aware_policy,
        start_republish_policy, start_sequencer_event_loop, start_sorted_conflict_policy,
        start_zone_node, submit_atomic_zone_deposit, submit_zone_deposit, submit_zone_withdraw,
        wait_for_zone_network_ready,
    },
    tables::{ConcurrentZoneMessageRow, group_zone_messages_by_sequencer},
};
use crate::{
    common::{
        mantle_inscription::make_inscription, manual_cluster::wait_for_height,
        wallet::WalletReservedInputs,
    },
    cucumber::{
        error::{StepError, StepResult},
        steps::TARGET,
        wallet::sync::sync_wallet_state_from_feed,
        world::{CucumberWorld, NodeInfo},
    },
};

const ZONE_CHANNEL_WITHDRAW_THRESHOLD: u16 = 1;
const ZONE_CHANNEL_DEPOSIT_THRESHOLD: u16 = 1;
const SEQUENCER_READY_TIMEOUT: Duration = Duration::from_mins(2);
const SEQUENCER_READY_POLL_TIMEOUT: Duration = Duration::from_secs(10);
const SEQUENCER_READY_HEIGHT_ADVANCE_TIMEOUT: Duration = Duration::from_secs(30);
const ZONE_FUNDING_WALLET_NAME: &str = "zone-funding";

pub(super) enum DriveMode {
    Passive {
        republish_orphans: bool,
    },
    Republish,
    Sorted {
        discarded: DiscardedPayloads,
    },
    BalanceAware {
        initial_balances: ZoneAccountBalances,
        planned_payloads: Vec<Inscription>,
    },
}

impl DriveMode {
    pub(super) const fn passive() -> Self {
        Self::Passive {
            republish_orphans: false,
        }
    }

    pub(super) const fn passive_republish_orphans() -> Self {
        Self::Passive {
            republish_orphans: true,
        }
    }
}

struct PublishedZoneMessage {
    alias: String,
    payload: Inscription,
    result: PublishResult,
}

struct StartedSequencerRuntime {
    task: JoinHandle<()>,
    events: Option<Receiver<Event>>,
    checkpoint_rx: Option<tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>>,
    discarded_payloads: Option<DiscardedPayloads>,
}

pub(super) fn register_zone_sequencers(world: &mut CucumberWorld, aliases: Vec<String>) {
    for alias in aliases {
        world.zone.register_sequencer(alias, keygen());
    }
}

pub(super) fn register_zone_sequencers_with_shared_key(
    world: &mut CucumberWorld,
    source_alias: &str,
    aliases: Vec<String>,
) -> StepResult {
    let signing_key = world.zone.sequencer_signing_key(source_alias)?.clone();

    for alias in aliases {
        world.zone.register_sequencer(alias, signing_key.clone());
    }

    Ok(())
}

pub(super) async fn start_zone_cluster(world: &mut CucumberWorld, step: &Step) -> StepResult {
    let zone_cluster = prepare_zone_cluster(world.scenario_base_dir.clone())
        .map_err(|error| zone_step_error(step, &error))?;

    let funding_public_key = zone_cluster.funding_public_key;
    let genesis_block_utxos = zone_cluster.genesis_block_utxos;
    let cluster = zone_cluster.cluster;

    let started_zone_node = start_zone_node(&cluster, &world.scenario_base_dir)
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    wait_for_zone_network_ready(&cluster)
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    let client = started_zone_node.started_node.client.clone();

    remember_zone_cluster(
        world,
        cluster,
        started_zone_node,
        funding_public_key,
        genesis_block_utxos,
    );

    info!(target: TARGET, node_url = %client.base_url(), "Started zone cluster");

    Ok(())
}

fn remember_zone_cluster(
    world: &mut CucumberWorld,
    cluster: LbcManualCluster,
    started_zone_node: StartedZoneNode,
    funding_public_key: ZkPublicKey,
    genesis_block_utxos: Vec<Utxo>,
) {
    let node_name = "NODE_1".to_owned();

    world.genesis_block_utxos = genesis_block_utxos;
    world.local_cluster = Some(cluster);
    world.nodes_info.insert(
        node_name.clone(),
        NodeInfo {
            name: node_name.clone(),
            started_node: started_zone_node.started_node,
            run_config: None,
            chain_info: HashMap::default(),
            wallet_info: HashMap::default(),
            runtime_dir: started_zone_node.runtime_dir,
            immediate_start: false,
        },
    );
    world.zone.initialize_cluster(node_name, funding_public_key);
}

pub(super) async fn submit_zone_channel_config(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    transaction_alias: String,
    authorized_aliases: Vec<String>,
    posting_timeframe: u32,
    posting_timeout: u32,
) -> StepResult {
    let handle = log_step_error(step, world.zone.sequencer_handle(sequencer_alias))?;
    let mut ordered_aliases = vec![sequencer_alias.to_owned()];

    for alias in authorized_aliases {
        if ordered_aliases.iter().any(|existing| existing == &alias) {
            continue;
        }

        ordered_aliases.push(alias);
    }

    let authorized_keys = ordered_aliases
        .into_iter()
        .map(|alias| {
            world
                .zone
                .sequencer_signing_key(&alias)
                .map(Ed25519Key::public_key)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut checkpoint_rx = world
        .zone
        .checkpoint_receiver(sequencer_alias)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Zone sequencer '{sequencer_alias}' has no checkpoint watch"),
        })?;
    checkpoint_rx.mark_unchanged();

    let (result, finalized) = handle
        .channel_config(
            Keys::new_unchecked(authorized_keys),
            posting_timeframe.into(),
            posting_timeout.into(),
            ZONE_CHANNEL_WITHDRAW_THRESHOLD,
            ZONE_CHANNEL_DEPOSIT_THRESHOLD,
        )
        .await
        .map_err(|error| StepError::LogicalError {
            message: format!("Zone channel_config failed: {error}"),
        })?;

    drop(finalized);

    // Wait until the SDK has published a checkpoint that contains our newly
    // submitted tx — the watch is updated by the drive loop on its next
    // iteration after the actor's reply, so a naive read here races.
    let checkpoint = timeout(
        Duration::from_secs(30),
        wait_for_checkpoint_with_tx(&mut checkpoint_rx, result.inscription_id),
    )
    .await
    .map_err(|_| StepError::LogicalError {
        message: format!(
            "timed out waiting for sequencer '{sequencer_alias}' checkpoint to include {:?}",
            result.inscription_id
        ),
    })?
    .map_err(|message| StepError::LogicalError { message })?;

    world
        .zone
        .remember_checkpoint(format!("{transaction_alias}_CHECKPOINT"), checkpoint);
    world.remember_submitted_transaction(transaction_alias, result.inscription_id);

    Ok(())
}

pub(super) fn stop_zone_sequencer(
    world: &mut CucumberWorld,
    sequencer_alias: impl AsRef<str>,
) -> StepResult {
    world.zone.stop_sequencer(sequencer_alias.as_ref())?;

    Ok(())
}

pub(super) fn save_zone_checkpoint(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
    checkpoint_alias: String,
) -> StepResult {
    let sequencer_alias = sequencer_alias.as_ref();
    let checkpoint = log_step_error(step, world.zone.current_checkpoint_for(sequencer_alias))?;

    world.zone.remember_checkpoint(checkpoint_alias, checkpoint);

    Ok(())
}

pub(super) fn remember_published_zone_message(
    world: &mut CucumberWorld,
    sequencer_alias: &str,
    message_alias: String,
    payload: Inscription,
    result: &PublishResult,
) {
    let checkpoint = world.zone.current_checkpoint_for(sequencer_alias).ok();
    world.zone.remember_zone_message(
        message_alias,
        payload,
        Some(result.inscription_id),
        Some(sequencer_alias),
        checkpoint,
    );
}

async fn wait_for_checkpoint_with_tx(
    rx: &mut tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>,
    tx_hash: TxHash,
) -> Result<SequencerCheckpoint, String> {
    loop {
        let snapshot = rx.borrow_and_update().clone();
        if let Some(cp) = snapshot
            && cp.pending_txs.iter().any(|(hash, _)| *hash == tx_hash)
        {
            return Ok(cp);
        }
        rx.changed()
            .await
            .map_err(|err| format!("checkpoint watch closed: {err}"))?;
    }
}

async fn sync_zone_funding_wallet_utxos(
    world: &mut CucumberWorld,
    step: &Step,
) -> Result<Vec<Utxo>, StepError> {
    let node_name = world.zone.node_name()?.to_owned();
    let funding_public_key = world.zone.funding_public_key()?;

    Ok(sync_wallet_state_from_feed(
        world,
        ZONE_FUNDING_WALLET_NAME,
        &node_name,
        funding_public_key,
    )
    .await
    .inspect_err(|e| {
        tracing::warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?
    .into_available_utxos())
}

fn record_zone_wallet_submission(
    world: &CucumberWorld,
    tx_hash: TxHash,
    reserved_inputs: Vec<Utxo>,
) -> StepResult {
    world.with_wallets_mut(|wallets| {
        wallets.record_wallet_reservation(
            ZONE_FUNDING_WALLET_NAME.to_owned(),
            tx_hash,
            WalletReservedInputs::new(reserved_inputs, Vec::new()),
            0,
        );
    })
}

pub(super) async fn submit_zone_deposit_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    channel_alias: String,
    amount: u64,
    metadata: Metadata,
) -> StepResult {
    let node_url = log_step_error(step, world.zone_node_url())?;
    let funding_public_key = log_step_error(step, world.zone.funding_public_key())?;
    let available_utxos = sync_zone_funding_wallet_utxos(world, step).await?;
    let ZoneDeposit {
        deposit,
        reserved_inputs,
    } = build_zone_deposit(
        available_utxos,
        world.zone.sequencer_channel_id(&channel_alias)?,
        amount,
        metadata,
    )
    .map_err(|error| zone_step_error(step, &error))?;

    let response = submit_zone_deposit(&node_url, &deposit, funding_public_key)
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    world
        .zone
        .remember_submitted_deposit(transaction_alias.clone(), deposit, amount);
    record_zone_wallet_submission(world, response, reserved_inputs)?;
    world.remember_submitted_transaction(transaction_alias, response);

    Ok(())
}

pub(super) async fn submit_atomic_zone_deposit_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    transaction_alias: String,
    message_alias: String,
    amount: u64,
    metadata: Metadata,
) -> StepResult {
    let node_url = log_step_error(step, world.zone_node_url())?;
    let funding_public_key = log_step_error(step, world.zone.funding_public_key())?;
    let available_utxos = sync_zone_funding_wallet_utxos(world, step).await?;
    let sequencer = log_step_error(step, world.zone.sequencer_handle(sequencer_alias))?;
    let inscription_data = make_inscription(&format!("Mint {amount} to Alice"));

    let submission = submit_atomic_zone_deposit(
        &node_url,
        sequencer,
        AtomicZoneDepositRequest {
            channel_id: world.zone.sequencer_channel_id(sequencer_alias)?,
            funding_public_key,
            available_utxos,
            amount,
            metadata,
            inscription_data: inscription_data.clone(),
        },
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    world
        .zone
        .remember_submitted_deposit(transaction_alias.clone(), submission.deposit, amount);
    remember_published_zone_message(
        world,
        sequencer_alias,
        message_alias,
        inscription_data,
        &submission.publish,
    );
    record_zone_wallet_submission(
        world,
        submission.publish.inscription_id,
        submission.reserved_inputs,
    )?;
    world.remember_submitted_transaction(transaction_alias, submission.publish.inscription_id);

    Ok(())
}

pub(super) async fn submit_zone_withdraw_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    transaction_alias: String,
    message_alias: String,
    amount: u64,
) -> StepResult {
    let funding_public_key = log_step_error(step, world.zone.funding_public_key())?;
    let sequencer = log_step_error(step, world.zone.sequencer_handle(sequencer_alias))?;
    let inscription_data = make_inscription(&format!("Burn {amount}"));

    let submission = submit_zone_withdraw(
        sequencer,
        world.zone.sequencer_channel_id(sequencer_alias)?,
        funding_public_key,
        amount,
        inscription_data.clone(),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    world
        .zone
        .remember_submitted_withdraw(transaction_alias.clone(), submission.withdraw);
    remember_published_zone_message(
        world,
        sequencer_alias,
        message_alias,
        inscription_data,
        &submission.publish,
    );
    world.remember_submitted_transaction(transaction_alias, submission.publish.inscription_id);

    Ok(())
}

/// Action wrapper for the new `publish_atomic_withdraw` SDK API. Mirrors
/// [`submit_zone_withdraw_transaction`] but uses the high-level fire-and-forget
/// flow: SDK fills the withdraw nonce, locates its own accredited-key index,
/// builds the bundled `MantleTx`, signs locally, and submits.
///
/// `withdraw_rows` carries one `(alias, outputs)` per `WithdrawArg`; each
/// withdraw is remembered under its own alias so multi-withdraw bundles can
/// be asserted per-withdraw via the indexer step. `bundle_alias` is remembered
/// as the bundle's tx hash for `zone transaction "..." is finalized`.
pub(super) async fn publish_atomic_zone_withdraw_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    bundle_alias: String,
    message_alias: String,
    withdraw_rows: Vec<(String, Vec<u64>)>,
) -> StepResult {
    let funding_public_key = log_step_error(step, world.zone.funding_public_key())?;
    let total: u64 = withdraw_rows
        .iter()
        .flat_map(|(_, outputs)| outputs.iter())
        .sum();
    let inscription_data = make_inscription(&format!("Burn {total}"));
    let outputs_per_arg: Vec<Vec<u64>> = withdraw_rows
        .iter()
        .map(|(_, outputs)| outputs.clone())
        .collect();

    let submission = {
        let sequencer = log_step_error(step, world.zone.sequencer_handle(sequencer_alias))?.clone();
        let sequencer_events =
            log_step_error(step, world.zone.sequencer_events_mut(sequencer_alias))?;

        publish_atomic_zone_withdraw(
            &sequencer,
            sequencer_events,
            funding_public_key,
            outputs_per_arg,
            inscription_data.clone(),
            PublishDeadline::from_now(Duration::from_mins(3)),
        )
        .await
        .map_err(|error| zone_step_error(step, &error))?
    };

    if submission.withdraws.len() != withdraw_rows.len() {
        return Err(zone_step_error(
            step,
            &super::support::ZoneTestError::SubmitWithdraw {
                message: format!(
                    "atomic withdraw bundle produced {} withdraw ops, expected {}",
                    submission.withdraws.len(),
                    withdraw_rows.len(),
                ),
            },
        ));
    }
    for ((alias, _), withdraw_op) in withdraw_rows.iter().zip(submission.withdraws) {
        world
            .zone
            .remember_submitted_withdraw(alias.clone(), withdraw_op);
    }
    remember_published_zone_message(
        world,
        sequencer_alias,
        message_alias,
        inscription_data,
        &submission.publish,
    );
    world.remember_submitted_transaction(bundle_alias, submission.publish.inscription_id);

    Ok(())
}

pub(super) fn initialize_zone_indexer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
) -> StepResult {
    let sequencer_alias = sequencer_alias.as_ref();
    let node_url = log_step_error(step, world.zone_node_url())?;
    let indexer = ZoneIndexer::new(
        world.zone.sequencer_channel_id(sequencer_alias)?,
        ZoneNodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );

    world.zone.set_indexer(indexer);

    Ok(())
}

pub(super) async fn publish_zone_messages(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
    rows: Vec<(String, Inscription)>,
) -> StepResult {
    let sequencer_alias = sequencer_alias.as_ref().to_owned();
    let node = log_step_error(step, world.zone_node_http_client())?;

    let published = {
        let sequencer =
            log_step_error(step, world.zone.sequencer_handle(&sequencer_alias))?.clone();
        let sequencer_events =
            log_step_error(step, world.zone.sequencer_events_mut(&sequencer_alias))?;

        let publish_deadline = PublishDeadline::from_now(Duration::from_mins(3));
        let mut published = Vec::with_capacity(rows.len());

        for (alias, payload) in &rows {
            let result =
                publish_message_with_retry(&sequencer, sequencer_events, payload, publish_deadline)
                    .await
                    .map_err(|error| zone_step_error(step, &error))?;

            ensure_zone_transactions_included(
                &node,
                &[result.inscription_id],
                Duration::from_mins(3),
            )
            .await
            .map_err(|error| zone_step_error(step, &error))?;

            published.push(PublishedZoneMessage {
                alias: alias.clone(),
                payload: payload.clone(),
                result,
            });
        }

        published
    };

    for message in published {
        remember_published_zone_message(
            world,
            &sequencer_alias,
            message.alias,
            message.payload,
            &message.result,
        );
    }

    Ok(())
}

pub(super) async fn publish_zone_messages_concurrently(
    world: &mut CucumberWorld,
    step: &Step,
    rows: Vec<ConcurrentZoneMessageRow>,
) -> StepResult {
    let grouped = group_zone_messages_by_sequencer(&rows);
    let handles = grouped
        .keys()
        .map(|sequencer_alias| {
            log_step_error(step, world.zone.sequencer_handle(sequencer_alias))
                .map(|handle| (sequencer_alias.clone(), handle.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    join_all(handles.into_iter().map(|(sequencer_alias, handle)| {
        let payloads = grouped[&sequencer_alias]
            .iter()
            .map(|message| message.payload.clone())
            .collect::<Vec<_>>();

        async move {
            for payload in payloads {
                handle.publish_message(payload).await.map_err(|error| {
                    StepError::LogicalError {
                        message: format!(
                            "Zone concurrent publish failed for sequencer '{sequencer_alias}': {error}"
                        ),
                    }
                })?;
            }

            Ok::<(), StepError>(())
        }
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;

    for row in rows {
        world
            .zone
            .remember_zone_message(row.message_alias, row.payload, None, None, None);
    }

    if world.zone.indexer().is_err() {
        initialize_zone_indexer(world, step, DEFAULT_ZONE_SEQUENCER)?;
    }

    Ok(())
}

pub(super) async fn start_named_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
    checkpoint: Option<SequencerCheckpoint>,
    mode: DriveMode,
) -> StepResult {
    start_named_sequencer_with_config(
        world,
        step,
        sequencer_alias,
        checkpoint,
        mode,
        sequencer_config(),
    )
    .await
}

pub(super) async fn start_named_sequencer_with_pending_submit_depth(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
    checkpoint: Option<SequencerCheckpoint>,
    mode: DriveMode,
    max_pending_publish_depth: usize,
) -> StepResult {
    let config = sequencer_config_with_pending_submit_depth(max_pending_publish_depth);

    start_named_sequencer_with_config(world, step, sequencer_alias, checkpoint, mode, config).await
}

async fn start_named_sequencer_with_config(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
    checkpoint: Option<SequencerCheckpoint>,
    mode: DriveMode,
    config: lb_zone_sdk::sequencer::SequencerConfig,
) -> StepResult {
    let sequencer_alias = sequencer_alias.as_ref().to_owned();
    let signing_key =
        log_step_error(step, world.zone.sequencer_signing_key(&sequencer_alias))?.clone();
    let node_client = log_step_error(step, world.zone_node_http_client())?;
    let node_url = log_step_error(step, world.zone_node_url())?;
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        world.zone.sequencer_channel_id(&sequencer_alias)?,
        signing_key,
        ZoneNodeHttpClient::new(CommonHttpClient::new(None), node_url),
        config,
        checkpoint,
    );

    let runtime = start_sequencer_runtime(sequencer, handle.clone(), mode);

    if let Err(error) = wait_for_sequencer_ready(&sequencer_alias, &node_client, &mut handle).await
    {
        runtime.task.abort();
        return Err(error);
    }

    world.zone.set_sequencer_runtime(
        sequencer_alias,
        handle,
        runtime.task,
        runtime.events,
        runtime.checkpoint_rx,
        runtime.discarded_payloads,
    );

    Ok(())
}

async fn wait_for_sequencer_ready(
    sequencer_alias: &str,
    node_client: &NodeHttpClient,
    handle: &mut SequencerHandle<ZoneNodeHttpClient>,
) -> StepResult {
    timeout(SEQUENCER_READY_TIMEOUT, async {
        let mut last_height = node_client.consensus_info().await?.cryptarchia_info.height;

        loop {
            if timeout(SEQUENCER_READY_POLL_TIMEOUT, handle.wait_ready())
                .await
                .is_ok()
            {
                return Ok(());
            }

            let _ = wait_for_height(
                node_client,
                last_height.saturating_add(1),
                SEQUENCER_READY_HEIGHT_ADVANCE_TIMEOUT,
            )
            .await;

            last_height = node_client
                .consensus_info()
                .await?
                .cryptarchia_info
                .height
                .max(last_height);
        }
    })
    .await
    .map_err(|_: Elapsed| StepError::Timeout {
        message: format!(
            "Sequencer `{sequencer_alias}` did not become ready within {} seconds",
            SEQUENCER_READY_TIMEOUT.as_secs()
        ),
    })?
}

fn start_sequencer_runtime(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    handle: SequencerHandle<ZoneNodeHttpClient>,
    mode: DriveMode,
) -> StartedSequencerRuntime {
    match mode {
        DriveMode::Passive { republish_orphans } => {
            let (task, events, checkpoint_rx) =
                start_sequencer_event_loop(sequencer, handle, republish_orphans);

            StartedSequencerRuntime {
                task,
                events: Some(events),
                checkpoint_rx: Some(checkpoint_rx),
                discarded_payloads: None,
            }
        }
        DriveMode::Republish => StartedSequencerRuntime {
            task: start_republish_policy(sequencer, handle),
            events: None,
            checkpoint_rx: None,
            discarded_payloads: None,
        },
        DriveMode::Sorted { discarded } => StartedSequencerRuntime {
            task: start_sorted_conflict_policy(sequencer, handle, Arc::clone(&discarded)),
            events: None,
            checkpoint_rx: None,
            discarded_payloads: Some(discarded),
        },
        DriveMode::BalanceAware {
            initial_balances,
            planned_payloads,
        } => StartedSequencerRuntime {
            task: start_balance_aware_policy(sequencer, handle, initial_balances, planned_payloads),
            events: None,
            checkpoint_rx: None,
            discarded_payloads: None,
        },
    }
}

pub(super) fn ensure_zone_sequencer_exists(world: &mut CucumberWorld, sequencer_alias: &str) {
    if world.zone.sequencer_signing_key(sequencer_alias).is_ok() {
        return;
    }

    world
        .zone
        .register_sequencer(sequencer_alias.to_owned(), keygen());
}
