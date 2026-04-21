use cucumber::{then, when};
use lb_testing_framework::{
    LbcEnv, LbcK8sDeployer, LbcLocalDeployer, run_with_failure_diagnostics,
};
use testing_framework_core::scenario::{Deployer, Scenario};

use crate::cucumber::{
    error::{StepError, StepResult},
    world::{CucumberWorld, DeployerKind},
};

#[when(expr = "run scenario")]
async fn run_scenario(world: &mut CucumberWorld) -> StepResult {
    let deployer = selected_deployer(world)?;
    let result = match deployer {
        DeployerKind::Local => run_local_scenario(world).await,
        DeployerKind::Compose => unsupported_compose_run(),
        DeployerKind::K8s => run_k8s_scenario(world).await,
    };

    world.run.result = Some(result.map_err(|error| error.to_string()));

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

fn selected_deployer(world: &CucumberWorld) -> Result<DeployerKind, StepError> {
    world.deployer.ok_or(StepError::MissingDeployer)
}

async fn run_local_scenario(world: &CucumberWorld) -> StepResult {
    let mut scenario = world.build_local_scenario()?;

    deploy_and_run(
        &LbcLocalDeployer::default(),
        &mut scenario,
        "local deploy failed",
    )
    .await
}

async fn run_k8s_scenario(world: &CucumberWorld) -> StepResult {
    let mut scenario = world.build_k8s_scenario()?;

    deploy_and_run(
        &LbcK8sDeployer::default(),
        &mut scenario,
        "k8s deploy failed",
    )
    .await
}

fn unsupported_compose_run() -> StepResult {
    Err(StepError::UnsupportedDeployer {
        value: "compose".to_owned(),
    })
}

async fn deploy_and_run<D>(
    deployer: &D,
    scenario: &mut Scenario<LbcEnv>,
    deploy_error_message: &'static str,
) -> StepResult
where
    D: Deployer<LbcEnv>,
    D::Error: std::fmt::Display,
{
    let runner = deployer
        .deploy(scenario)
        .await
        .map_err(|error| StepError::RunFailed {
            message: format!("{deploy_error_message}: {error}"),
        })?;

    run_with_failure_diagnostics(runner, scenario)
        .await
        .map_err(|error| StepError::RunFailed {
            message: format!("scenario run failed: {error}"),
        })?;

    Ok(())
}
