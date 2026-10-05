use std::pin::Pin;

use lb_testing_framework::{LbcClusterApp, SharedDeployment, internal::DeploymentPlan};
use testing_framework_app::AppDeployer;
use testing_framework_core::{scenario::DynError, topology::FixedDeploymentProvider};

use super::{CucumberClusterApp, LocalDeployment, runtime_info::NodeRuntimeInfo};
use crate::cucumber::error::StepError;

pub type DeploymentFuture = Pin<Box<dyn Future<Output = Result<LocalDeployment, DynError>> + Send>>;

/// Receives prepared network inputs and returns a TF-owned local deployment.
/// External runners provide this function; the suite does not select adapters.
pub type DeploymentFactory = fn(SharedDeployment, Vec<NodeRuntimeInfo>) -> DeploymentFuture;

/// The default Logos deployment or a factory supplied by an integration runner.
/// Shared steps use TF control in either case.
#[derive(Clone, Copy, Debug, Default)]
pub enum LocalImplementation {
    #[default]
    Logos,
    External {
        name: &'static str,
        deploy: DeploymentFactory,
    },
}

impl LocalImplementation {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Logos => "logos",
            Self::External { name, .. } => name,
        }
    }

    #[must_use]
    pub const fn is_logos(self) -> bool {
        matches!(self, Self::Logos)
    }

    pub fn require_logos(self, operation: &str) -> Result<(), StepError> {
        if self.is_logos() {
            return Ok(());
        }
        Err(StepError::InvalidArgument {
            message: format!("{operation} currently requires the Logos adapter"),
        })
    }

    pub async fn deploy(self, deployment: DeploymentPlan) -> Result<LocalDeployment, StepError> {
        let inputs = SharedDeployment::from_plan(&deployment)?;
        let node_runtime_info = NodeRuntimeInfo::from_deployment(&deployment)?;

        match self {
            Self::Logos => {
                let app = LbcClusterApp::new(Box::new(FixedDeploymentProvider::new(deployment)))
                    .with_on_demand_start();

                Ok(AppDeployer::new()
                    .deploy(CucumberClusterApp {
                        app,
                        inputs,
                        node_runtime_info,
                    })
                    .await?)
            }
            Self::External { deploy, .. } => Ok(deploy(inputs, node_runtime_info).await?),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use async_trait::async_trait;
    use lb_testing_framework::{DeploymentBuilder, TopologyConfig};
    use testing_framework_app::{AppDeployment, AppHostEnv, DeployContext};
    use testing_framework_core::scenario::NodeControl;

    use super::*;
    use crate::cucumber::world::ClusterState;

    struct TestApp {
        inputs: SharedDeployment,
        nodes: Vec<NodeRuntimeInfo>,
    }

    #[derive(Default)]
    struct TestControl {
        stopped: AtomicBool,
    }

    #[async_trait]
    impl NodeControl for TestControl {
        fn node_names(&self) -> Vec<String> {
            vec!["external-node".to_owned()]
        }

        async fn stop_node(&self, name: &str) -> Result<(), DynError> {
            assert_eq!(name, "external-node");
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait]
    impl AppDeployment<AppHostEnv> for TestApp {
        type Handle = Arc<dyn NodeControl>;

        async fn deploy(
            self,
            ctx: &mut DeployContext<AppHostEnv>,
        ) -> Result<Self::Handle, DynError> {
            ctx.expose(self.inputs)?;
            ctx.expose(self.nodes)?;

            let control = Arc::new(TestControl::default());
            ctx.expose(Arc::clone(&control))?;
            Ok(control)
        }
    }

    #[tokio::test]
    async fn external_factory_installs_shared_inputs_and_common_control() {
        let plan =
            DeploymentBuilder::new(TopologyConfig::with_node_numbers(1).with_blend_core_nodes(0))
                .build()
                .unwrap();
        let expected = SharedDeployment::from_plan(&plan).unwrap();
        let expected_node = NodeRuntimeInfo::from_deployment(&plan).unwrap().remove(0);
        let implementation = LocalImplementation::External {
            name: "test",
            deploy: |inputs, nodes| {
                Box::pin(async move { AppDeployer::new().deploy(TestApp { inputs, nodes }).await })
            },
        };

        let app = implementation.deploy(plan).await.unwrap();
        let inputs = app.runtime().get::<SharedDeployment>().unwrap();
        assert_eq!(inputs.node_count(), 1);
        assert_eq!(
            inputs.deployment_yaml().unwrap(),
            expected.deployment_yaml().unwrap()
        );
        assert_eq!(
            inputs.network_key(0).unwrap().public(),
            expected.network_key(0).unwrap().public()
        );
        let control = app.runtime().get::<Arc<TestControl>>().unwrap();

        let mut cluster = ClusterState::default();
        cluster.install_local(app).unwrap();
        assert!(cluster.logos_cluster().is_none());
        assert_eq!(cluster.node_runtime_info.len(), 1);
        assert_eq!(cluster.node_runtime_info[0].peer_id, expected_node.peer_id);
        assert_eq!(
            cluster.node_runtime_info[0].slots_per_epoch,
            expected_node.slots_per_epoch
        );

        let common_control = cluster.local_control().unwrap();
        assert_eq!(common_control.node_names(), ["external-node"]);
        common_control.stop_node("external-node").await.unwrap();
        assert!(control.stopped.load(Ordering::SeqCst));
    }
}
