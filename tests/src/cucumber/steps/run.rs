use cucumber::{then, when};
use testing_framework_core::scenario::Deployer as _;
use testing_framework_runner_compose::ComposeDeployer;
use testing_framework_runner_local::LocalDeployer;

use crate::cucumber::{
    error::{StepError, StepResult},
    world::{CucumberWorld, DeployerKind},
};

#[when(expr = "run scenario")]
async fn run_scenario(world: &mut CucumberWorld) -> StepResult {
    let deployer = world.deployer.ok_or(StepError::MissingDeployer)?;
    world.run.result = Some(match deployer {
        DeployerKind::Local => {
            let mut scenario = world.build_local_scenario()?;
            let deployer = LocalDeployer::default();
            let result = async {
                let runner =
                    deployer
                        .deploy(&scenario)
                        .await
                        .map_err(|e| StepError::RunFailed {
                            message: format!("local deploy failed: {e}"),
                        })?;
                runner
                    .run(&mut scenario)
                    .await
                    .map_err(|e| StepError::RunFailed {
                        message: format!("scenario run failed: {e}"),
                    })?;
                Ok::<(), StepError>(())
            }
            .await;

            result.map_err(|e| e.to_string())
        }
        DeployerKind::Compose => {
            let mut scenario = world.build_compose_scenario()?;
            let deployer = ComposeDeployer::default().with_readiness(world.readiness_checks);
            let result = async {
                let runner =
                    deployer
                        .deploy(&scenario)
                        .await
                        .map_err(|e| StepError::RunFailed {
                            message: format!("compose deploy failed: {e}"),
                        })?;
                runner
                    .run(&mut scenario)
                    .await
                    .map_err(|e| StepError::RunFailed {
                        message: format!("scenario run failed: {e}"),
                    })?;
                Ok::<(), StepError>(())
            }
            .await;

            result.map_err(|e| e.to_string())
        }
    });

    Ok(())
}

#[then(expr = "scenario should succeed")]
fn scenario_should_succeed(world: &mut CucumberWorld) -> StepResult {
    match world.run.result.take() {
        Some(Ok(())) => Ok(()),
        Some(Err(message)) => Err(StepError::RunFailed { message }),
        None => Err(StepError::RunFailed {
            message: "scenario was not run".to_owned(),
        }),
    }
}
