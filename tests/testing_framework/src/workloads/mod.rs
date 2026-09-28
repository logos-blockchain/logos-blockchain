pub mod consensus_liveness;
pub mod fork_monitor;
pub mod inscription;
pub mod transaction;

use std::sync::Arc;

pub use consensus_liveness::ConsensusLiveness;
pub use fork_monitor::ClusterForkMonitor;
pub use inscription::*;
use testing_framework_app::{AppHostEnv, AppRunContextExt as _};
use testing_framework_core::scenario::{ClusterHandle, DynError, RunContext};
use tokio::sync::broadcast;

use crate::{BlockFeed, BlockRecord, NodeHttpClient, framework::LbcEnv, node::DeploymentPlan};

pub type BlockFeedSubscription = broadcast::Receiver<Arc<BlockRecord>>;

/// Access to the Logos cluster application from workload run contexts.
pub trait LbcRunContextExt {
    /// Returns the Logos cluster handle exposed by the cluster application.
    fn lbc_cluster(&self) -> Result<ClusterHandle<LbcEnv>, DynError>;

    /// Returns the deployment plan the managed cluster was provisioned from.
    fn lbc_deployment(&self) -> Result<DeploymentPlan, DynError>;

    /// Returns the current node clients of the Logos cluster.
    fn lbc_clients(&self) -> Result<Vec<NodeHttpClient>, DynError>;

    /// Returns the shared block feed exposed by the cluster application.
    fn block_feed(&self) -> Result<BlockFeed, DynError>;

    /// Subscribes to the shared block feed.
    fn block_feed_subscription(&self) -> Result<BlockFeedSubscription, DynError>;
}

impl LbcRunContextExt for RunContext<AppHostEnv> {
    fn lbc_cluster(&self) -> Result<ClusterHandle<LbcEnv>, DynError> {
        self.require_app::<ClusterHandle<LbcEnv>>()
    }

    fn lbc_deployment(&self) -> Result<DeploymentPlan, DynError> {
        self.lbc_cluster()?
            .deployment()
            .cloned()
            .ok_or_else(|| "logos cluster has no managed deployment plan".into())
    }

    fn lbc_clients(&self) -> Result<Vec<NodeHttpClient>, DynError> {
        Ok(self.lbc_cluster()?.clients())
    }

    fn block_feed(&self) -> Result<BlockFeed, DynError> {
        self.require_app::<BlockFeed>()
    }

    fn block_feed_subscription(&self) -> Result<BlockFeedSubscription, DynError> {
        self.block_feed().map(|feed| feed.subscribe())
    }
}
