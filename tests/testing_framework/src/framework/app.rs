//! Logos blockchain cluster deployed through the unified app-composition
//! model.

use std::sync::Arc;

use async_trait::async_trait;
use testing_framework_app::{AppDeployment, AppHostEnv, DeployContext};
use testing_framework_core::{
    observation::{ObservationRuntime, StaticSourceProvider},
    scenario::{
        CleanupGuard, ClusterControlRequest, ClusterHandle, ClusterProvisioner, ClusterRequest,
        ClusterStartMode, DynError, ExternalNodeSource, ObservabilityInputs,
    },
    topology::DeploymentProvider,
};
use tokio::task::JoinHandle;

/// Environment marker for Logos blockchain scenarios.
pub use super::LbcEnv;
use super::{
    apply_wallet_config_to_deployment,
    block_feed::{BlockFeed, BlockFeedObserver, block_feed_sources},
};
use crate::node::{DeploymentPlan, configs::wallet::WalletConfig};

/// Logos blockchain cluster application.
///
/// Deploys the node cluster through the backend cluster provisioner selected
/// by the scenario and layers the shared block feed on top of the resulting
/// node clients. The cluster is exposed as [`ClusterHandle<LbcEnv>`]; the feed
/// is exposed as [`BlockFeed`].
#[derive(Clone)]
pub struct LbcClusterApp {
    provider: Arc<dyn DeploymentProvider<DeploymentPlan>>,
    wallet: Option<WalletConfig>,
    external_nodes: Vec<ExternalNodeSource>,
    external_only: bool,
    block_feed: bool,
    start_mode: ClusterStartMode,
    observability: Option<ObservabilityInputs>,
}

impl LbcClusterApp {
    #[must_use]
    pub fn new(provider: Box<dyn DeploymentProvider<DeploymentPlan>>) -> Self {
        Self {
            provider: Arc::from(provider),
            wallet: None,
            external_nodes: Vec::new(),
            external_only: false,
            block_feed: false,
            start_mode: ClusterStartMode::Eager,
            observability: None,
        }
    }

    #[must_use]
    pub fn with_wallet_config(mut self, wallet: WalletConfig) -> Self {
        self.wallet = Some(wallet);
        self
    }

    #[must_use]
    pub fn with_external_node(mut self, node: ExternalNodeSource) -> Self {
        self.external_nodes.push(node);
        self
    }

    #[must_use]
    pub const fn with_external_only(mut self) -> Self {
        self.external_only = true;
        self
    }

    #[must_use]
    pub const fn with_block_feed(mut self) -> Self {
        self.block_feed = true;
        self
    }

    #[must_use]
    pub fn with_observability(mut self, observability: ObservabilityInputs) -> Self {
        self.observability = Some(observability);
        self
    }

    #[must_use]
    pub const fn with_on_demand_start(mut self) -> Self {
        self.start_mode = ClusterStartMode::OnDemand;
        self
    }

    fn into_request(self) -> Result<(ClusterRequest<LbcEnv>, bool), DynError> {
        let Self {
            provider,
            wallet,
            external_nodes,
            external_only,
            block_feed,
            start_mode,
            observability,
        } = self;

        let mut request = if external_only {
            ClusterRequest::external(external_nodes)
        } else {
            let mut deployment = provider.build(None)?;
            if let Some(wallet) = wallet {
                apply_wallet_config_to_deployment(&mut deployment, &wallet);
            }

            ClusterRequest::managed(deployment)
                .with_external_nodes(external_nodes)
                .with_control(ClusterControlRequest::Full)
                .with_start_mode(start_mode)
        };

        if let Some(observability) = observability {
            request = request.with_observability(observability);
        }

        Ok((request, block_feed))
    }
}

#[async_trait]
impl<P> AppDeployment<AppHostEnv, P> for LbcClusterApp
where
    P: ClusterProvisioner<LbcEnv>,
{
    type Handle = ClusterHandle<LbcEnv>;

    async fn deploy(
        self,
        ctx: &mut DeployContext<AppHostEnv, P>,
    ) -> Result<Self::Handle, DynError> {
        let (request, block_feed) = self.into_request()?;
        let handle = ctx.deploy_cluster(request).await?;

        if block_feed {
            let feed = start_block_feed(ctx, &handle).await?;
            ctx.expose(feed)?;
        }

        Ok(handle)
    }
}

async fn start_block_feed<P>(
    ctx: &mut DeployContext<AppHostEnv, P>,
    handle: &ClusterHandle<LbcEnv>,
) -> Result<BlockFeed, DynError>
where
    P: Send + Sync + 'static,
{
    let provider = Box::new(StaticSourceProvider::new(block_feed_sources(
        handle.clients(),
    )));
    let runtime =
        ObservationRuntime::start(provider, BlockFeedObserver, BlockFeedObserver::config()).await?;
    let (feed_handle, task) = runtime.into_parts();

    ctx.defer_cleanup(Box::new(AbortTaskGuard { task }));

    Ok(BlockFeed::new(feed_handle))
}

struct AbortTaskGuard {
    task: JoinHandle<()>,
}

impl CleanupGuard for AbortTaskGuard {
    fn cleanup(self: Box<Self>) {
        self.task.abort();
    }
}
