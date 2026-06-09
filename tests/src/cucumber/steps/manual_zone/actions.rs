use std::{collections::HashMap, num::NonZero, time::Duration};

use cucumber::gherkin::Step;
use futures::future::join_all;
use lb_common_http_client::CommonHttpClient;
use lb_core::mantle::{
    TxHash, Utxo,
    ops::channel::{config::Keys, deposit::Metadata, inscribe::Inscription},
};
use lb_key_management_system_service::keys::Ed25519Key;
use lb_testing_framework::NodeHttpClient;
use lb_zone_sdk::{
    adapter::NodeHttpClient as ZoneNodeHttpClient, indexer::ZoneIndexer, sequencer::ZoneSequencer,
};
use tokio::{
    sync::broadcast,
    task::JoinHandle,
    time::{error::Elapsed, timeout},
};

use super::{
    errors::{log_step_error, zone_step_error},
    runner::{Event, PublishResult, SequencerCheckpoint, SequencerClient},
    steps::DEFAULT_ZONE_SEQUENCER,
    support::{
        AtomicZoneDepositRequest, DiscardedPayloads, PublishDeadline, ZoneAccountBalances,
        ZoneDeposit, build_zone_deposit, ensure_zone_transactions_included, keygen,
        publish_atomic_zone_withdraw, publish_message_with_retry, sequencer_config,
        sequencer_config_with_pending_submit_depth, start_balance_aware_policy,
        start_republish_policy, start_sequencer_event_loop, start_sorted_conflict_policy,
        submit_atomic_zone_deposit, submit_zone_deposit, submit_zone_withdraw,
    },
    tables::{ConcurrentZoneMessageRow, ZoneNodeResourcesRow, group_zone_messages_by_sequencer},
};
use crate::{
    common::{
        mantle_inscription::make_inscription, manual_cluster::wait_for_height,
        wallet::WalletReservedInputs,
    },
    cucumber::{
        error::{StepError, StepResult},
        steps::{
            TARGET,
            manual_nodes::{
                config_override::set_deployment_config_override,
                utils::{
                    NodesToStartUnordered, WalletStartInfo, start_node,
                    start_nodes_order_respecting_dependencies,
                },
            },
        },
        wallet::sync::sync_available_utxos_for_wallet,
        world::{CucumberWorld, WalletInfo},
    },
};

const ZONE_CHANNEL_WITHDRAW_THRESHOLD: u16 = 1;
const ZONE_CHANNEL_DEPOSIT_THRESHOLD: u16 = 1;
const SEQUENCER_READY_TIMEOUT: Duration = Duration::from_mins(2);
const SEQUENCER_READY_POLL_TIMEOUT: Duration = Duration::from_secs(10);
const SEQUENCER_READY_HEIGHT_ADVANCE_TIMEOUT: Duration = Duration::from_secs(30);
const ZONE_SECURITY_PARAM: u32 = 5;

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
    client: SequencerClient<ZoneNodeHttpClient>,
    events: broadcast::Receiver<Event>,
    checkpoint_rx: tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>,
    ready_rx: tokio::sync::watch::Receiver<bool>,
    channel_view_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::SequencerChannelView>,
    turn_to_write_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::TurnNotification>,
    discarded_payloads: Option<DiscardedPayloads>,
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

pub(super) async fn start_nodes_with_zone_resources(
    world: &mut CucumberWorld,
    step: &Step,
    rows: Vec<ZoneNodeResourcesRow>,
) -> StepResult {
    apply_zone_timing_defaults(world, &step.value)?;

    let nodes = collect_zone_nodes_to_start(&rows);
    let nodes = start_nodes_order_respecting_dependencies(nodes).inspect_err(|error| {
        tracing::warn!(target: TARGET, "Step `{}` error: {error}", step.value);
    })?;

    for (node_name, wallet_start_info, mut initial_peers) in nodes {
        initial_peers.sort();
        initial_peers.dedup();

        start_node(
            world,
            &step.value,
            &node_name,
            &wallet_start_info,
            &initial_peers,
            false,
        )
        .await?;
    }

    register_zone_resources(world, rows)
}

fn apply_zone_timing_defaults(world: &mut CucumberWorld, step: &str) -> StepResult {
    world.set_cryptarchia_security_param(
        NonZero::new(ZONE_SECURITY_PARAM).expect("zone security parameter is non-zero"),
    );
    world.set_prolonged_bootstrap_period(Duration::ZERO);

    set_deployment_config_override(world, step, "time.slot_duration", "seconds(1)")?;
    set_deployment_config_override(
        world,
        step,
        "cryptarchia.slot_activation_coeff.numerator",
        "1",
    )?;
    set_deployment_config_override(
        world,
        step,
        "cryptarchia.slot_activation_coeff.denominator",
        "2",
    )
}

fn collect_zone_nodes_to_start(rows: &[ZoneNodeResourcesRow]) -> NodesToStartUnordered {
    let mut nodes = HashMap::new();

    for row in rows {
        let entry = nodes
            .entry(row.node_name.clone())
            .or_insert_with(|| (Vec::new(), Vec::new()));

        entry.0.push(WalletStartInfo {
            wallet_name: row.wallet_name.clone(),
            account_index: row.account_index,
        });

        if let Some(peer) = &row.connected_to {
            entry.1.push(peer.clone());
        }
    }

    nodes
}

fn register_zone_resources(
    world: &mut CucumberWorld,
    rows: Vec<ZoneNodeResourcesRow>,
) -> StepResult {
    for row in rows {
        for alias in row.sequencers {
            if !world.zone.has_sequencer(&alias) {
                world.zone.register_sequencer(alias.clone(), keygen());
            }

            world.zone.attach_sequencer_resources(
                &alias,
                row.node_name.clone(),
                row.wallet_name.clone(),
            )?;
        }
    }

    Ok(())
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
    let handle = log_step_error(step, world.zone.sequencer_client(sequencer_alias))?;
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

    let (result, post_call_checkpoint, _signed_tx) = handle
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

    // Sanity-check the inline checkpoint already mentions our tx; the
    // event-stream watcher below also catches it once the drive task
    // re-publishes its checkpoint after the next block.
    let tx_hash = result.inscription_id();
    let checkpoint = if post_call_checkpoint
        .pending_txs
        .iter()
        .any(|(hash, _)| *hash == tx_hash)
    {
        post_call_checkpoint
    } else {
        timeout(
            Duration::from_secs(30),
            wait_for_checkpoint_with_tx(&mut checkpoint_rx, tx_hash),
        )
        .await
        .map_err(|_| StepError::LogicalError {
            message: format!(
                "timed out waiting for sequencer '{sequencer_alias}' checkpoint to include {tx_hash:?}",
            ),
        })?
        .map_err(|message| StepError::LogicalError { message })?
    };

    world
        .zone
        .set_latest_checkpoint_for(sequencer_alias, checkpoint.clone());
    world
        .zone
        .remember_checkpoint(format!("{transaction_alias}_CHECKPOINT"), checkpoint);
    world.remember_submitted_transaction(transaction_alias, tx_hash);

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
        Some(result.inscription_id()),
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
        if let Some(checkpoint) = snapshot
            && checkpoint
                .pending_txs
                .iter()
                .any(|(hash, _)| *hash == tx_hash)
        {
            return Ok(checkpoint);
        }

        rx.changed()
            .await
            .map_err(|error| format!("checkpoint watch closed: {error}"))?;
    }
}

fn resolve_zone_wallet(
    world: &CucumberWorld,
    sequencer_alias: &str,
) -> Result<WalletInfo, StepError> {
    let wallet_name = world.zone.sequencer_default_wallet_name(sequencer_alias)?;

    world.resolve_wallet(wallet_name)
}

fn record_zone_wallet_submission(
    world: &CucumberWorld,
    wallet_name: &str,
    tx_hash: TxHash,
    reserved_inputs: Vec<Utxo>,
) -> StepResult {
    world.with_wallets_mut(|wallets| {
        wallets.record_wallet_reservation(
            wallet_name.to_owned(),
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
    let node_url = log_step_error(step, world.zone_node_url_for_sequencer(&channel_alias))?;
    let wallet = log_step_error(step, resolve_zone_wallet(world, &channel_alias))?;
    let public_key = log_step_error(step, wallet.public_key())?;
    let available_utxos = log_step_error(
        step,
        sync_available_utxos_for_wallet(world, &step.value, &wallet.wallet_name).await,
    )?;
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

    let response = submit_zone_deposit(&node_url, &deposit, public_key)
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    world
        .zone
        .remember_submitted_deposit(transaction_alias.clone(), deposit, amount);
    record_zone_wallet_submission(world, &wallet.wallet_name, response, reserved_inputs)?;
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
    let node_url = log_step_error(step, world.zone_node_url_for_sequencer(sequencer_alias))?;
    let wallet = log_step_error(step, resolve_zone_wallet(world, sequencer_alias))?;
    let public_key = log_step_error(step, wallet.public_key())?;
    let available_utxos = log_step_error(
        step,
        sync_available_utxos_for_wallet(world, &step.value, &wallet.wallet_name).await,
    )?;
    let sequencer = log_step_error(step, world.zone.sequencer_client(sequencer_alias))?;
    let inscription_data = make_inscription(&format!("Mint {amount} to Alice"));

    let submission = submit_atomic_zone_deposit(
        &node_url,
        sequencer,
        AtomicZoneDepositRequest {
            channel_id: world.zone.sequencer_channel_id(sequencer_alias)?,
            funding_public_key: public_key,
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
        &wallet.wallet_name,
        submission.publish.inscription_id(),
        submission.reserved_inputs,
    )?;
    world.remember_submitted_transaction(transaction_alias, submission.publish.inscription_id());

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
    let wallet = log_step_error(step, resolve_zone_wallet(world, sequencer_alias))?;
    let public_key = log_step_error(step, wallet.public_key())?;
    let sequencer = log_step_error(step, world.zone.sequencer_client(sequencer_alias))?;
    let inscription_data = make_inscription(&format!("Burn {amount}"));

    let submission = submit_zone_withdraw(
        sequencer,
        world.zone.sequencer_channel_id(sequencer_alias)?,
        public_key,
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
    world.remember_submitted_transaction(transaction_alias, submission.publish.inscription_id());

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
    let wallet = log_step_error(step, resolve_zone_wallet(world, sequencer_alias))?;
    let public_key = log_step_error(step, wallet.public_key())?;
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
        let sequencer = log_step_error(step, world.zone.sequencer_client(sequencer_alias))?.clone();

        publish_atomic_zone_withdraw(
            &sequencer,
            public_key,
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
    world.remember_submitted_transaction(bundle_alias, submission.publish.inscription_id());

    Ok(())
}

pub(super) fn initialize_zone_indexer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
) -> StepResult {
    let sequencer_alias = sequencer_alias.as_ref();
    let node_url = log_step_error(step, world.zone_node_url_for_sequencer(sequencer_alias))?;
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
    let node = log_step_error(
        step,
        world.zone_node_http_client_for_sequencer(&sequencer_alias),
    )?;

    let published = {
        let sequencer =
            log_step_error(step, world.zone.sequencer_client(&sequencer_alias))?.clone();

        let publish_deadline = PublishDeadline::from_now(Duration::from_mins(3));
        let mut published = Vec::with_capacity(rows.len());

        for (alias, payload) in &rows {
            let result = publish_message_with_retry(&sequencer, payload, publish_deadline)
                .await
                .map_err(|error| zone_step_error(step, &error))?;

            ensure_zone_transactions_included(
                &node,
                &[result.inscription_id()],
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
            log_step_error(step, world.zone.sequencer_client(sequencer_alias))
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
                handle.publish(payload).await.map_err(|error| {
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
    let node_client = log_step_error(
        step,
        world.zone_node_http_client_for_sequencer(&sequencer_alias),
    )?;
    let node_url = log_step_error(step, world.zone_node_url_for_sequencer(&sequencer_alias))?;
    let sequencer = ZoneSequencer::init_with_config(
        world.zone.sequencer_channel_id(&sequencer_alias)?,
        signing_key,
        ZoneNodeHttpClient::new(CommonHttpClient::new(None), node_url),
        config,
        checkpoint,
    );

    let runtime = start_sequencer_runtime(sequencer, mode);
    let mut ready_rx = runtime.ready_rx.clone();

    if let Err(error) =
        wait_for_sequencer_ready(&sequencer_alias, &node_client, &mut ready_rx).await
    {
        runtime.task.abort();
        return Err(error);
    }

    world.zone.set_sequencer_runtime(
        sequencer_alias,
        runtime.client,
        runtime.task,
        runtime.events,
        runtime.checkpoint_rx,
        runtime.ready_rx,
        runtime.channel_view_rx,
        runtime.turn_to_write_rx,
        runtime.discarded_payloads,
    );

    Ok(())
}

async fn wait_for_sequencer_ready(
    sequencer_alias: &str,
    node_client: &NodeHttpClient,
    ready_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> StepResult {
    timeout(SEQUENCER_READY_TIMEOUT, async {
        let mut last_height = node_client.consensus_info().await?.cryptarchia_info.height;

        loop {
            let poll = timeout(SEQUENCER_READY_POLL_TIMEOUT, async {
                loop {
                    if ready_rx.changed().await.is_err() {
                        return Err(());
                    }
                    if *ready_rx.borrow() {
                        return Ok(());
                    }
                }
            })
            .await;
            if matches!(poll, Ok(Ok(()))) {
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

fn from_policy_runtime(
    rt: super::support::PolicyRuntime,
    discarded_payloads: Option<DiscardedPayloads>,
) -> StartedSequencerRuntime {
    StartedSequencerRuntime {
        task: rt.task,
        client: rt.client,
        events: rt.events,
        checkpoint_rx: rt.checkpoint_rx,
        ready_rx: rt.ready_rx,
        channel_view_rx: rt.channel_view_rx,
        turn_to_write_rx: rt.turn_to_write_rx,
        discarded_payloads,
    }
}

fn start_sequencer_runtime(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    mode: DriveMode,
) -> StartedSequencerRuntime {
    match mode {
        DriveMode::Passive { republish_orphans } => from_policy_runtime(
            start_sequencer_event_loop(sequencer, republish_orphans),
            None,
        ),
        DriveMode::Republish => from_policy_runtime(start_republish_policy(sequencer), None),
        DriveMode::Sorted { discarded } => from_policy_runtime(
            start_sorted_conflict_policy(sequencer, &discarded),
            Some(discarded),
        ),
        DriveMode::BalanceAware {
            initial_balances,
            planned_payloads,
        } => from_policy_runtime(
            start_balance_aware_policy(sequencer, initial_balances, planned_payloads),
            None,
        ),
    }
}
