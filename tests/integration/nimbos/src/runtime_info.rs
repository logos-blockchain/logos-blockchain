use blockchain_test_support::runtime_info::{NodeRuntimeInfo, NodeRuntimeInfoProvider};
use testing_framework_core::scenario::DynError;

use crate::{NimbosConfig, NimbosEnv};

impl NodeRuntimeInfoProvider for NimbosEnv {
    fn runtime_info(config: &NimbosConfig) -> Result<NodeRuntimeInfo, DynError> {
        Ok(NodeRuntimeInfo {
            peer_id: config.network_key.public().to_peer_id(),
            slots_per_epoch: config.deployment.slots_per_epoch,
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
    use crate::{NETWORK_KEY_FILE, NimbosDeployment};

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
            let info = NimbosEnv::runtime_info(&node.config).unwrap();
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
