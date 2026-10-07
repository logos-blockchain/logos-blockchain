//! Exercise the public external-runner path with real Logos processes.
//!
//! The external factory reuses the existing Logos app. See
//! `tests/interop/README.md` for the networking scenario used in CI.

use std::process::ExitCode;

use async_trait::async_trait;
use logos_blockchain_tests::cucumber::{
    deployment::{
        ExternalDeploymentFactory, LocalDeployment, LocalImplementation, PreparedDeployment,
    },
    runner,
};
use testing_framework_app::AppDeployer;
use testing_framework_core::scenario::DynError;

#[tokio::main]
async fn main() -> ExitCode {
    // Run the existing features and steps, selected by the usual Cucumber CLI
    // filters. CI passes --name '^Two nodes connect at runtime$'. The factory
    // below supplies the deployment when a scenario sets up its cluster.
    runner::run(LocalImplementation::External(&ExternalLogosFactory)).await
}

#[derive(Debug)]
struct ExternalLogosFactory;

#[async_trait]
impl ExternalDeploymentFactory for ExternalLogosFactory {
    fn name(&self) -> &'static str {
        "external-logos-smoke"
    }

    async fn deploy(&self, deployment: PreparedDeployment) -> Result<LocalDeployment, DynError> {
        // Prepare an on-demand cluster from the scenario's original Logos plan.
        // Subsequent steps start the node processes, connect them, check peers
        // and stop them; those steps stay in the shared suite.
        AppDeployer::new()
            .deploy(deployment.into_logos_app()?)
            .await
    }
}
