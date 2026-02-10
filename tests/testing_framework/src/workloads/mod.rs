pub mod consensus_liveness;
pub mod transaction;

use std::sync::Arc;

pub use consensus_liveness::ConsensusLiveness;
use testing_framework_core::scenario::{Application, RunContext};
use tokio::sync::broadcast;

use crate::{BlockRecord, NodeHttpClient, framework::LbcEnv, node::DeploymentPlan};

pub type BlockFeedSubscription = broadcast::Receiver<Arc<BlockRecord>>;

/// Common environment bounds required by Nomos-specific workloads.
pub trait LbcScenarioEnv:
    Application<Deployment = DeploymentPlan, NodeClient = NodeHttpClient>
{
}

impl LbcScenarioEnv for LbcEnv {}

/// Extension trait for environments that expose block feed subscriptions.
pub trait LbcBlockFeedEnv: LbcScenarioEnv + Sized {
    fn block_feed_subscription(ctx: &RunContext<Self>) -> BlockFeedSubscription;
}

impl LbcBlockFeedEnv for LbcEnv {
    fn block_feed_subscription(ctx: &RunContext<Self>) -> BlockFeedSubscription {
        ctx.feed().subscribe()
    }
}
