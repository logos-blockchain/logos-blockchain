use std::{env, path::PathBuf};

use lb_testing_framework::SharedDeployment;
use logos_blockchain_tests::cucumber::deployment::{
    CucumberClusterApp, LocalDeployment, LocalImplementation, runtime_info::NodeRuntimeInfo,
};
use testing_framework_app::{AppDeployer, ClusterApp};
use testing_framework_core::scenario::{ClusterStartMode, DynError};

use super::NimbosEnv;

/// Select Nimbos while reusing the standard Cucumber suite and TF lifecycle.
#[must_use]
pub fn implementation() -> LocalImplementation {
    LocalImplementation::External {
        name: "nimbos",
        deploy: |inputs, nodes| Box::pin(deploy(inputs, nodes)),
    }
}

async fn deploy(
    inputs: SharedDeployment,
    mut node_runtime_info: Vec<NodeRuntimeInfo>,
) -> Result<LocalDeployment, DynError> {
    let binary = PathBuf::from(env::var("NIMBOS_NODE_BIN")?);
    let circuits = PathBuf::from(env::var("NIMBOS_CIRCUITS_DIR")?);
    let deployment = NimbosEnv::prepare_deployment(
        inputs.deployment_yaml()?,
        inputs.node_count(),
        &binary,
        &circuits,
    )?;

    for (index, info) in node_runtime_info.iter_mut().enumerate() {
        info.peer_id = deployment.peer_id(index)?;
        // This adapter does not provision node-local wallets. Scenario wallets
        // still use the accounts and funding recorded in the shared genesis.
        info.wallets.clear();
    }

    let app = ClusterApp::<NimbosEnv>::new(deployment).with_start_mode(ClusterStartMode::OnDemand);

    AppDeployer::new()
        .deploy(CucumberClusterApp {
            app,
            inputs,
            node_runtime_info,
        })
        .await
}
