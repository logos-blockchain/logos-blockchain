pub mod runtime_info;

use std::sync::Arc;

use async_trait::async_trait;
use lb_testing_framework::SharedDeployment;
use testing_framework_app::{AppDeployment, AppHostEnv, DeployContext, DeployedApp};
use testing_framework_core::scenario::{Application, ClusterHandle, DynError, NodeControl};

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
    E: Application,
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
        ctx.expose(cluster)?;
        Ok(control)
    }
}
