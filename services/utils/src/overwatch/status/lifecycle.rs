use std::{fmt::Display, time::Duration};

use lb_log_targets::utils;
use overwatch::services::status::{ServiceStatus, StatusWatcher};
use tokio::time::{Instant, MissedTickBehavior, interval_at, timeout};
use tracing::{error, info};

use crate::overwatch::status::ServiceStatusEntry;

const LOG_TARGET: &str = utils::SERVICE_READINESS;
const PROGRESS_INTERVAL: Duration = Duration::from_secs(60);

/// Wait for a service to become ready, or report a terminal lifecycle state.
///
/// Call this only after the dependency has passed an acknowledged startup
/// boundary. `StatusWatcher` exposes only the latest state, so an initial
/// `Stopped` status is treated as terminal just like any later `Stopped`.
///
/// `None` waits without a readiness deadline. `Some(duration)` preserves a
/// bounded wait and reports the status observed when the deadline expires.
/// Periodic progress logs do not terminate the wait. The future is polled
/// inline, so dropping its caller cancels the wait without leaving a detached
/// monitoring task.
pub async fn wait_for_service_to_be_ready<RuntimeServiceId>(
    mut status_watcher: StatusWatcher,
    service_id: RuntimeServiceId,
    timeout_duration: Option<Duration>,
) -> Result<(), ServiceStatusEntry<RuntimeServiceId>>
where
    RuntimeServiceId: Display + Send + Sync,
{
    let started_at = Instant::now();
    info!(
        target: LOG_TARGET,
        dependency = %service_id,
        "Waiting for dependency readiness"
    );

    let readiness = wait_for_readiness(&mut status_watcher, &service_id, started_at);
    let result = match timeout_duration {
        Some(timeout_duration) => timeout(timeout_duration, readiness)
            .await
            .unwrap_or_else(|_| {
                let status = status_watcher.current();
                match status {
                    ServiceStatus::Ready => Ok(()),
                    ServiceStatus::Starting => {
                        error!(
                            target: LOG_TARGET,
                            dependency = %service_id,
                            status = %status,
                            elapsed_seconds = started_at.elapsed().as_secs_f64(),
                            timeout_seconds = timeout_duration.as_secs_f64(),
                            "Timed out waiting for dependency readiness"
                        );
                        Err(status)
                    }
                    ServiceStatus::Stopped | ServiceStatus::Failed => {
                        error!(
                            target: LOG_TARGET,
                            dependency = %service_id,
                            status = %status,
                            elapsed_seconds = started_at.elapsed().as_secs_f64(),
                            timeout_seconds = timeout_duration.as_secs_f64(),
                            "Dependency reached a terminal state before readiness"
                        );
                        Err(status)
                    }
                }
            }),
        None => readiness.await,
    };

    result.map_err(|status| ServiceStatusEntry::from_overwatch(service_id, status))
}

#[expect(
    clippy::cognitive_complexity,
    reason = "The lifecycle states and notification/progress select form one readiness state machine."
)]
async fn wait_for_readiness<RuntimeServiceId>(
    status_watcher: &mut StatusWatcher,
    service_id: &RuntimeServiceId,
    started_at: Instant,
) -> Result<(), ServiceStatus>
where
    RuntimeServiceId: Display + Send + Sync,
{
    let mut progress = interval_at(started_at + PROGRESS_INTERVAL, PROGRESS_INTERVAL);
    progress.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        let status = status_watcher.current();
        match status {
            ServiceStatus::Ready => {
                info!(
                    target: LOG_TARGET,
                    dependency = %service_id,
                    elapsed_seconds = started_at.elapsed().as_secs_f64(),
                    "Dependency became ready"
                );
                return Ok(());
            }
            ServiceStatus::Starting => {}
            ServiceStatus::Stopped => {
                error!(
                    target: LOG_TARGET,
                    dependency = %service_id,
                    status = %status,
                    elapsed_seconds = started_at.elapsed().as_secs_f64(),
                    "Dependency stopped before becoming ready"
                );
                return Err(status);
            }
            ServiceStatus::Failed => {
                error!(
                    target: LOG_TARGET,
                    dependency = %service_id,
                    status = %status,
                    elapsed_seconds = started_at.elapsed().as_secs_f64(),
                    "Dependency failed before becoming ready"
                );
                return Err(status);
            }
        }

        tokio::select! {
            changed = status_watcher.receiver_mut().changed() => {
                if changed.is_err() {
                    let status = status_watcher.current();
                    error!(
                        target: LOG_TARGET,
                        dependency = %service_id,
                        status = %status,
                        elapsed_seconds = started_at.elapsed().as_secs_f64(),
                        "Dependency status notifications closed before readiness"
                    );
                    return Err(status);
                }
            }
            _ = progress.tick() => {
                info!(
                    target: LOG_TARGET,
                    dependency = %service_id,
                    status = %status,
                    elapsed_seconds = started_at.elapsed().as_secs_f64(),
                    "Dependency is still starting"
                );
            }
        }
    }
}
