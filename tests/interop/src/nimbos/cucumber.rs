use std::{env, path::PathBuf};

use async_trait::async_trait;
use lb_testing_framework::SharedDeployment;
use logos_blockchain_tests::cucumber::deployment::{
    CucumberClusterApp, ExternalDeploymentFactory, LocalDeployment, LocalImplementation,
    runtime_info::{NodeRuntimeInfo, NodeRuntimeInfoProvider},
};
use testing_framework_app::{AppDeployer, ClusterApp};
use testing_framework_core::scenario::{ClusterStartMode, DynError};

use super::{NimbosConfig, NimbosEnv};

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

    async fn deploy(&self, inputs: SharedDeployment) -> Result<LocalDeployment, DynError> {
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

impl NodeRuntimeInfoProvider for NimbosConfig {
    fn runtime_info(&self) -> Result<NodeRuntimeInfo, DynError> {
        Ok(NodeRuntimeInfo {
            peer_id: self.network_key.public().to_peer_id(),
            slots_per_epoch: self.deployment.slots_per_epoch,
            // Node-local wallets are not provisioned by this adapter. Scenario
            // wallets use the accounts and funding in the shared genesis.
            wallets: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env::current_exe,
        num::{NonZeroU64, NonZeroUsize},
        path::Path,
    };

    use libp2p::identity::Keypair;
    use tempfile::tempdir;
    use testing_framework_core::scenario::StartNodeOptions;
    use testing_framework_runner_local::{
        LocalBuildContext, LocalDeployerEnv as _, allocate_local_node_ports,
    };

    use super::*;
    use crate::nimbos::{NETWORK_KEY_FILE, NimbosDeployment};

    const DEPLOYMENT: &str = "\
cryptarchia:
  epoch_config:
    epoch_stake_distribution_stabilization: 1
    epoch_period_nonce_buffer: 2
    epoch_period_nonce_stabilization: 3
  security_param: 3
  slot_activation_coeff:
    numerator: 4
    denominator: 5
";

    #[tokio::test]
    async fn runtime_info_matches_each_nodes_launch_inputs() {
        let dir = tempdir().unwrap();
        let deployment = NimbosDeployment::from_deployment_yaml(
            &current_exe().unwrap(),
            dir.path(),
            DEPLOYMENT.to_owned(),
            NonZeroU64::new(18).unwrap(),
        )
        .unwrap()
        .with_node_count(NonZeroUsize::new(2).unwrap());
        let mut ports = allocate_local_node_ports(2, &[], "nimbos-info-test").unwrap();
        let options = StartNodeOptions::default();
        let mut identities = Vec::new();

        for (index, ports) in ports.iter_mut().enumerate() {
            let node = NimbosEnv::build_node_config(LocalBuildContext {
                topology: &deployment,
                index,
                ports,
                peers: &[],
                options: &options,
                template_config: None,
            })
            .unwrap();
            let info = node.config.runtime_info().unwrap();
            let launch = NimbosEnv::build_launch_spec(&node.config, dir.path(), &node.name)
                .await
                .unwrap();
            let key_file = launch
                .files
                .iter()
                .find(|file| file.relative_path == Path::new(NETWORK_KEY_FILE))
                .unwrap();
            let launched_key = Keypair::from_protobuf_encoding(&key_file.contents).unwrap();

            assert_eq!(info.peer_id, launched_key.public().to_peer_id());
            assert_eq!(info.peer_id, deployment.peer_id(index).unwrap());
            assert_eq!(info.slots_per_epoch.get(), 18);
            assert!(info.wallets.is_empty());
            identities.push(info.peer_id);
        }

        assert_ne!(identities[0], identities[1]);
    }
}
