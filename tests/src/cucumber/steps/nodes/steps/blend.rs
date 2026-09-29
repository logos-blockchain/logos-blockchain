use std::{
    collections::BTreeSet,
    sync::{Arc, atomic::Ordering},
};

use cucumber::gherkin::Table;
use hex::ToHex as _;
use tokio::time::interval;

use super::*;

const BLEND_CHURN_TASK: &str = "epoch-driven Blend provider churn";
const BLEND_CHURN_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[given("Blend provider endpoints use controllable test relays")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step entrypoints must take `&mut World`"
)]
fn step_enable_blend_provider_relays(world: &mut CucumberWorld) -> StepResult {
    if !world.nodes_info.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "controllable Blend provider relays must be enabled before any node starts"
                .to_owned(),
        });
    }
    world
        .blend_relays
        .enable()
        .map_err(|error| StepError::LogicalError {
            message: format!("failed to enable controllable Blend provider relays: {error}"),
        })
}

#[when(expr = "I make Blend unreachable on node {string}")]
async fn step_make_blend_unreachable(world: &mut CucumberWorld, node_name: String) -> StepResult {
    set_blend_reachability(world, &node_name, false).await
}

#[when(expr = "I restore Blend reachability on node {string}")]
async fn step_restore_blend_reachability(
    world: &mut CucumberWorld,
    node_name: String,
) -> StepResult {
    set_blend_reachability(world, &node_name, true).await
}

#[when("I start epoch-driven Blend provider churn:")]
#[expect(
    clippy::too_many_lines,
    reason = "The scheduler setup and transition logging form one Cucumber operation"
)]
async fn step_start_epoch_driven_blend_churn(world: &mut CucumberWorld, step: &Step) -> StepResult {
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;
    let schedule = parse_blend_churn_schedule(table, &step.value)?;
    let providers = world.blend_relays.provider_reachability()?;
    if providers.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "epoch-driven Blend provider churn requires controllable provider relays"
                .to_owned(),
        });
    }
    let provider_names = providers
        .iter()
        .map(|(node_name, _)| node_name.clone())
        .collect::<BTreeSet<_>>();
    for (node_name, _) in &providers {
        if !world.nodes_info.contains_key(node_name) {
            return Err(StepError::LogicalError {
                message: format!("Blend provider `{node_name}` is not running"),
            });
        }
    }
    for desired_unreachable in &schedule {
        if let Some(node_name) = desired_unreachable
            .iter()
            .find(|node_name| !provider_names.contains(*node_name))
        {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "churn schedule names `{node_name}`, which has no controllable Blend provider relay"
                ),
            });
        }
    }

    let reference_node = world
        .blend_diagnostics
        .reference_node
        .clone()
        .ok_or_else(|| StepError::InvalidArgument {
            message: "observe an epoch transition before starting Blend provider churn".to_owned(),
        })?;
    let reference_client = world.resolve_node_http_client(&reference_node)?;
    let initial_time = reference_client
        .time_info()
        .await
        .map_err(|error| StepError::StepFail {
            message: format!(
                "failed to read the starting epoch from reference node `{reference_node}`: {error}"
            ),
        })?;
    let mut unreachable = providers
        .into_iter()
        .filter_map(|(node_name, enabled)| (!enabled).then_some(node_name))
        .collect::<BTreeSet<_>>();
    let relays = world.blend_relays.clone();
    let reachability = world.blend_diagnostics.reachability.clone();
    let event_logger = BlendDiagnosticEventLogger::from_world(world);
    let start_epoch = initial_time.current_epoch;
    if world.blend_churn_progress.is_some() {
        return Err(StepError::LogicalError {
            message: "Blend provider churn is already active".to_owned(),
        });
    }
    let rows_applied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    world.blend_churn_progress = Some(crate::cucumber::world::BlendChurnProgress {
        rows_applied: Arc::clone(&rows_applied),
        total_rows: schedule.len(),
    });

    let previous_phase = reachability.phase();
    reachability.set_phase(Some(crate::cucumber::world::BlendDiagnosticPhase::Outage));
    let task_reachability = reachability.clone();
    let start_logger = event_logger.clone();
    let initial_unreachable = unreachable.iter().cloned().collect::<Vec<_>>();
    let log_reference_node = reference_node.clone();
    let schedule_rows = schedule.len();
    let spawn_result = world.spawn_background_task(BLEND_CHURN_TASK, async move |mut cancellation| {
        let mut observed_epoch = start_epoch;
        let mut next_schedule_index = 0usize;
        let mut poll_interval = interval(BLEND_CHURN_POLL_INTERVAL);
        poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        info!(
            target: TARGET,
            event = "blend_churn_started",
                            reference_node,
                            start_epoch,
                            schedule_rows,
            "Started epoch-driven Blend provider churn"
        );

        loop {
            tokio::select! {
                result = cancellation.changed() => {
                    if result.is_err() || *cancellation.borrow() {
                        return Ok(());
                    }
                }
                _ = poll_interval.tick() => {
                    let time_info = reference_client.time_info().await.map_err(|error| {
                        StepError::StepFail {
                            message: format!("epoch-driven Blend churn could not query `{reference_node}` time: {error}"),
                        }
                    })?;

                    if let Some(skipped_epochs) = skipped_churn_epochs(
                        observed_epoch,
                        time_info.current_epoch,
                    ) && next_schedule_index < schedule.len() {
                        warn!(
                            target: TARGET,
                            event = "blend_churn_epoch_gap",
                            reference_node,
                            previous_epoch = observed_epoch,
                            current_epoch = time_info.current_epoch,
                            skipped_epochs,
                            next_transition_index = next_schedule_index + 1,
                            "Failing Blend churn because a scheduled provider state was skipped"
                        );
                        event_logger.append_timeline_record(&serde_json::json!({
                            "event": "blend_churn_epoch_gap",
                            "timestamp": OffsetDateTime::now_utc().to_string(),
                            "reference_node": reference_node,
                            "diagnostic_phase": "outage",
                            "previous_epoch": observed_epoch,
                            "current_epoch": time_info.current_epoch,
                            "skipped_epochs": skipped_epochs,
                            "next_transition_index": next_schedule_index + 1,
                        }));
                        return Err(StepError::StepFail {
                            message: format!(
                                "epoch-driven Blend churn on `{reference_node}` skipped {skipped_epochs} epoch(s) between epoch {observed_epoch} and {} before schedule row {}",
                                time_info.current_epoch,
                                next_schedule_index + 1,
                            ),
                        });
                    }

                    if time_info.current_epoch > observed_epoch {
                        observed_epoch = time_info.current_epoch;
                        let Some(desired_unreachable) = schedule.get(next_schedule_index) else {
                            continue;
                        };
                        let transition_index = next_schedule_index + 1;
                        let desired_unreachable = desired_unreachable.clone();
                        next_schedule_index += 1;
                        let transition: BlendReachabilityTransition =
                            apply_blend_reachability_set(
                            &relays,
                            &task_reachability,
                            &event_logger,
                            &mut unreachable,
                            &desired_unreachable,
                        )
                        .await?;

                        let current_unreachable = unreachable.iter().cloned().collect::<Vec<_>>();
                        let current_reachable = provider_names
                            .difference(&unreachable)
                            .cloned()
                            .collect::<Vec<_>>();
                        let chain = reference_client.consensus_info().await.ok().map(|info| {
                            (
                                info.cryptarchia_info.height,
                                u64::from(info.cryptarchia_info.lib_slot),
                                info.cryptarchia_info.lib.encode_hex::<String>(),
                            )
                        });
                        info!(
                            target: TARGET,
                            event = "blend_churn_transition",
                            reference_node,
                            epoch = observed_epoch,
                            transition_index,
                            desired_unreachable = ?desired_unreachable,
                            providers_restored = ?transition.restored,
                            providers_made_unreachable = ?transition.made_unreachable,
                            current_reachable = ?current_reachable,
                            current_unreachable = ?current_unreachable,
                            diagnostic_phase = task_reachability
                                .phase()
                                .map(crate::cucumber::world::BlendDiagnosticPhase::as_str),
                            chain_tip_height = chain.as_ref().map(|state| state.0),
                            chain_lib_slot = chain.as_ref().map(|state| state.1),
                            chain_lib_id = chain.as_ref().map(|state| state.2.as_str()),
                            "Applied complete Blend provider reachability set at epoch boundary"
                        );
                        event_logger.append_timeline_record(&serde_json::json!({
                            "event": "blend_churn_transition",
                            "timestamp": OffsetDateTime::now_utc().to_string(),
                            "reference_node": reference_node,
                            "epoch": observed_epoch,
                            "diagnostic_phase": task_reachability
                                .phase()
                                .map(crate::cucumber::world::BlendDiagnosticPhase::as_str),
                            "transition_index": transition_index,
                            "desired_unreachable": desired_unreachable,
                            "providers_restored": transition.restored,
                            "providers_made_unreachable": transition.made_unreachable,
                            "current_reachable": current_reachable,
                            "current_unreachable": current_unreachable,
                            "chain_tip_height": chain.as_ref().map(|state| state.0),
                            "chain_lib_slot": chain.as_ref().map(|state| state.1),
                            "chain_lib_id": chain.as_ref().map(|state| state.2.as_str()),
                        }));
                        rows_applied.fetch_add(1, Ordering::Release);
                    }
                }
            }
        }
    });

    if let Err(error) = spawn_result {
        reachability.set_phase(previous_phase);
        world.blend_churn_progress = None;
        return Err(error);
    }
    start_logger.append_timeline_record(&serde_json::json!({
        "event": "blend_churn_started",
        "timestamp": OffsetDateTime::now_utc().to_string(),
        "reference_node": log_reference_node,
        "start_epoch": start_epoch,
        "schedule_rows": schedule_rows,
        "diagnostic_phase": "outage",
        "current_unreachable": initial_unreachable,
    }));
    Ok(())
}

fn skipped_churn_epochs(previous_epoch: u32, current_epoch: u32) -> Option<u32> {
    current_epoch
        .checked_sub(previous_epoch)
        .filter(|distance| *distance > 1)
        .map(|distance| distance - 1)
}

#[cfg(test)]
mod tests {
    use super::skipped_churn_epochs;

    #[test]
    fn skipped_epoch_is_detected_instead_of_compressing_schedule_rows() {
        assert_eq!(skipped_churn_epochs(4, 6), Some(1));
        assert_eq!(skipped_churn_epochs(4, 5), None);
        assert_eq!(skipped_churn_epochs(4, 4), None);
        assert_eq!(skipped_churn_epochs(6, 4), None);
    }
}

fn parse_blend_churn_schedule(
    table: &Table,
    step: &str,
) -> Result<Vec<BTreeSet<String>>, StepError> {
    if table
        .rows
        .first()
        .is_none_or(|header| header != &["unreachable"])
        || table.rows.len() < 2
    {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` requires a non-empty one-column table headed `unreachable`"
            ),
        });
    }

    table
        .rows
        .iter()
        .skip(1)
        .map(|row| {
            if row.len() != 1 {
                return Err(StepError::InvalidArgument {
                    message: format!("Step `{step}` churn rows must have one column"),
                });
            }
            let mut unreachable = BTreeSet::new();
            for node_name in row[0].split(',').map(str::trim) {
                if node_name.is_empty() || !unreachable.insert(node_name.to_owned()) {
                    return Err(StepError::InvalidArgument {
                        message: format!(
                            "Step `{step}` has an empty or duplicate provider in `{}`",
                            row[0]
                        ),
                    });
                }
            }
            Ok(unreachable)
        })
        .collect()
}

#[when("I stop Blend provider churn")]
async fn step_stop_blend_provider_churn(world: &mut CucumberWorld) -> StepResult {
    let Some(progress) = world.blend_churn_progress.as_ref() else {
        return Err(StepError::LogicalError {
            message: "Blend provider churn has no active schedule".to_owned(),
        });
    };
    let rows_applied = Arc::clone(&progress.rows_applied);
    let total_rows = progress.total_rows;
    let deadline = Instant::now() + Duration::from_secs(30);
    while rows_applied.load(Ordering::Acquire) < total_rows {
        world.ensure_background_task_healthy(BLEND_CHURN_TASK)?;
        if Instant::now() >= deadline {
            return Err(StepError::Timeout {
                message: format!(
                    "Blend provider churn applied {} of {total_rows} epoch rows before stop",
                    rows_applied.load(Ordering::Acquire)
                ),
            });
        }
        sleep(Duration::from_millis(100)).await;
    }

    world.blend_churn_progress = None;
    world.stop_background_task(BLEND_CHURN_TASK).await?;
    BlendDiagnosticEventLogger::from_world(world).append_timeline_record(&serde_json::json!({
        "event": "blend_churn_stopped",
        "timestamp": OffsetDateTime::now_utc().to_string(),
        "reference_node": world.blend_diagnostics.reference_node,
        "diagnostic_phase": world
            .blend_diagnostics
            .reachability
            .phase()
            .map(crate::cucumber::world::BlendDiagnosticPhase::as_str),
        "current_unreachable": world
            .blend_diagnostics
            .reachability
            .unreachable_nodes(),
    }));
    Ok(())
}

#[when("I restore all Blend provider reachability")]
async fn step_restore_all_blend_provider_reachability(world: &mut CucumberWorld) -> StepResult {
    restore_all_blend_reachability(world).await
}

#[when(
    expr = "all nodes have at least {int} blocks and converged to within {int} blocks in {int} seconds"
)]
#[then(
    expr = "all nodes have at least {int} blocks and converged to within {int} blocks in {int} seconds"
)]
async fn step_all_nodes_reached_min_height_and_converged(
    world: &mut CucumberWorld,
    step: &Step,
    min_height: u64,
    max_diff_height: u64,
    time_out_seconds: u64,
) -> StepResult {
    nodes_converged(
        world,
        &step.value,
        Some(min_height),
        max_diff_height,
        time_out_seconds,
    )
    .await
}

#[when(expr = "all nodes agree on LIB in {int} seconds")]
#[then(expr = "all nodes agree on LIB in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_all_nodes_agree_on_lib(
    world: &mut CucumberWorld,
    step: &Step,
    time_out_seconds: u64,
) -> StepResult {
    ensure_all_nodes_agree_on_lib(world, &step.value, time_out_seconds).await
}

#[when("I wait for all nodes to be synced to the chain")]
#[then("I wait for all nodes to be synced to the chain")]
async fn step_wait_for_all_nodes_to_be_synced_to_the_chain(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    wait_for_all_nodes_to_be_synced_to_chain(world, &step.value).await
}

#[when("I query cryptarchia info for all nodes")]
#[then("I query cryptarchia info for all nodes")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_query_cryptarchia_info_all_nodes(world: &mut CucumberWorld, step: &Step) {
    get_cryptarchia_info_all_nodes(world, &step.value).await;
}

#[then(expr = "I stop all nodes")]
async fn step_stop_all_nodes(world: &mut CucumberWorld) -> StepResult {
    let mut background_cleanup_errors = Vec::new();
    if let Err(error) = world.stop_background_activity().await {
        background_cleanup_errors.push(error.to_string());
    }

    world.logos_sql.shutdown_all().await?;

    let runtime_dir_by_node_name: Vec<(String, String)> = world
        .nodes_info
        .iter()
        .map(|(node_name, info)| (node_name.clone(), info.started_node.name.clone()))
        .collect();

    if world.snapshots.save.extensions.is_some() {
        prepare_all_wallets_snapshot(world).await?;
    }

    world.reset_wallet_scanner_after_current_iteration().await;
    world.zone.clear();
    if let Err(error) = stop_active_manual_cluster(world) {
        background_cleanup_errors.push(error.to_string());
    }

    if let Some(snapshot_name) = world.snapshots.save.node_state.take() {
        create_snapshots_all_nodes(world, &snapshot_name)?;
    }

    if let Some(snapshot_name) = world.snapshots.save.extensions.take() {
        save_prepared_all_wallets_snapshot(&snapshot_name, world)?;
    }

    for (node_name, _) in &runtime_dir_by_node_name {
        info!(target: TARGET, "Stopping node '{node_name}'");
    }
    world.nodes_info.clear();

    if background_cleanup_errors.is_empty() {
        Ok(())
    } else {
        Err(StepError::StepFail {
            message: format!(
                "scenario background cleanup reported: {}",
                background_cleanup_errors.join("; ")
            ),
        })
    }
}

#[when(
    expr = "I send {int} transactions of {int} LGO each from wallet {string} to blend core zk key of node {string}"
)]
async fn step_send_multiple_transactions_to_blend_core_zk_key(
    world: &mut CucumberWorld,
    step: &Step,
    number_of_transactions: usize,
    output_value: u64,
    sender_wallet_name: String,
    receiver_node_name: String,
) -> StepResult {
    let receiver_blend_zk_pk = blend_zk_pk_for_node(world, &receiver_node_name)?;
    let sender_node_name = world.resolve_wallet(&sender_wallet_name)?.node_name;
    let sender_node_client = world
        .nodes_info
        .get(&sender_node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{sender_node_name}' not found in world state"),
        })?
        .started_node
        .client
        .clone();

    let mut available_utxos = WalletUtxos::new();
    let best_node_info = wait_wallet_send_ready(
        world,
        &step.value,
        &sender_wallet_name,
        180,
        number_of_transactions as u64 * output_value,
        WalletSendReadiness::TotalValueOnly,
        &mut available_utxos,
        &HashSet::new(),
    )
    .await?;

    for _ in 0..number_of_transactions {
        let tx_hashes = create_and_submit_transaction_hashes_with_utxo_cache(
            world,
            &step.value,
            &sender_wallet_name,
            &[(receiver_blend_zk_pk, output_value)],
            Some(&best_node_info),
            Some(&mut available_utxos),
        )
        .await
        .inspect_err(|error| {
            warn!(target: TARGET, "Step `{}` error: {error}", step.value);
        })?;

        wait_for_transactions_inclusion(&sender_node_client, &tx_hashes, Duration::from_mins(2))
            .await
            .inspect_err(|error| {
                warn!(target: TARGET, "Step `{}` error: {error}", step.value);
            })?;

        info!(
            target: TARGET,
            "Sent and included normal transaction from `{sender_wallet_name}` to blend zk key of {receiver_node_name}, value: {output_value}, tx count: {}",
            tx_hashes.len(),
        );
    }

    Ok(())
}

fn blend_zk_pk_for_node(world: &CucumberWorld, node_name: &str) -> Result<ZkPublicKey, StepError> {
    let node_info = world
        .nodes_info
        .get(node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{node_name}' not found in world state"),
        })?;

    let user_config_path = node_info.runtime_dir.join(USER_CONFIG_FILE);
    let blend_zk_pk_hex = blend_core_zk_pk_from_node_yaml(&user_config_path)?;
    let blend_zk_pk = ZkPublicKey::from_bytes(&hex::decode(blend_zk_pk_hex)?)?;

    Ok(blend_zk_pk)
}

/// Wait for the node-local wallet API to expose a funded note for a Blend key.
///
/// Blend ZK keys are read from node configuration and are not scenario wallets,
/// so the wallet scanner does not currently track them. Keep this exception
/// node-local because the returned note is immediately consumed by that node's
/// SDP declaration endpoint.
async fn wait_for_blend_funded_note(
    world: &CucumberWorld,
    node_name: &str,
    blend_zk_pk: ZkPublicKey,
) -> Result<lb_core::mantle::NoteId, StepError> {
    let base_url = world
        .nodes_info
        .get(node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{node_name}' not found in world state"),
        })?
        .started_node
        .client
        .base_url()
        .clone();
    let timeout = Duration::from_secs(30);
    let started = Instant::now();
    let client = CommonHttpClient::new(None);

    loop {
        let last_error = match client
            .get_wallet_balance(base_url.clone(), blend_zk_pk, None)
            .await
        {
            Ok(wallet_balance) => {
                if let Some(note_id) = wallet_balance.notes.keys().next().copied() {
                    return Ok(note_id);
                }
                "wallet has no notes yet".to_owned()
            }
            Err(error) => error.to_string(),
        };

        if started.elapsed() >= timeout {
            return Err(StepError::Timeout {
                message: format!(
                    "Timed out waiting for a funded note on Blend ZK key of '{node_name}' via \
                     '{base_url}' (last error: {last_error})"
                ),
            });
        }

        sleep(Duration::from_millis(250)).await;
    }
}

#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Required to be mutable by cucumber step function signature"
)]
#[expect(unused_variables, reason = "Cucumber step function signature")]
#[then(expr = "I declare node {string} as blend core node via the CLI binary")]
async fn step_run_blend_sdp_declaration_cli(
    world: &mut CucumberWorld,
    step: &Step,
    declarer_node_name: String,
) -> StepResult {
    let user_config_path = node_user_config_path(world, &declarer_node_name)?;
    let locator = blend_core_locator_from_node_yaml(&user_config_path)?;
    let blend_zk_pk = blend_zk_pk_for_node(world, &declarer_node_name)?;
    let service_note_id =
        wait_for_blend_funded_note(world, &declarer_node_name, blend_zk_pk).await?;
    let service_note_id_json =
        serde_json::to_string(&service_note_id).map_err(|error| StepError::LogicalError {
            message: format!("Failed to serialize service note ID: {error}"),
        })?;
    let service_note_id_hex = service_note_id_json.trim_matches('"').to_owned();

    let declarer_api_base_url = world
        .nodes_info
        .get(&declarer_node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{declarer_node_name}' not found in world state"),
        })?
        .started_node
        .client
        .base_url()
        .clone();

    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let output = tokio::process::Command::new("cargo")
        .current_dir(workspace_root)
        .arg("run")
        .arg("-p")
        .arg("logos-blockchain-tools")
        .arg("--bin")
        .arg("logos-blockchain-tools-api")
        .arg("--")
        .arg("sdp")
        .arg("post-blend-declaration")
        .arg("--user-config-path")
        .arg(user_config_path)
        .arg("--blend-addr")
        .arg(format!("{locator}"))
        .arg("--service-note-id")
        .arg(service_note_id_hex)
        .arg("--node-address")
        .arg(declarer_api_base_url.to_string())
        .output()
        .await?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(StepError::StepFail {
            message: format!(
                "Blend declaration CLI failed for node '{declarer_node_name}'\nstdout:\n{stdout}\nstderr:\n{stderr}"
            ),
        });
    }

    Ok(())
}

#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Required to be mutable by cucumber step function signature"
)]
#[then(expr = "I declare node {string} as blend core node via the API")]
async fn step_run_blend_sdp_declaration_api(
    world: &mut CucumberWorld,
    step: &Step,
    declarer_node_name: String,
) -> StepResult {
    let user_config_path = node_user_config_path(world, &declarer_node_name)?;
    let locator = blend_core_locator_from_node_yaml(&user_config_path)?;
    let blend_zk_pk = blend_zk_pk_for_node(world, &declarer_node_name)?;
    let service_note_id =
        wait_for_blend_funded_note(world, &declarer_node_name, blend_zk_pk).await?;

    let declarer_node_client = world
        .nodes_info
        .get(&declarer_node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{declarer_node_name}' not found in world state"),
        })?
        .started_node
        .client
        .clone();

    let declaration_id = declarer_node_client
        .join_blend_network(locator, service_note_id)
        .await
        .inspect_err(|error| {
            warn!(target: TARGET, "Step `{}` error: {error}", step.value);
        })?;

    info!(
        target: TARGET,
        "Node '{declarer_node_name}' joined blend core via API, declaration id: {declaration_id}"
    );

    Ok(())
}

fn node_user_config_path(world: &CucumberWorld, node_name: &str) -> Result<PathBuf, StepError> {
    let node_info = world
        .nodes_info
        .get(node_name)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node '{node_name}' not found in world state"),
        })?;

    Ok(node_info.runtime_dir.join(USER_CONFIG_FILE))
}

#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: Address this at some point."
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Required to be mutable by cucumber step function signature"
)]
#[then(expr = "blend core SDP declaration for node {string} is included on node {string}")]
async fn step_verify_blend_sdp_declaration_included(
    world: &mut CucumberWorld,
    step: &Step,
    declarer_node_name: String,
    api_node_name: String,
) -> StepResult {
    let blend_zk_pk = blend_zk_pk_for_node(world, &declarer_node_name)?;
    let service_note_id =
        wait_for_blend_funded_note(world, &declarer_node_name, blend_zk_pk).await?;

    let step_timeout = Duration::from_secs(30);
    let start_time = Instant::now();
    loop {
        let declarations_result = world
            .nodes_info
            .get(&api_node_name)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Node '{api_node_name}' not found in world state"),
            })?
            .started_node
            .client
            .get_sdp_declarations()
            .await;

        let declarations = match declarations_result {
            Ok(declarations) => declarations,
            Err(error) => {
                let error_message = error.to_string();
                if error_message.contains("404 Not Found") {
                    info!(
                        target: TARGET,
                        "Skipping declaration visibility assertion on '{api_node_name}' because testing SDP endpoint is unavailable: {error_message}",
                    );
                    return Ok(());
                }

                warn!(target: TARGET, "Step `{}` error: {error}", step.value);
                return Err(error.into());
            }
        };

        if declarations.values().any(|declaration| {
            declaration.service_note_id == service_note_id && declaration.zk_id == blend_zk_pk
        }) {
            info!(
                target: TARGET,
                "Blend declaration observed for node '{declarer_node_name}'"
            );
            break;
        }

        if start_time.elapsed() >= step_timeout {
            return Err(StepError::Timeout {
                message: format!(
                    "Timed out waiting for declaration submitted by '{declarer_node_name}' to appear on node '{api_node_name}'"
                ),
            });
        }

        sleep(Duration::from_millis(250)).await;
    }

    Ok(())
}
