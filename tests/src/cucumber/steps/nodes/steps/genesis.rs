use super::{
    CucumberWorld, Duration, GenesisTime, Instant, NodeBinaryProfile, OffsetDateTime, Step,
    StepError, StepResult, TARGET, TimeDuration, ensure_node_binary_built, given, info,
    rebuild_pending_local_manual_cluster, sleep, then, when,
};

pub(super) fn resolve_step_genesis_time(
    step_value: &str,
    now: OffsetDateTime,
    seconds: i64,
) -> Result<GenesisTime, StepError> {
    if seconds < 0 {
        return Err(StepError::InvalidArgument {
            message: format!("step `{step_value}` requires a non-negative offset"),
        });
    }

    let genesis_datetime = now
        .checked_add(TimeDuration::seconds(seconds))
        .ok_or_else(|| StepError::InvalidArgument {
            message: format!(
                "step `{step_value}` has an invalid genesis time: offset is out of range"
            ),
        })?;

    GenesisTime::try_from(genesis_datetime).map_err(|error| StepError::InvalidArgument {
        message: format!("step `{step_value}` has an invalid genesis time: {error}"),
    })
}

pub(super) fn validate_genesis_time_change(
    existing_genesis_time: Option<GenesisTime>,
    nodes_started: bool,
    requested_genesis_time: GenesisTime,
) -> StepResult {
    if nodes_started && existing_genesis_time != Some(requested_genesis_time) {
        return Err(StepError::LogicalError {
            message: "cannot change genesis time after nodes have started".to_owned(),
        });
    }

    Ok(())
}

#[given(expr = "the chain starts {int} seconds from now")]
#[when(expr = "the chain starts {int} seconds from now")]
async fn step_chain_starts_from_now(
    world: &mut CucumberWorld,
    step: &Step,
    seconds: i64,
) -> StepResult {
    let node_binary_profile = if world.tokio_console_profile_enabled() {
        NodeBinaryProfile::TokioConsole
    } else {
        NodeBinaryProfile::default()
    };
    ensure_node_binary_built(&node_binary_profile)
        .await
        .map_err(|error| StepError::Preflight {
            message: format!("failed to resolve/build node binary: {error}"),
        })?;

    let genesis_time = resolve_step_genesis_time(&step.value, OffsetDateTime::now_utc(), seconds)?;
    validate_genesis_time_change(
        world.lifecycle.genesis_time,
        !world.nodes_info.is_empty(),
        genesis_time,
    )?;

    world.set_genesis_time(genesis_time);
    if world.nodes_info.is_empty() && world.cluster.manual_cluster_spec.is_some() {
        rebuild_pending_local_manual_cluster(world)?;
    }

    Ok(())
}

#[then(expr = "the configured genesis time has not passed for {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_genesis_time_has_not_passed(
    world: &mut CucumberWorld,
    step: &Step,
    seconds: u64,
) -> StepResult {
    let genesis_time = world
        .lifecycle
        .genesis_time
        .ok_or_else(|| StepError::LogicalError {
            message: "the scenario has no configured genesis time".to_owned(),
        })?;
    let genesis_datetime = OffsetDateTime::from(genesis_time);
    let timeout = Duration::from_secs(seconds);
    let started_waiting = Instant::now();

    loop {
        let now = OffsetDateTime::now_utc();
        if now >= genesis_datetime {
            return Err(StepError::StepFail {
                message: format!(
                    "Step `{}` failed: configured genesis time {genesis_datetime} had passed at {now}",
                    step.value
                ),
            });
        }

        if started_waiting.elapsed() >= timeout {
            info!(
                target: TARGET,
                "Configured genesis time {genesis_datetime} remained in the future for {seconds} seconds"
            );
            return Ok(());
        }

        sleep(Duration::from_millis(250)).await;
    }
}
