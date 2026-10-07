use std::time::{Duration, Instant};

use cucumber::{gherkin::Step, then, when};
use futures::future::join_all;
use hex::ToHex as _;
use tokio::time::{sleep, timeout};
use tracing::info;

use crate::cucumber::{
    background_tasks::CONTINUOUS_NEXT_WALLET_LOAD_TASK,
    error::{StepError, StepResult},
    steps::{
        mempool::{
            actions::{
                prepare_transfer_transaction, submit_prepared_transaction_through_blend,
                submit_prepared_transaction_to_nodes, try_submit_invalid_transaction,
                wait_for_mempool_recovery_flush,
            },
            assertions::{
                assert_transaction_not_pending_on_all_nodes, assert_transaction_pending_on_nodes,
                assert_transaction_remains_not_pending_on_all_nodes,
            },
        },
        nodes::diagnostics::BlendDiagnosticEventLogger,
    },
    world::CucumberWorld,
};

#[when(expr = "I record mempool pending counts for {string} workload at {string}")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_record_mempool_pending_counts(
    world: &mut CucumberWorld,
    workload_mode: String,
    observation_phase: String,
) -> StepResult {
    record_mempool_pending_counts(world, &workload_mode, &observation_phase).await
}

#[when(expr = "I observe the {string} mempool load for {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_observe_mempool_load(
    world: &mut CucumberWorld,
    workload_mode: String,
    duration_seconds: u64,
) -> StepResult {
    observe_mempool_window(world, &workload_mode, duration_seconds, true).await
}

#[when(expr = "I observe the {string} mempool drain for {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_observe_mempool_drain(
    world: &mut CucumberWorld,
    workload_mode: String,
    duration_seconds: u64,
) -> StepResult {
    observe_mempool_window(world, &workload_mode, duration_seconds, false).await
}

#[when(expr = "I observe the {string} mempool drain for {int} epochs")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_observe_mempool_drain_for_epochs(
    world: &mut CucumberWorld,
    workload_mode: String,
    drain_epochs: u64,
) -> StepResult {
    observe_mempool_drain_for_epochs(world, &workload_mode, drain_epochs).await
}

async fn observe_mempool_window(
    world: &CucumberWorld,
    workload_mode: &str,
    duration_seconds: u64,
    check_load_health: bool,
) -> StepResult {
    let started = Instant::now();
    let duration = Duration::from_secs(duration_seconds);
    let mut sample = 0usize;

    loop {
        if check_load_health {
            world.ensure_background_task_healthy(CONTINUOUS_NEXT_WALLET_LOAD_TASK)?;
        }
        record_mempool_pending_counts(
            world,
            workload_mode,
            &format!(
                "{}_{sample}",
                if check_load_health {
                    "running"
                } else {
                    "draining"
                }
            ),
        )
        .await?;
        sample = sample.saturating_add(1);

        let remaining = duration.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        sleep(remaining.min(Duration::from_secs(30))).await;
    }

    if check_load_health {
        world.ensure_background_task_healthy(CONTINUOUS_NEXT_WALLET_LOAD_TASK)?;
    }
    BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
        "mempool_diagnostic_observation_completed",
        &serde_json::json!({
            "workload_mode": workload_mode,
            "observation_phase": if check_load_health { "load" } else { "drain" },
            "duration_seconds": started.elapsed().as_secs(),
            "sample_count": sample,
        }),
    );
    Ok(())
}

struct MempoolDrainObservation {
    requested_epochs: u32,
    start_epoch: u32,
    target_epoch: u32,
    end_epoch: u32,
    end_slot: u64,
    end_height: Option<u64>,
    duration: Duration,
    sample_count: usize,
    reference_node: String,
}

fn log_mempool_drain_observation_completed(
    world: &CucumberWorld,
    workload_mode: &str,
    observation: &MempoolDrainObservation,
) {
    BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
        "mempool_diagnostic_observation_completed",
        &serde_json::json!({
            "workload_mode": workload_mode,
            "observation_phase": "drain",
            "duration_epochs_requested": observation.requested_epochs,
            "start_epoch": observation.start_epoch,
            "target_epoch": observation.target_epoch,
            "end_epoch": observation.end_epoch,
            "end_slot": observation.end_slot,
            "end_height": observation.end_height,
            "duration_seconds": observation.duration.as_secs(),
            "sample_count": observation.sample_count,
            "reference_node": observation.reference_node,
        }),
    );
    info!(
        workload_mode,
        reference_node = observation.reference_node,
        start_epoch = observation.start_epoch,
        target_epoch = observation.target_epoch,
        end_epoch = observation.end_epoch,
        samples = observation.sample_count,
        duration_seconds = observation.duration.as_secs(),
        "Completed epoch-based mempool drain observation"
    );
}

async fn observe_mempool_drain_for_epochs(
    world: &CucumberWorld,
    workload_mode: &str,
    drain_epochs: u64,
) -> StepResult {
    if drain_epochs == 0 {
        return Err(StepError::InvalidArgument {
            message: "mempool drain epoch count must be greater than zero".to_owned(),
        });
    }
    let drain_epochs = u32::try_from(drain_epochs).map_err(|_| StepError::InvalidArgument {
        message: format!("mempool drain epoch count {drain_epochs} exceeds the supported range"),
    })?;

    let mut node_names = world.all_node_names();
    node_names.sort();
    let reference_node = node_names.first().ok_or(StepError::MissingTopology)?;
    let reference_client = world.resolve_node_http_client(reference_node)?;
    let initial_time = timeout(Duration::from_secs(10), reference_client.time_info())
        .await
        .map_err(|_| StepError::Timeout {
            message: format!("timed out reading the starting epoch from `{reference_node}`"),
        })?
        .map_err(|error| StepError::StepFail {
            message: format!(
                "failed to read the starting epoch from reference node `{reference_node}`: {error}"
            ),
        })?;
    let start_epoch = initial_time.current_epoch;
    let target_epoch = start_epoch.checked_add(drain_epochs).ok_or_else(|| {
        StepError::InvalidArgument {
            message: format!(
                "mempool drain target epoch overflows: current epoch {start_epoch}, requested {drain_epochs}"
            ),
        }
    })?;
    let started = Instant::now();
    let maximum_wait = Duration::from_secs(u64::from(drain_epochs).saturating_mul(15 * 60));
    let mut observed_epoch = start_epoch;
    let mut end_slot = initial_time.current_slot;
    let mut last_sample = started;
    let mut sample = 0usize;

    record_mempool_pending_counts(
        world,
        workload_mode,
        &format!("draining_epoch_{observed_epoch}_{sample}"),
    )
    .await?;
    sample = sample.saturating_add(1);

    while observed_epoch < target_epoch {
        if started.elapsed() >= maximum_wait {
            return Err(StepError::Timeout {
                message: format!(
                    "mempool drain on `{reference_node}` did not advance from epoch {start_epoch} to {target_epoch} within {} seconds",
                    maximum_wait.as_secs()
                ),
            });
        }
        sleep(Duration::from_secs(1)).await;
        let time_info = timeout(Duration::from_secs(10), reference_client.time_info())
            .await
            .map_err(|_| StepError::Timeout {
                message: format!("timed out reading the current epoch from `{reference_node}`"),
            })?
            .map_err(|error| StepError::StepFail {
                message: format!(
                    "failed to read the current epoch from reference node `{reference_node}`: {error}"
                ),
            })?;

        let epoch_advanced = time_info.current_epoch > observed_epoch;
        if epoch_advanced {
            observed_epoch = time_info.current_epoch;
            end_slot = time_info.current_slot;
        }
        if epoch_advanced || last_sample.elapsed() >= Duration::from_secs(30) {
            record_mempool_pending_counts(
                world,
                workload_mode,
                &format!("draining_epoch_{observed_epoch}_{sample}"),
            )
            .await?;
            sample = sample.saturating_add(1);
            last_sample = Instant::now();
        }
    }

    let final_consensus = reference_client.consensus_info().await.ok();
    log_mempool_drain_observation_completed(
        world,
        workload_mode,
        &MempoolDrainObservation {
            requested_epochs: drain_epochs,
            start_epoch,
            target_epoch,
            end_epoch: observed_epoch,
            end_slot,
            end_height: final_consensus.map(|info| info.cryptarchia_info.height),
            duration: started.elapsed(),
            sample_count: sample,
            reference_node: reference_node.clone(),
        },
    );
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep concurrent node sampling and its explicit coverage handling together"
)]
pub(crate) async fn record_mempool_pending_counts(
    world: &CucumberWorld,
    workload_mode: &str,
    observation_phase: &str,
) -> StepResult {
    let mut node_names = world.all_node_names();
    node_names.retain(|node_name| !world.lifecycle.node_stopped_at.contains_key(node_name));
    node_names.sort();

    let expected_node_count = node_names.len();
    let observations = join_all(node_names.into_iter().map(async |node_name| {
        match world.resolve_node_http_client(&node_name) {
            Ok(client) => {
                let (metrics, consensus) =
                    tokio::join!(client.mantle_metrics(), client.consensus_info());
                let cryptarchia_info = consensus.as_ref().ok().map(|info| &info.cryptarchia_info);
                PendingNodeObservation {
                    node_name,
                    pending_count: metrics.as_ref().ok().map(|metrics| metrics.pending_items),
                    height: cryptarchia_info.map(|info| info.height),
                    chain_tip_slot: cryptarchia_info.map(|info| u64::from(info.slot)),
                    chain_tip_id: cryptarchia_info.map(|info| info.tip.encode_hex::<String>()),
                    chain_lib_slot: cryptarchia_info.map(|info| u64::from(info.lib_slot)),
                    chain_lib_id: cryptarchia_info.map(|info| info.lib.encode_hex::<String>()),
                    chain_state: cryptarchia_info.map(|info| format!("{:?}", info.state)),
                    metrics_error: metrics.as_ref().err().map(ToString::to_string),
                    consensus_info_error: consensus.as_ref().err().map(ToString::to_string),
                }
            }
            Err(error) => PendingNodeObservation {
                node_name,
                pending_count: None,
                height: None,
                chain_tip_slot: None,
                chain_tip_id: None,
                chain_lib_slot: None,
                chain_lib_id: None,
                chain_state: None,
                metrics_error: Some(error.to_string()),
                consensus_info_error: Some(error.to_string()),
            },
        }
    }))
    .await;
    let summary = summarize_pending_observations(expected_node_count, &observations);
    let nodes = observations
        .iter()
        .map(PendingNodeObservation::to_timeline_record)
        .collect::<Vec<_>>();

    info!(
        workload_mode,
        observation_phase,
        expected_node_count = summary.expected_node_count,
        successful_node_count = summary.successful_node_count,
        failed_node_count = summary.failed_node_count,
        complete_snapshot = summary.complete_snapshot,
        pending_count_coverage_complete = summary.pending_count_coverage_complete,
        total_pending_count = summary.total_pending_count,
        max_node_pending_count = ?summary.max_node_pending_count,
        "Mempool pending-count diagnostic snapshot"
    );

    BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
        "mempool_pending_count_snapshot",
        &serde_json::json!({
            "workload_mode": workload_mode,
            "observation_phase": observation_phase,
            "expected_node_count": summary.expected_node_count,
            "successful_node_count": summary.successful_node_count,
            "failed_node_count": summary.failed_node_count,
            "complete_snapshot": summary.complete_snapshot,
            "successful_metrics_node_count": summary.successful_metrics_node_count,
            "pending_count_coverage_complete": summary.pending_count_coverage_complete,
            "total_pending_count": summary.total_pending_count,
            "max_node_pending_count": summary.max_node_pending_count,
            "nodes": nodes,
        }),
    );

    if observation_phase.starts_with("burst_")
        && observation_phase.ends_with("_submitted")
        && !summary.pending_count_coverage_complete
    {
        let failures = observations
            .iter()
            .filter(|observation| observation.pending_count.is_none())
            .map(|observation| {
                format!(
                    "{}: {}",
                    observation.node_name,
                    observation
                        .metrics_error
                        .as_deref()
                        .unwrap_or("pending metrics unavailable")
                )
            })
            .collect::<Vec<_>>();
        return Err(StepError::StepFail {
            message: format!(
                "Immediate post-network mempool snapshot was incomplete: metrics succeeded on \
                {}/{} running nodes; failures: {}",
                summary.successful_metrics_node_count,
                summary.expected_node_count,
                failures.join("; ")
            ),
        });
    }

    Ok(())
}

struct PendingNodeObservation {
    node_name: String,
    pending_count: Option<usize>,
    height: Option<u64>,
    chain_tip_slot: Option<u64>,
    chain_tip_id: Option<String>,
    chain_lib_slot: Option<u64>,
    chain_lib_id: Option<String>,
    chain_state: Option<String>,
    metrics_error: Option<String>,
    consensus_info_error: Option<String>,
}

impl PendingNodeObservation {
    fn to_timeline_record(&self) -> serde_json::Value {
        serde_json::json!({
            "node_name": &self.node_name,
            "height": self.height,
            "chain_tip_slot": self.chain_tip_slot,
            "chain_tip_id": &self.chain_tip_id,
            "chain_lib_slot": self.chain_lib_slot,
            "chain_lib_id": &self.chain_lib_id,
            "chain_state": &self.chain_state,
            "pending_count": self.pending_count,
            "metrics_error": &self.metrics_error,
            "consensus_info_error": &self.consensus_info_error,
        })
    }
}

struct PendingObservationSummary {
    expected_node_count: usize,
    successful_node_count: usize,
    failed_node_count: usize,
    successful_metrics_node_count: usize,
    complete_snapshot: bool,
    pending_count_coverage_complete: bool,
    total_pending_count: usize,
    max_node_pending_count: Option<usize>,
}

fn summarize_pending_observations(
    expected_node_count: usize,
    observations: &[PendingNodeObservation],
) -> PendingObservationSummary {
    let successful_metrics_node_count = observations
        .iter()
        .filter(|observation| observation.pending_count.is_some())
        .count();
    let successful_node_count = observations
        .iter()
        .filter(|observation| observation.pending_count.is_some() && observation.height.is_some())
        .count();
    let pending_counts = observations
        .iter()
        .filter_map(|observation| observation.pending_count)
        .collect::<Vec<_>>();
    let pending_count_coverage_complete =
        expected_node_count > 0 && successful_metrics_node_count == expected_node_count;

    PendingObservationSummary {
        expected_node_count,
        successful_node_count,
        failed_node_count: expected_node_count.saturating_sub(successful_node_count),
        successful_metrics_node_count,
        complete_snapshot: expected_node_count > 0 && successful_node_count == expected_node_count,
        pending_count_coverage_complete,
        total_pending_count: pending_counts.iter().sum(),
        max_node_pending_count: pending_counts.into_iter().max(),
    }
}

#[when(
    expr = "I prepare transfer transaction {string} of {int} LGO from wallet {string} to wallet {string}"
)]
async fn step_prepare_transfer_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    amount: u64,
    sender_wallet_name: String,
    receiver_wallet_name: String,
) -> StepResult {
    prepare_transfer_transaction(
        world,
        &step.value,
        transaction_alias,
        amount,
        sender_wallet_name,
        receiver_wallet_name,
    )
    .await
}

#[when(expr = "I submit prepared transaction {string} to nodes:")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_submit_prepared_transaction_to_nodes(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
) -> StepResult {
    let node_names = parse_node_names_table(step)?;

    submit_prepared_transaction_to_nodes(world, &step.value, transaction_alias, node_names).await
}

#[when(expr = "I submit prepared transaction {string} through Blend on node {string}")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_submit_prepared_transaction_through_blend(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    node_name: String,
) -> StepResult {
    submit_prepared_transaction_through_blend(world, &step.value, &transaction_alias, &node_name)
        .await
}

#[when(expr = "I try to submit invalid transaction {string} to node {string}")]
async fn step_try_submit_invalid_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    node_name: String,
) -> StepResult {
    try_submit_invalid_transaction(world, &step.value, transaction_alias, node_name).await
}

#[then(expr = "mempool recovery for node {string} contains transaction {string}")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_wait_for_pending_mempool_recovery_flush(
    world: &mut CucumberWorld,
    node_name: String,
    transaction_alias: String,
) -> StepResult {
    wait_for_mempool_recovery_flush(world, &node_name, &transaction_alias).await
}

#[then(expr = "transaction {string} is pending in mempool of nodes in {int} seconds:")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_transaction_pending_in_node_mempools(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let node_names = parse_node_names_table(step)?;

    assert_transaction_pending_on_nodes(world, transaction_alias, node_names, timeout_seconds).await
}

#[then(expr = "transaction {string} is not pending in mempool of all nodes in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_transaction_not_pending_in_all_mempools(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let _ = step;

    assert_transaction_not_pending_on_all_nodes(world, transaction_alias, timeout_seconds).await
}

#[then(
    expr = "transaction {string} remains not pending in mempool of all nodes for {int} blocks in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_transaction_remains_not_pending_in_all_mempools(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    blocks: u64,
    timeout_seconds: u64,
) -> StepResult {
    let _ = step;

    assert_transaction_remains_not_pending_on_all_nodes(
        world,
        transaction_alias,
        blocks,
        timeout_seconds,
    )
    .await
}

fn parse_node_names_table(step: &Step) -> Result<Vec<String>, StepError> {
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;

    if table.rows.is_empty() || table.rows[0].len() != 1 || table.rows[0][0].trim() != "node_name" {
        return Err(StepError::InvalidArgument {
            message: "Expected table columns: | node_name |".to_owned(),
        });
    }

    table
        .rows
        .iter()
        .skip(1)
        .map(|row| {
            if row.len() != 1 {
                return Err(StepError::InvalidArgument {
                    message: "Each node row must have exactly one column".to_owned(),
                });
            }

            Ok(row[0].trim().to_owned())
        })
        .collect()
}

#[cfg(test)]
mod pending_metrics_tests {
    use super::*;

    fn observation(
        node_name: &str,
        pending_count: Option<usize>,
        height: Option<u64>,
    ) -> PendingNodeObservation {
        PendingNodeObservation {
            node_name: node_name.to_owned(),
            pending_count,
            height,
            chain_tip_slot: None,
            chain_tip_id: None,
            chain_lib_slot: None,
            chain_lib_id: None,
            chain_state: None,
            metrics_error: pending_count
                .is_none()
                .then(|| "metrics unavailable".to_owned()),
            consensus_info_error: height.is_none().then(|| "consensus unavailable".to_owned()),
        }
    }

    #[test]
    fn all_node_metrics_produce_complete_pending_coverage() {
        let observations = [
            observation("NODE_A", Some(10), Some(1)),
            observation("NODE_B", Some(20), Some(2)),
            observation("NODE_C", Some(30), Some(3)),
        ];

        let summary = summarize_pending_observations(3, &observations);
        assert_eq!(summary.expected_node_count, 3);
        assert_eq!(summary.successful_node_count, 3);
        assert_eq!(summary.failed_node_count, 0);
        assert_eq!(summary.successful_metrics_node_count, 3);
        assert!(summary.complete_snapshot);
        assert!(summary.pending_count_coverage_complete);
        assert_eq!(summary.total_pending_count, 60);
        assert_eq!(summary.max_node_pending_count, Some(30));
    }

    #[test]
    fn failed_node_metrics_mark_the_aggregate_as_partial() {
        let observations = [
            observation("NODE_A", Some(10), Some(1)),
            observation("NODE_B", None, Some(2)),
            observation("NODE_C", Some(30), Some(3)),
        ];

        let summary = summarize_pending_observations(3, &observations);
        assert_eq!(summary.successful_node_count, 2);
        assert_eq!(summary.failed_node_count, 1);
        assert_eq!(summary.successful_metrics_node_count, 2);
        assert!(!summary.complete_snapshot);
        assert!(!summary.pending_count_coverage_complete);
        assert_eq!(summary.total_pending_count, 40);
        assert_eq!(summary.max_node_pending_count, Some(30));
    }

    #[test]
    fn consensus_failure_marks_snapshot_incomplete_without_hiding_metric_coverage() {
        let observations = [
            observation("NODE_A", Some(10), Some(1)),
            observation("NODE_B", Some(20), None),
        ];

        let summary = summarize_pending_observations(2, &observations);
        assert_eq!(summary.successful_node_count, 1);
        assert_eq!(summary.failed_node_count, 1);
        assert!(!summary.complete_snapshot);
        assert!(summary.pending_count_coverage_complete);
        assert_eq!(summary.total_pending_count, 30);
    }

    #[test]
    fn timeline_node_record_serializes_cryptarchia_consensus_fields() {
        let observation = PendingNodeObservation {
            node_name: "NODE_A".to_owned(),
            pending_count: Some(12),
            height: Some(34),
            chain_tip_slot: Some(56),
            chain_tip_id: Some("ab12".to_owned()),
            chain_lib_slot: Some(45),
            chain_lib_id: Some("cd34".to_owned()),
            chain_state: Some("OnLine".to_owned()),
            metrics_error: None,
            consensus_info_error: None,
        };

        let record = observation.to_timeline_record();
        assert_eq!(record["height"], 34);
        assert_eq!(record["chain_tip_slot"], 56);
        assert_eq!(record["chain_tip_id"], "ab12");
        assert_eq!(record["chain_lib_slot"], 45);
        assert_eq!(record["chain_lib_id"], "cd34");
        assert_eq!(record["chain_state"], "OnLine");
        assert_eq!(record["pending_count"], 12);
    }

    #[test]
    fn timeline_node_record_keeps_consensus_fields_null_when_consensus_is_unavailable() {
        let record = observation("NODE_A", Some(12), None).to_timeline_record();

        assert_eq!(record["height"], serde_json::Value::Null);
        assert_eq!(record["chain_tip_slot"], serde_json::Value::Null);
        assert_eq!(record["chain_tip_id"], serde_json::Value::Null);
        assert_eq!(record["chain_lib_slot"], serde_json::Value::Null);
        assert_eq!(record["chain_lib_id"], serde_json::Value::Null);
        assert_eq!(record["chain_state"], serde_json::Value::Null);
    }
}
