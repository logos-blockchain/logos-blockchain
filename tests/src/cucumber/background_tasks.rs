use std::{
    collections::{HashMap, HashSet},
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex, atomic::AtomicUsize},
    time::Duration,
};

use futures::FutureExt as _;
use tokio::{sync::watch as tokio_watch, task::JoinHandle};
use tracing::warn;

use crate::cucumber::{
    TARGET,
    error::{StepError, StepResult},
    steps::nodes::diagnostics::BlendDiagnosticEventLogger,
    world::CucumberWorld,
};

pub(super) const CONTINUOUS_NEXT_WALLET_LOAD_TASK: &str = "continuous next-wallet transaction load";

#[derive(Default)]
pub(super) struct BackgroundBestNodeSelection {
    /// Keep diagnostic workloads waiting until a majority tip appears or the
    /// owning scenario cancels the task.
    pub(super) timeout_override: Option<Duration>,
    pub(super) cancellation: Option<tokio_watch::Receiver<bool>>,
    pub(super) timeline_logger: Option<BlendDiagnosticEventLogger>,
}

impl std::fmt::Debug for BackgroundBestNodeSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundBestNodeSelection")
            .field("timeout_override", &self.timeout_override)
            .field("has_cancellation", &self.cancellation.is_some())
            .field("has_timeline_logger", &self.timeline_logger.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BackgroundTaskStatus {
    Running,
    Failed(String),
    Stopped,
}

struct BackgroundTaskHandle {
    cancellation: tokio_watch::Sender<bool>,
    join: JoinHandle<()>,
    status: Arc<Mutex<BackgroundTaskStatus>>,
}

#[derive(Default)]
pub(super) struct BackgroundTasks {
    tasks: HashMap<String, BackgroundTaskHandle>,
}

impl std::fmt::Debug for BackgroundTasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut task_names = self.tasks.keys().collect::<Vec<_>>();
        task_names.sort();
        f.debug_struct("BackgroundTasks")
            .field("task_names", &task_names)
            .finish()
    }
}

impl BackgroundTasks {
    pub(super) fn abort_all(&mut self) {
        for (_, task) in self.tasks.drain() {
            task.join.abort();
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct ContinuousTransactionLoadProgress {
    completed_verified_transactions: Arc<AtomicUsize>,
    checkpoint_transactions: Arc<AtomicUsize>,
    transactions_per_round: usize,
}

fn log_continuous_transaction_load_stop_event(
    event_logger: &BlendDiagnosticEventLogger,
    event: &str,
    progress: &ContinuousTransactionLoadProgress,
    task_status: &str,
    error: Option<&str>,
) {
    let (completed_rounds, completed_verified_transactions) = progress.snapshot();
    event_logger.append_named_timeline_record(
        event,
        &serde_json::json!({
            "task_status": task_status,
            "completed_rounds": completed_rounds,
            "completed_verified_transactions": completed_verified_transactions,
            "transactions_per_round": progress.transactions_per_round(),
            "error": error,
        }),
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ContinuousTransactionLoadCheckpoint {
    pub(super) completed_rounds: usize,
    pub(super) completed_verified_transactions: usize,
    pub(super) rounds_since_previous_check: usize,
    pub(super) verified_transactions_since_previous_check: usize,
    pub(super) transactions_per_round: usize,
}

impl ContinuousTransactionLoadProgress {
    #[must_use]
    pub(super) fn new(transactions_per_round: usize) -> Self {
        Self {
            completed_verified_transactions: Arc::default(),
            checkpoint_transactions: Arc::default(),
            transactions_per_round,
        }
    }

    pub(super) fn record_completed_round(&self) {
        self.completed_verified_transactions.fetch_add(
            self.transactions_per_round,
            std::sync::atomic::Ordering::Release,
        );
    }

    #[must_use]
    const fn transactions_per_round(&self) -> usize {
        self.transactions_per_round
    }

    #[must_use]
    pub(super) fn snapshot(&self) -> (usize, usize) {
        let transactions = self
            .completed_verified_transactions
            .load(std::sync::atomic::Ordering::Acquire);
        let rounds = transactions
            .checked_div(self.transactions_per_round)
            .unwrap_or_default();
        (rounds, transactions)
    }

    #[must_use]
    pub(super) fn checkpoint(&self) -> ContinuousTransactionLoadCheckpoint {
        let completed_verified_transactions = self
            .completed_verified_transactions
            .load(std::sync::atomic::Ordering::Acquire);
        let previous_transactions = self.checkpoint_transactions.swap(
            completed_verified_transactions,
            std::sync::atomic::Ordering::AcqRel,
        );
        let verified_transactions_since_previous_check =
            completed_verified_transactions.saturating_sub(previous_transactions);
        let rounds_since_previous_check = verified_transactions_since_previous_check
            .checked_div(self.transactions_per_round)
            .unwrap_or_default();
        let completed_rounds = completed_verified_transactions
            .checked_div(self.transactions_per_round)
            .unwrap_or_default();

        ContinuousTransactionLoadCheckpoint {
            completed_rounds,
            completed_verified_transactions,
            rounds_since_previous_check,
            verified_transactions_since_previous_check,
            transactions_per_round: self.transactions_per_round,
        }
    }
}

async fn stop_background_task(name: &str, task: BackgroundTaskHandle) -> StepResult {
    let _ = task.cancellation.send(true);
    if let Err(error) = task.join.await {
        let task_error = format!("background task `{name}` failed while joining: {error}");
        if let Ok(mut status) = task.status.lock() {
            *status = BackgroundTaskStatus::Failed(task_error.clone());
        }
        return Err(StepError::StepFail {
            message: task_error,
        });
    }

    let status = task.status.lock().map_err(|_| StepError::LogicalError {
        message: format!("background task `{name}` status lock was poisoned"),
    })?;
    match &*status {
        BackgroundTaskStatus::Stopped => Ok(()),
        BackgroundTaskStatus::Failed(error) => Err(StepError::StepFail {
            message: format!("background task `{name}` failed: {error}"),
        }),
        BackgroundTaskStatus::Running => Err(StepError::StepFail {
            message: format!("background task `{name}` exited without recording its status"),
        }),
    }
}

#[expect(
    clippy::multiple_inherent_impl,
    reason = "Keep background-task behavior with its Cucumber-specific task state"
)]
impl CucumberWorld {
    /// Build an owned Cucumber world view for a detached transaction workload.
    /// Node clients, wallet observations, and scanner observations remain
    /// shared with the owning scenario world; only the selected user wallets
    /// are visible to the workload.
    pub(super) fn background_workload_view(
        &self,
        user_wallet_node_names: &[String],
    ) -> Result<Self, StepError> {
        let selected_nodes = user_wallet_node_names
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        if selected_nodes.is_empty() {
            return Err(StepError::InvalidArgument {
                message: "background transaction load requires at least one wallet node".to_owned(),
            });
        }
        for node_name in &selected_nodes {
            if !self.nodes_info.contains_key(node_name) {
                return Err(StepError::LogicalError {
                    message: format!(
                        "background transaction load node `{node_name}` is not running"
                    ),
                });
            }
        }

        let mut background = Self::default();
        background.chain.slots_per_epoch = self.chain.slots_per_epoch;
        background.background_best_node_selection.timeout_override = Some(Duration::MAX);
        background.background_best_node_selection.timeline_logger =
            Some(BlendDiagnosticEventLogger::from_world(self));
        background.nodes_info = self
            .nodes_info
            .iter()
            .map(|(node_name, node_info)| (node_name.clone(), node_info.clone()))
            .collect();
        background.fork_groups = self.fork_groups.clone();
        background.wallet_registry.wallet_info = self
            .wallet_registry
            .wallet_info
            .iter()
            .filter(|(_, wallet)| {
                wallet.is_user_wallet() && selected_nodes.contains(&wallet.node_name)
            })
            .map(|(name, wallet)| (name.clone(), wallet.clone()))
            .collect();
        if background.wallet_registry.wallet_info.is_empty() {
            return Err(StepError::InvalidArgument {
                message: "background transaction load nodes have no user wallets".to_owned(),
            });
        }
        background.wallet_registry.wallets = Arc::clone(&self.wallet_registry.wallets);
        background.wallet_registry.fee_state = self.wallet_registry.fee_state.clone();
        background.scanner.state = Arc::clone(&self.scanner.state);
        background.scanner.observed_transaction_hashes =
            Arc::clone(&self.scanner.observed_transaction_hashes);
        background.scanner.runtime_is_shared = true;

        Ok(background)
    }

    /// Spawn a scenario-owned task whose cancellation and result are tracked
    /// until explicitly joined.
    pub(super) fn spawn_background_task<F, Fut>(&mut self, name: &str, task: F) -> StepResult
    where
        F: FnOnce(tokio_watch::Receiver<bool>) -> Fut + Send + 'static,
        Fut: Future<Output = StepResult> + Send + 'static,
    {
        if self.background_tasks.tasks.contains_key(name) {
            return Err(StepError::LogicalError {
                message: format!("background task `{name}` is already registered"),
            });
        }

        let (cancellation, receiver) = tokio_watch::channel(false);
        let status = Arc::new(Mutex::new(BackgroundTaskStatus::Running));
        let task_status = Arc::clone(&status);
        let task_name = name.to_owned();
        let join = tokio::spawn(async move {
            let result = AssertUnwindSafe(task(receiver)).catch_unwind().await;
            let outcome = match result {
                Ok(Ok(())) => BackgroundTaskStatus::Stopped,
                Ok(Err(error)) => BackgroundTaskStatus::Failed(error.to_string()),
                Err(_) => BackgroundTaskStatus::Failed("task panicked".to_owned()),
            };
            if let Ok(mut status) = task_status.lock() {
                *status = outcome;
            } else {
                warn!(target: TARGET, task = %task_name, "Background task status lock was poisoned");
            }
        });

        self.background_tasks.tasks.insert(
            name.to_owned(),
            BackgroundTaskHandle {
                cancellation,
                join,
                status,
            },
        );
        Ok(())
    }

    pub(super) fn ensure_background_task_healthy(&self, name: &str) -> StepResult {
        let task =
            self.background_tasks
                .tasks
                .get(name)
                .ok_or_else(|| StepError::LogicalError {
                    message: format!("background task `{name}` is not running"),
                })?;
        let status = task.status.lock().map_err(|_| StepError::LogicalError {
            message: format!("background task `{name}` status lock was poisoned"),
        })?;
        match &*status {
            BackgroundTaskStatus::Running => Ok(()),
            BackgroundTaskStatus::Failed(error) => Err(StepError::StepFail {
                message: format!("background task `{name}` failed: {error}"),
            }),
            BackgroundTaskStatus::Stopped => Err(StepError::StepFail {
                message: format!("background task `{name}` stopped unexpectedly"),
            }),
        }
    }

    pub(super) fn background_task_status(&self, name: &str) -> Result<String, StepError> {
        let task =
            self.background_tasks
                .tasks
                .get(name)
                .ok_or_else(|| StepError::LogicalError {
                    message: format!("background task `{name}` is not registered"),
                })?;
        let status = task.status.lock().map_err(|_| StepError::LogicalError {
            message: format!("background task `{name}` status lock was poisoned"),
        })?;
        Ok(match &*status {
            BackgroundTaskStatus::Running => "Running".to_owned(),
            BackgroundTaskStatus::Failed(error) => format!("Failed({error})"),
            BackgroundTaskStatus::Stopped => "Stopped".to_owned(),
        })
    }

    pub(super) fn ensure_background_tasks_healthy(&self) -> StepResult {
        for (name, task) in &self.background_tasks.tasks {
            let status = task.status.lock().map_err(|_| StepError::LogicalError {
                message: format!("background task `{name}` status lock was poisoned"),
            })?;
            match &*status {
                BackgroundTaskStatus::Running => {}
                BackgroundTaskStatus::Failed(error) => {
                    return Err(StepError::StepFail {
                        message: format!("background task `{name}` failed: {error}"),
                    });
                }
                BackgroundTaskStatus::Stopped => {
                    return Err(StepError::StepFail {
                        message: format!("background task `{name}` stopped unexpectedly"),
                    });
                }
            }
        }
        Ok(())
    }

    pub(super) async fn stop_background_task(&mut self, name: &str) -> StepResult {
        let task =
            self.background_tasks
                .tasks
                .remove(name)
                .ok_or_else(|| StepError::LogicalError {
                    message: format!("background task `{name}` is not registered"),
                })?;
        let is_transaction_load = name == CONTINUOUS_NEXT_WALLET_LOAD_TASK;
        let progress = is_transaction_load
            .then(|| self.continuous_transaction_load_progress.clone())
            .flatten();
        let event_logger =
            is_transaction_load.then(|| BlendDiagnosticEventLogger::from_world(self));
        let task_status = Arc::clone(&task.status);
        if let (Some(progress), Some(event_logger)) = (&progress, &event_logger) {
            let status = task_status.lock().map_or_else(
                |_| "Unavailable(status lock poisoned)".to_owned(),
                |status| format!("{status:?}"),
            );
            log_continuous_transaction_load_stop_event(
                event_logger,
                "continuous_transaction_load_stop_requested",
                progress,
                &status,
                None,
            );
        }

        let result = stop_background_task(name, task).await;
        if let (Some(progress), Some(event_logger)) = (progress, event_logger) {
            let status = task_status.lock().map_or_else(
                |_| "Unavailable(status lock poisoned)".to_owned(),
                |status| format!("{status:?}"),
            );
            log_continuous_transaction_load_stop_event(
                &event_logger,
                "continuous_transaction_load_stopped",
                &progress,
                &status,
                result.as_ref().err().map(ToString::to_string).as_deref(),
            );
        }
        result
    }

    pub(super) async fn stop_all_background_tasks(&mut self) -> StepResult {
        let mut task_names = self
            .background_tasks
            .tasks
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        task_names.sort();

        let mut errors = Vec::new();
        for name in task_names {
            if let Err(error) = self.stop_background_task(&name).await {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(StepError::StepFail {
                message: errors.join("; "),
            })
        }
    }
}

#[cfg(test)]
mod background_task_tests {
    use std::fs;

    use super::{
        CONTINUOUS_NEXT_WALLET_LOAD_TASK, ContinuousTransactionLoadProgress, CucumberWorld,
    };
    use crate::cucumber::error::StepError;

    #[tokio::test]
    async fn reports_background_task_failure_to_health_check_and_join() {
        let mut world = CucumberWorld::default();
        world
            .spawn_background_task("failing task", async move |_cancellation| {
                Err(StepError::StepFail {
                    message: "producer failed".to_owned(),
                })
            })
            .expect("task should start");

        tokio::task::yield_now().await;

        let health = world.ensure_background_task_healthy("failing task");
        assert!(matches!(
            health,
            Err(StepError::StepFail { message }) if message.contains("producer failed")
        ));

        let joined = world.stop_background_task("failing task").await;
        assert!(matches!(
            joined,
            Err(StepError::StepFail { message }) if message.contains("producer failed")
        ));
    }

    #[tokio::test]
    async fn abnormal_cleanup_logs_continuous_load_stop_markers_with_final_progress() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be created");
        let mut world = CucumberWorld::default();
        world.lifecycle.scenario_base_dir = temp_dir.path().to_owned();
        world.set_scenario_name("abnormal background load cleanup");

        let progress = ContinuousTransactionLoadProgress::new(8);
        progress.record_completed_round();
        progress.record_completed_round();
        world.continuous_transaction_load_progress = Some(progress);
        world
            .spawn_background_task(
                CONTINUOUS_NEXT_WALLET_LOAD_TASK,
                async move |mut cancellation| {
                    cancellation
                        .changed()
                        .await
                        .expect("cleanup should signal cancellation");
                    Ok(())
                },
            )
            .expect("load task should start");

        world
            .stop_background_activity()
            .await
            .expect("abnormal cleanup should stop the task successfully");

        let timeline = fs::read_to_string(temp_dir.path().join("blend_diagnostic_timeline.ndjson"))
            .expect("cleanup timeline should be readable");
        let records = timeline
            .lines()
            .filter(|line| !line.is_empty())
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<Result<Vec<_>, _>>()
            .expect("timeline records should be valid JSON");
        let stop_requested = records
            .iter()
            .find(|record| record["event"] == "continuous_transaction_load_stop_requested")
            .expect("cleanup should log a stop request");
        let stopped = records
            .iter()
            .find(|record| record["event"] == "continuous_transaction_load_stopped")
            .expect("cleanup should log task completion");

        assert_eq!(stop_requested["task_status"], "Running");
        assert_eq!(stopped["task_status"], "Stopped");
        assert_eq!(stopped["completed_rounds"], 2);
        assert_eq!(stopped["completed_verified_transactions"], 16);
        assert_eq!(stopped["transactions_per_round"], 8);
    }
}

#[cfg(test)]
mod continuous_transaction_load_progress_tests {
    use super::ContinuousTransactionLoadProgress;

    #[test]
    fn checkpoints_report_completed_rounds_and_verified_transaction_deltas() {
        let progress = ContinuousTransactionLoadProgress::new(80);
        assert_eq!(progress.snapshot(), (0, 0));
        let empty_checkpoint = progress.checkpoint();
        assert_eq!(empty_checkpoint.completed_rounds, 0);
        assert_eq!(empty_checkpoint.completed_verified_transactions, 0);
        assert_eq!(empty_checkpoint.rounds_since_previous_check, 0);
        assert_eq!(
            empty_checkpoint.verified_transactions_since_previous_check,
            0
        );

        progress.record_completed_round();
        assert_eq!(progress.snapshot(), (1, 80));
        let first_round = progress.checkpoint();
        assert_eq!(first_round.completed_rounds, 1);
        assert_eq!(first_round.completed_verified_transactions, 80);
        assert_eq!(first_round.rounds_since_previous_check, 1);
        assert_eq!(first_round.verified_transactions_since_previous_check, 80);

        progress.record_completed_round();
        progress.record_completed_round();
        assert_eq!(progress.snapshot(), (3, 240));
        let later_rounds = progress.checkpoint();
        assert_eq!(later_rounds.completed_rounds, 3);
        assert_eq!(later_rounds.completed_verified_transactions, 240);
        assert_eq!(later_rounds.rounds_since_previous_check, 2);
        assert_eq!(later_rounds.verified_transactions_since_previous_check, 160);

        let stalled_checkpoint = progress.checkpoint();
        assert_eq!(stalled_checkpoint.rounds_since_previous_check, 0);
        assert_eq!(
            stalled_checkpoint.verified_transactions_since_previous_check,
            0
        );
    }
}
