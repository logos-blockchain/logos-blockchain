use std::{fmt::Display, time::Duration};

use lb_log_targets::utils;
use overwatch::services::status::{ServiceStatus, StatusWatcher, WaitPolicy};
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tracing::{error, info};

use crate::overwatch::status::ServiceStatusEntry;

const LOG_TARGET: &str = utils::SERVICE_READINESS;
const PROGRESS_INTERVAL: Duration = Duration::from_secs(60);

/// Wait for a service to become ready while logging periodic progress.
///
/// Overwatch's fail-fast waiter owns lifecycle and timeout handling. This
/// wrapper only reports progress and the result; it does not interpret status
/// transitions. Call it after the dependency's startup boundary is
/// acknowledged. Status watchers retain only the latest state, so intermediate
/// unobserved transitions cannot be guaranteed.
#[expect(
    clippy::cognitive_complexity,
    reason = "keep readiness logging and waiting cancellation-safe"
)]
pub async fn wait_for_service_to_be_ready<RuntimeServiceId>(
    mut status_watcher: StatusWatcher,
    service_id: RuntimeServiceId,
    timeout_duration: Option<Duration>,
) -> Result<(), ServiceStatusEntry<RuntimeServiceId>>
where
    RuntimeServiceId: Display + Send + Sync,
{
    let started_at = Instant::now();
    let progress_watcher = status_watcher.clone();
    info!(
        target: LOG_TARGET,
        dependency = %service_id,
        "Waiting for dependency readiness"
    );

    let readiness = status_watcher.wait_with_policy(
        ServiceStatus::Ready,
        timeout_duration,
        WaitPolicy::FailFast,
    );
    tokio::pin!(readiness);

    let mut progress = interval_at(started_at + PROGRESS_INTERVAL, PROGRESS_INTERVAL);
    progress.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            result = &mut readiness => {
                match result {
                    Ok(ServiceStatus::Ready) => {
                        info!(
                            target: LOG_TARGET,
                            dependency = %service_id,
                            elapsed_seconds = started_at.elapsed().as_secs_f64(),
                            "Dependency became ready"
                        );
                        return Ok(());
                    }
                    Ok(status) => {
                        error!(
                            target: LOG_TARGET,
                            dependency = %service_id,
                            status = %status,
                            elapsed_seconds = started_at.elapsed().as_secs_f64(),
                            "Fail-fast readiness returned a status other than Ready"
                        );
                        return Err(ServiceStatusEntry::from_overwatch(service_id, status));
                    }
                    Err(wait_error) => {
                        error!(
                            target: LOG_TARGET,
                            dependency = %service_id,
                            readiness_error = %wait_error,
                            elapsed_seconds = started_at.elapsed().as_secs_f64(),
                            timeout_seconds = ?timeout_duration.map(|duration| duration.as_secs_f64()),
                            "Dependency readiness failed"
                        );
                        return Err(ServiceStatusEntry::from_wait_error(service_id, wait_error));
                    }
                }
            }
            _ = progress.tick() => {
                info!(
                    target: LOG_TARGET,
                    dependency = %service_id,
                    status = %progress_watcher.current(),
                    elapsed_seconds = started_at.elapsed().as_secs_f64(),
                    "Dependency readiness is still pending"
                );
            }
        }
    }
}
