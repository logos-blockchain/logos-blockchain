use std::num::NonZero;

use lb_libp2p::{
    PeerId,
    identity::{Keypair, ed25519},
};
use lb_node::config::RunConfig;
use lb_testing_framework::{configs::build_node_run_config, internal::DeploymentPlan};

use crate::cucumber::{
    error::StepError, utils::node_wallet_keys_from_config, world::NodeWalletKey,
};

/// Node identity, wallets and epoch settings used by test steps.
///
/// Previously, startup helpers reopened the node's YAML files and deserialized
/// them into this checkout's Logos config types to obtain these values. An
/// older binary could accept its saved configuration while the test runner
/// failed to parse it. Another implementation's config would have the same
/// problem. Supplying these values separately lets steps use them without
/// parsing the node's native configuration.
///
/// Adapters supply these values from their prepared inputs. Logos config
/// overrides refresh them after the patch is applied.
#[derive(Clone)]
pub struct NodeRuntimeInfo {
    pub peer_id: PeerId,
    pub slots_per_epoch: NonZero<u64>,
    pub wallets: Vec<NodeWalletKey>,
}

impl NodeRuntimeInfo {
    pub fn from_deployment(deployment: &DeploymentPlan) -> Result<Vec<Self>, StepError> {
        deployment
            .nodes()
            .iter()
            .map(|node| {
                let config = build_node_run_config(
                    deployment,
                    node,
                    deployment.config().node_config_override(node.index()),
                )?;

                Self::from_config(&config)
            })
            .collect()
    }

    pub fn from_config(config: &RunConfig) -> Result<Self, StepError> {
        let key = Keypair::from(ed25519::Keypair::from(
            config.user.network.backend.swarm.node_key.clone(),
        ));

        Ok(Self {
            peer_id: key.public().to_peer_id(),
            slots_per_epoch: NonZero::new(
                config
                    .deployment
                    .genesis_era_parameters()
                    .cryptarchia
                    .slots_per_epoch(),
            )
            .ok_or_else(|| StepError::LogicalError {
                message: "deployment has zero slots per epoch".into(),
            })?,
            wallets: node_wallet_keys_from_config(&config.user)?,
        })
    }
}
