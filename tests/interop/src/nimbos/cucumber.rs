use std::{env, path::PathBuf};

use async_trait::async_trait;
use logos_blockchain_tests::cucumber::deployment::{
    CucumberClusterApp, DeploymentInput, ExternalDeploymentFactory, LocalDeployment,
    LocalImplementation,
};
use testing_framework_app::{AppDeployer, ClusterApp};
use testing_framework_core::scenario::{ClusterStartMode, DynError};

use super::NimbosEnv;

/// Select Nimbos while reusing the standard Cucumber suite and TF lifecycle.
#[must_use]
pub fn implementation() -> LocalImplementation {
    LocalImplementation::External(&NimbosFactory)
}

#[derive(Debug)]
struct NimbosFactory;

#[async_trait]
impl ExternalDeploymentFactory for NimbosFactory {
    fn name(&self) -> &'static str {
        "nimbos"
    }

    async fn deploy(&self, prepared: DeploymentInput) -> Result<LocalDeployment, DynError> {
        let inputs = prepared.shared_inputs()?;
        let binary = PathBuf::from(env::var("NIMBOS_NODE_BIN")?);
        let circuits = PathBuf::from(env::var("NIMBOS_CIRCUITS_DIR")?);
        let deployment = NimbosEnv::prepare_deployment(&inputs, &binary, &circuits)?;
        let app =
            ClusterApp::<NimbosEnv>::new(deployment).with_start_mode(ClusterStartMode::OnDemand);

        AppDeployer::new()
            .deploy(CucumberClusterApp { app, inputs })
            .await
    }
}
