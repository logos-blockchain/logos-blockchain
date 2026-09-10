use super::{
    CucumberWorld, Duration, Instant, MANUAL_COMMAND_FILE_ENV, MANUAL_COMMAND_POLL_INTERVAL_ENV,
    Path, StepError, TARGET, env, execute_manual_command, info, sleep, take_next_command,
};

#[expect(
    clippy::cognitive_complexity,
    reason = "Singular fn with multiple branches to handle different events and futures."
)]
pub async fn perform_manual_step_control(
    world: &mut CucumberWorld,
    step: &str,
    timeout_seconds: u64,
) -> Result<(), StepError> {
    let command_file =
        env::var(MANUAL_COMMAND_FILE_ENV).map_err(|_| StepError::InvalidArgument {
            message: format!(
                "Step `{step}` requires environment variable '{MANUAL_COMMAND_FILE_ENV}' to be set",
            ),
        })?;
    let poll_interval_ms = env::var(MANUAL_COMMAND_POLL_INTERVAL_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(300);

    info!(
        target: TARGET,
        "Manual control step started. Monitoring command file: `{command_file}`"
    );

    let time_out = Duration::from_secs(timeout_seconds);
    let start = Instant::now();
    while start.elapsed() < time_out {
        if let Some(command) = take_next_command(Path::new(&command_file))? {
            info!(target: TARGET, "====> manual command: {command:?}");
            if matches!(
                execute_manual_command(world, step, &command).await,
                Ok(true)
            ) {
                info!(
                    target: TARGET,
                   "Manual command loop stopped by STOP command after {:.2?}",
                   start.elapsed()
                );
                return Ok(());
            }
        } else {
            sleep(Duration::from_millis(poll_interval_ms)).await;
        }
    }
    info!(target: TARGET, "Manual command loop stopped by tine-out after {:.2?}", start.elapsed());

    Ok(())
}
