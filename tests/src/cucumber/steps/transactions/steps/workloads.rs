use lb_testing_framework::NodeHttpClient;

use super::{
    CONTINUOUS_NEXT_WALLET_LOAD_TASK, ContinuousTransactionLoadProgress, CucumberWorld, Duration,
    ManualCommand, Step, StepError, StepResult, TARGET, execute_coin_splits_all_user_wallets,
    execute_continuous_next_wallet_user_wallet, execute_continuous_round_robin_user_wallets, info,
    parse_wallet_output_state, perform_manual_step_control, then, timeout,
    verify_min_outputs_all_user_wallets, warn, when,
};
use crate::cucumber::steps::nodes::diagnostics::BlendDiagnosticEventLogger;

const CONTINUOUS_LOAD_REFERENCE_NODE: &str = "NODE_9";
const CONTINUOUS_LOAD_CLOCK_QUERY_TIMEOUT: Duration = Duration::from_millis(1_500);

fn log_continuous_load_timeline_event(
    event_logger: &BlendDiagnosticEventLogger,
    event: &str,
    fields: &serde_json::Value,
) {
    event_logger.append_named_timeline_record(event, fields);
}

async fn best_effort_reference_clock(
    client: Option<&NodeHttpClient>,
    client_error: Option<&str>,
) -> (Option<u32>, Option<u64>, Option<String>) {
    let Some(client) = client else {
        return (None, None, client_error.map(str::to_owned));
    };

    match timeout(CONTINUOUS_LOAD_CLOCK_QUERY_TIMEOUT, client.time_info()).await {
        Ok(Ok(time_info)) => (
            Some(time_info.current_epoch),
            Some(time_info.current_slot),
            None,
        ),
        Ok(Err(error)) => (None, None, Some(error.to_string())),
        Err(_) => (
            None,
            None,
            Some(format!(
                "Time service query timed out after {}ms",
                CONTINUOUS_LOAD_CLOCK_QUERY_TIMEOUT.as_millis()
            )),
        ),
    }
}

#[when(expr = "I perform manual control of transactions for all wallets for {int} seconds")]
async fn step_manual_control_transactions(
    world: &mut CucumberWorld,
    step: &Step,
    timeout_seconds: u64,
) -> StepResult {
    perform_manual_step_control(world, &step.value, timeout_seconds).await
}

#[when(expr = "I perform manual control of transactions for all wallets no time-out")]
async fn step_manual_control_transactions_no_time_out(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    perform_manual_step_control(world, &step.value, u64::MAX).await
}

#[when(
    expr = "I perform continuous transactions on user wallets with {int} coin split outputs of {int} LGO, {int} transactions of {int} LGO each for {int} cycles with {int} epochs headroom"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "Cucumber step captures map directly to step arguments"
)]
async fn step_continuous_user_wallets(
    world: &mut CucumberWorld,
    step: &Step,
    coin_split_outputs: usize,
    coin_split_value: u64,
    transactions: usize,
    value: u64,
    cycles: usize,
    epochs_headroom: u32,
) -> StepResult {
    info!(
        target: TARGET,
        "Starting continuous user wallet transactions: coin_split_outputs={coin_split_outputs}, coin_split_value={coin_split_value}, transactions={transactions}, value={value}, cycles={cycles}"
    );

    execute_continuous_round_robin_user_wallets(
        world,
        &step.value,
        coin_split_outputs,
        coin_split_value,
        transactions,
        value,
        cycles,
        epochs_headroom,
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    info!(target: TARGET, "Completed continuous user wallet transactions step");

    Ok(())
}

#[when(
    expr = "I perform continuous transactions on user wallets with {int} coin split outputs of {int} LGO, {int} transactions of {int} LGO each for {int} cycles and timeout of {int} seconds with {int} epochs headroom"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "Cucumber step captures map directly to step function arguments."
)]
async fn step_continuous_user_wallets_with_timeout(
    world: &mut CucumberWorld,
    step: &Step,
    coin_split_outputs: usize,
    coin_split_value: u64,
    transactions: usize,
    value: u64,
    cycles: usize,
    timeout_seconds: u64,
    epochs_headroom: u32,
) -> StepResult {
    timeout(
        Duration::from_secs(timeout_seconds),
        step_continuous_user_wallets(
            world,
            step,
            coin_split_outputs,
            coin_split_value,
            transactions,
            value,
            cycles,
            epochs_headroom,
        ),
    )
    .await
    .map_err(|_| StepError::Timeout {
        message: format!(
            "continuous user wallet transactions did not finish within {timeout_seconds} seconds"
        ),
    })?
}

#[when(
    expr = "I perform {int} coin split transactions for each user wallet with {int} outputs of {int} LGO each"
)]
async fn step_coin_split_transactions_for_each_user_wallet(
    world: &mut CucumberWorld,
    step: &Step,
    splits_per_wallet: usize,
    outputs: usize,
    value: u64,
) -> StepResult {
    execute_coin_splits_all_user_wallets(world, &step.value, splits_per_wallet, outputs, value)
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

    Ok(())
}

#[when(expr = "I verify each wallet has minimum {int} outputs {string} in {int} seconds")]
async fn step_verify_each_wallet_minimum_outputs(
    world: &mut CucumberWorld,
    step: &Step,
    min_outputs: usize,
    wallet_state_type: String,
    timeout_seconds: u64,
) -> StepResult {
    verify_min_outputs_all_user_wallets(
        world,
        &step.value,
        min_outputs,
        timeout_seconds,
        parse_wallet_output_state(&wallet_state_type)
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{}` error: {e}", step.value);
            })
            .map_err(|e| StepError::InvalidArgument {
                message: e.to_string(),
            })?,
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    Ok(())
}

#[when(
    expr = "I perform {int} stress continuous cycles with {int} transactions of {int} LGO to the next user wallet with {int} epochs headroom"
)]
async fn step_perform_stress_continuous_cycles_next_user_wallet(
    world: &mut CucumberWorld,
    step: &Step,
    cycles: usize,
    num_transactions: usize,
    value: u64,
    epochs_headroom: u32,
) -> StepResult {
    execute_continuous_next_wallet_user_wallet(
        world,
        &step.value,
        &ManualCommand::ContinuousNextWalletUserWallets {
            cycles,
            num_transactions,
            value,
            epochs_headroom,
        },
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;

    Ok(())
}

#[when(
    expr = "I start continuous next-wallet transaction load with {int} transactions of {int} LGO and {int} epochs headroom"
)]
#[expect(
    clippy::too_many_lines,
    reason = "The Cucumber step owns one workload setup and its background lifecycle"
)]
fn step_start_continuous_next_wallet_load(
    world: &mut CucumberWorld,
    step: &Step,
    num_transactions: usize,
    value: u64,
    epochs_headroom: u32,
) -> StepResult {
    let load_nodes = ["NODE_9", "NODE_10", "NODE_11", "NODE_12"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut workload_world = world.background_workload_view(&load_nodes)?;
    let wallet_nodes = workload_world
        .all_user_wallets()
        .into_iter()
        .map(|wallet| wallet.node_name)
        .collect::<std::collections::HashSet<_>>();
    if let Some(node_name) = load_nodes
        .iter()
        .find(|node_name| !wallet_nodes.contains(*node_name))
    {
        return Err(StepError::InvalidArgument {
            message: format!("load node `{node_name}` has no user wallet"),
        });
    }

    let workload_step = step.value.clone();
    let wallet_count = wallet_nodes.len();
    let command = ManualCommand::ContinuousNextWalletUserWallets {
        cycles: 1,
        num_transactions,
        value,
        epochs_headroom,
    };
    let transactions_per_round = wallet_count * num_transactions;
    let event_logger = BlendDiagnosticEventLogger::from_world(world);
    let reference_time_client = world
        .resolve_node_http_client(CONTINUOUS_LOAD_REFERENCE_NODE)
        .ok();
    let reference_time_client_error = reference_time_client.is_none().then(|| {
        format!("could not resolve Time service client for {CONTINUOUS_LOAD_REFERENCE_NODE}")
    });
    info!(
        target: TARGET,
        "Starting background next-wallet transaction load across NODE_9..NODE_12: {num_transactions} transaction(s) per wallet per batch, {value} LGO each, {epochs_headroom} epochs headroom"
    );

    let progress = ContinuousTransactionLoadProgress::new(transactions_per_round);
    let task_progress = progress.clone();
    let task_event_logger = event_logger.clone();
    let task_wallet_nodes = load_nodes.clone();
    let task_reference_time_client = reference_time_client;
    world.continuous_transaction_load_progress = Some(progress);
    let spawn_result = world.spawn_background_task(
        CONTINUOUS_NEXT_WALLET_LOAD_TASK,
        async move |cancellation| {
            workload_world.background_best_node_selection.cancellation = Some(cancellation.clone());
            log_continuous_load_timeline_event(
                &task_event_logger,
                "continuous_transaction_load_started",
                &serde_json::json!({
                    "wallet_nodes": task_wallet_nodes,
                    "wallet_count": wallet_count,
                    "transactions_per_wallet_per_round": num_transactions,
                    "transactions_per_round": transactions_per_round,
                    "value": value,
                    "epochs_headroom": epochs_headroom,
                }),
            );

            loop {
                if *cancellation.borrow() {
                    return Ok(());
                }

                let (completed_rounds, completed_verified_transactions) = task_progress.snapshot();
                let round = completed_rounds.saturating_add(1);
                log_continuous_load_timeline_event(
                    &task_event_logger,
                    "continuous_transaction_load_round_started",
                    &serde_json::json!({
                        "round": round,
                        "transactions_per_round": transactions_per_round,
                        "completed_verified_transactions": completed_verified_transactions,
                    }),
                );

                if let Err(error) = execute_continuous_next_wallet_user_wallet(
                    &mut workload_world,
                    &workload_step,
                    &command,
                )
                .await
                {
                    if matches!(&error, StepError::BackgroundTaskCancelled) {
                        return Ok(());
                    }
                    let (completed_rounds, completed_verified_transactions) =
                        task_progress.snapshot();
                    log_continuous_load_timeline_event(
                        &task_event_logger,
                        "continuous_transaction_load_failed",
                        &serde_json::json!({
                            "failure_stage": "round_execution",
                            "round": completed_rounds.saturating_add(1),
                            "transactions_per_round": transactions_per_round,
                            "completed_rounds": completed_rounds,
                            "completed_verified_transactions": completed_verified_transactions,
                            "error": error.to_string(),
                        }),
                    );
                    return Err(error);
                }

                task_progress.record_completed_round();
                let (completed_rounds, completed_verified_transactions) = task_progress.snapshot();
                let (clock_epoch, clock_slot, clock_query_error) = best_effort_reference_clock(
                    task_reference_time_client.as_ref(),
                    reference_time_client_error.as_deref(),
                )
                .await;
                log_continuous_load_timeline_event(
                    &task_event_logger,
                    "continuous_transaction_load_round_completed",
                    &serde_json::json!({
                        "round": completed_rounds,
                        "transactions_per_round": transactions_per_round,
                        "completed_verified_transactions": completed_verified_transactions,
                        "reference_node": CONTINUOUS_LOAD_REFERENCE_NODE,
                        "clock_epoch": clock_epoch,
                        "clock_slot": clock_slot,
                        "clock_query_error": clock_query_error,
                    }),
                );
            }
        },
    );

    if let Err(error) = spawn_result {
        log_continuous_load_timeline_event(
            &event_logger,
            "continuous_transaction_load_failed",
            &serde_json::json!({
                "failure_stage": "task_spawn",
                "wallet_nodes": load_nodes,
                "wallet_count": wallet_count,
                "transactions_per_wallet_per_round": num_transactions,
                "transactions_per_round": transactions_per_round,
                "value": value,
                "epochs_headroom": epochs_headroom,
                "error": error.to_string(),
            }),
        );
        world.continuous_transaction_load_progress = None;
        return Err(error);
    }

    Ok(())
}

#[then("the continuous transaction load task is healthy and has made progress")]
#[when("the continuous transaction load task is healthy and has made progress")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step entrypoints must accept `&mut World`"
)]
fn step_continuous_transaction_load_is_healthy_and_has_made_progress(
    world: &mut CucumberWorld,
) -> StepResult {
    let task_status = world.background_task_status(CONTINUOUS_NEXT_WALLET_LOAD_TASK)?;
    let progress = world
        .continuous_transaction_load_progress
        .as_ref()
        .ok_or_else(|| StepError::LogicalError {
            message: "continuous transaction load has no progress state".to_owned(),
        })?;
    let checkpoint = progress.checkpoint();
    info!(
        target: TARGET,
        event = "continuous_transaction_load_health_and_progress",
        task_status = %task_status,
        completed_rounds = checkpoint.completed_rounds,
        completed_verified_transactions = checkpoint.completed_verified_transactions,
        rounds_since_previous_check = checkpoint.rounds_since_previous_check,
        verified_transactions_since_previous_check =
            checkpoint.verified_transactions_since_previous_check,
        transactions_per_round = checkpoint.transactions_per_round,
        "Continuous next-wallet transaction load health and progress"
    );
    log_continuous_load_timeline_event(
        &BlendDiagnosticEventLogger::from_world(world),
        "continuous_transaction_load_health_and_progress",
        &serde_json::json!({
            "task_status": task_status.as_str(),
            "completed_rounds": checkpoint.completed_rounds,
            "completed_verified_transactions": checkpoint.completed_verified_transactions,
            "rounds_since_previous_check": checkpoint.rounds_since_previous_check,
            "verified_transactions_since_previous_check": checkpoint
                .verified_transactions_since_previous_check,
            "transactions_per_round": checkpoint.transactions_per_round,
        }),
    );
    world.ensure_background_task_healthy(CONTINUOUS_NEXT_WALLET_LOAD_TASK)?;
    let expected_verified_transactions = checkpoint
        .rounds_since_previous_check
        .saturating_mul(checkpoint.transactions_per_round);
    if checkpoint.rounds_since_previous_check == 0
        || checkpoint.verified_transactions_since_previous_check != expected_verified_transactions
    {
        return Err(StepError::StepFail {
            message: format!(
                "continuous next-wallet transaction load made insufficient progress since the previous check: {} completed round(s), {} verified transaction(s); expected at least one round and {expected_verified_transactions} verified transaction(s)",
                checkpoint.rounds_since_previous_check,
                checkpoint.verified_transactions_since_previous_check,
            ),
        });
    }
    Ok(())
}

#[when("I stop the continuous next-wallet transaction load")]
async fn step_stop_continuous_next_wallet_load(world: &mut CucumberWorld) -> StepResult {
    let result = world
        .stop_background_task(CONTINUOUS_NEXT_WALLET_LOAD_TASK)
        .await;
    world.continuous_transaction_load_progress = None;
    result
}
