use std::num::NonZero;

use lb_libp2p::PeerId;
use testing_framework_core::scenario::{Application, ClusterHandle, DynError};

/// A scenario wallet is either user-owned or backed by a node wallet key.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NodeWalletKeyRole {
    Funding,
    VoucherMaster,
    BlendZk,
    General,
}

impl NodeWalletKeyRole {
    #[must_use]
    pub const fn priority(self) -> u8 {
        match self {
            Self::Funding => 0,
            Self::VoucherMaster => 1,
            Self::BlendZk => 2,
            Self::General => 3,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodeWalletKey {
    pub wallet_pk: String,
    pub role: NodeWalletKeyRole,
}

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
pub trait NodeRuntimeInfoProvider: Application {
    fn runtime_info(config: &Self::NodeConfig) -> Result<NodeRuntimeInfo, DynError>;
}

/// Implementation-independent access to a managed node's effective settings.
pub trait NodeRuntimeInfoSource: Send + Sync {
    fn read(&self, name: &str) -> Result<NodeRuntimeInfo, DynError>;
}

impl<E: NodeRuntimeInfoProvider> NodeRuntimeInfoSource for ClusterHandle<E> {
    fn read(&self, name: &str) -> Result<NodeRuntimeInfo, DynError> {
        E::runtime_info(&self.node_config(name)?)
    }
}
