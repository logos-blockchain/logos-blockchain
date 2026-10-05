use std::num::NonZero;

use lb_libp2p::{
    PeerId,
    identity::{Keypair, ed25519},
};
use lb_node::config::RunConfig;
use testing_framework_core::scenario::{Application, ClusterHandle, DynError};

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
/// Each implementation supplies these values for the node that was prepared.
/// They must reflect any start-time overrides.
#[derive(Clone)]
pub struct NodeRuntimeInfo {
    pub peer_id: PeerId,
    pub slots_per_epoch: NonZero<u64>,
    pub wallets: Vec<NodeWalletKey>,
}

/// Provides shared information from a node's prepared settings.
///
/// Implementations can reuse retained inputs or read their final native config;
/// the information must describe the settings actually used to launch the node.
pub trait NodeRuntimeInfoProvider {
    fn runtime_info(&self) -> Result<NodeRuntimeInfo, DynError>;
}

impl NodeRuntimeInfoProvider for RunConfig {
    fn runtime_info(&self) -> Result<NodeRuntimeInfo, DynError> {
        let config = self;
        let key = Keypair::from(ed25519::Keypair::from(
            config.user.network.backend.swarm.node_key.clone(),
        ));

        Ok(NodeRuntimeInfo {
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

/// Implementation-independent access to a managed node's effective settings.
pub trait NodeRuntimeInfoSource: Send + Sync {
    fn read(&self, name: &str) -> Result<NodeRuntimeInfo, DynError>;
}

impl<E: Application> NodeRuntimeInfoSource for ClusterHandle<E>
where
    E::NodeConfig: NodeRuntimeInfoProvider,
{
    fn read(&self, name: &str) -> Result<NodeRuntimeInfo, DynError> {
        self.node_config(name)?.runtime_info()
    }
}
