mod implementation;
pub mod runtime_info;

use std::sync::Arc;

use async_trait::async_trait;
pub use implementation::{DeploymentInput, ExternalDeploymentFactory, LocalImplementation};
use lb_testing_framework::SharedDeployment;
use testing_framework_app::{AppDeployment, AppHostEnv, DeployContext, DeployedApp};
use testing_framework_core::scenario::{ClusterHandle, DynError, NodeControl};

use self::runtime_info::{NodeRuntimeInfoProvider, NodeRuntimeInfoSource};

pub type LocalDeployment = DeployedApp<Arc<dyn NodeControl>>;

/// Keeps the typed handle in TF's registry while exposing ordinary lifecycle
/// control to the concrete Cucumber world.
pub struct CucumberClusterApp<A> {
    pub app: A,
    pub inputs: SharedDeployment,
}

#[async_trait]
impl<A, E> AppDeployment<AppHostEnv> for CucumberClusterApp<A>
where
    E: NodeRuntimeInfoProvider,
    A: AppDeployment<AppHostEnv, Handle = ClusterHandle<E>>,
{
    type Handle = Arc<dyn NodeControl>;

    async fn deploy(self, ctx: &mut DeployContext<AppHostEnv>) -> Result<Self::Handle, DynError> {
        ctx.expose(self.inputs)?;
        let cluster = ctx.deploy(self.app).await?;
        if let Some(readiness) = cluster.cluster_wait() {
            ctx.expose(readiness)?;
        }

        let control = cluster
            .control()
            .ok_or("Cucumber requires node lifecycle control")?;
        let node_info: Arc<dyn NodeRuntimeInfoSource> = Arc::new(cluster.clone());
        ctx.expose(node_info)?;
        ctx.expose(cluster)?;
        Ok(control)
    }
}
