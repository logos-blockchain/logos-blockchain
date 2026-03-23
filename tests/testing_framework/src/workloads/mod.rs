pub mod consensus_liveness;
pub mod fork_monitor;
pub mod inscription;
pub mod transaction;

use std::sync::Arc;

pub use consensus_liveness::ConsensusLiveness;
pub use fork_monitor::ClusterForkMonitor;
pub use inscription::*;
use testing_framework_core::scenario::{Application, RunContext};
use tokio::sync::broadcast;

use crate::{BlockFeed, BlockRecord, NodeHttpClient, framework::LbcEnv, node::DeploymentPlan};

pub type BlockFeedSubscription = broadcast::Receiver<Arc<BlockRecord>>;

/// Common environment bounds required by Nomos-specific workloads.
pub trait LbcScenarioEnv:
    Application<Deployment = DeploymentPlan, NodeClient = NodeHttpClient>
{
}

impl LbcScenarioEnv for LbcEnv {}

/// Extension trait for environments that expose block feed views.
pub trait LbcBlockFeedEnv: LbcScenarioEnv + Sized {
    fn block_feed_subscription(ctx: &RunContext<Self>) -> BlockFeedSubscription;

    fn block_feed(ctx: &RunContext<Self>) -> BlockFeed;
}

impl LbcBlockFeedEnv for LbcEnv {
    fn block_feed_subscription(ctx: &RunContext<Self>) -> BlockFeedSubscription {
        ctx.feed().subscribe()
    }

    fn block_feed(ctx: &RunContext<Self>) -> BlockFeed {
        ctx.feed()
    }
}
