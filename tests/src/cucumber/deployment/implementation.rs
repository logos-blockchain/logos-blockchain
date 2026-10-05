use std::fmt::Debug;

use async_trait::async_trait;
use lb_testing_framework::{LbcClusterApp, SharedDeployment, internal::DeploymentPlan};
use testing_framework_app::AppDeployer;
use testing_framework_core::{scenario::DynError, topology::FixedDeploymentProvider};

use super::{CucumberClusterApp, LocalDeployment};
use crate::cucumber::error::StepError;

/// Prepares an external implementation from the suite's shared network inputs.
#[async_trait]
pub trait ExternalDeploymentFactory: Debug + Send + Sync {
    fn name(&self) -> &'static str;

    async fn deploy(&self, inputs: SharedDeployment) -> Result<LocalDeployment, DynError>;
}

/// The default Logos deployment or a factory supplied by an integration runner.
/// Shared steps use TF control in either case.
#[derive(Clone, Copy, Debug, Default)]
pub enum LocalImplementation {
    #[default]
    Logos,
    External(&'static dyn ExternalDeploymentFactory),
}

impl LocalImplementation {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Logos => "logos",
            Self::External(factory) => factory.name(),
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

        match self {
            Self::Logos => {
                let app = LbcClusterApp::new(Box::new(FixedDeploymentProvider::new(deployment)))
                    .with_on_demand_start();

                Ok(AppDeployer::new()
                    .deploy(CucumberClusterApp { app, inputs })
                    .await?)
            }
            Self::External(factory) => Ok(factory.deploy(inputs).await?),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        num::NonZero,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use lb_libp2p::identity::Keypair;
    use lb_testing_framework::{DeploymentBuilder, TopologyConfig};
    use testing_framework_app::{AppDeployment, AppHostEnv, DeployContext};
    use testing_framework_core::{
        scenario::{
            Application, ClusterControlProfile, ClusterHandle, ClusterUnit, NodeAccess,
            NodeClients, NodeControl, NodeControlHandle, NodeLaunchOptions, StartedNodeAccess,
        },
        topology::ClusterTopology,
    };

    use super::*;
    use crate::cucumber::{
        deployment::runtime_info::{NodeRuntimeInfo, NodeRuntimeInfoProvider},
        world::ClusterState,
    };

    struct TestEnv;

    impl Application for TestEnv {
        type Deployment = ClusterTopology;
        type NodeConfig = NodeRuntimeInfo;
        type NodeClient = ();

        fn build_node_client(_access: &NodeAccess) -> Result<(), DynError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestControl {
        configs: Mutex<HashMap<String, NodeRuntimeInfo>>,
    }

    #[async_trait]
    impl NodeControl for TestControl {
        async fn start_node_with(
            &self,
            name: &str,
            _options: NodeLaunchOptions,
        ) -> Result<StartedNodeAccess, DynError> {
            let mut configs = self.configs.lock().unwrap();
            let index = u64::try_from(configs.len())?;
            configs.insert(
                name.to_owned(),
                NodeRuntimeInfo {
                    peer_id: Keypair::generate_ed25519().public().to_peer_id(),
                    slots_per_epoch: NonZero::new(index + 1).unwrap(),
                    wallets: Vec::new(),
                },
            );
            drop(configs);

            Ok(StartedNodeAccess {
                name: name.to_owned(),
                access: NodeAccess::new("127.0.0.1", 8080),
            })
        }

        async fn stop_node(&self, name: &str) -> Result<(), DynError> {
            self.configs
                .lock()
                .unwrap()
                .remove(name)
                .ok_or("missing test node")?;
            Ok(())
        }
    }

    #[async_trait]
    impl NodeControlHandle<TestEnv> for TestControl {
        fn node_config(&self, name: &str) -> Result<NodeRuntimeInfo, DynError> {
            self.configs
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or_else(|| "missing test node".into())
        }
    }

    struct TestApp;

    #[async_trait]
    impl AppDeployment<AppHostEnv> for TestApp {
        type Handle = ClusterHandle<TestEnv>;

        async fn deploy(
            self,
            _ctx: &mut DeployContext<AppHostEnv>,
        ) -> Result<Self::Handle, DynError> {
            Ok(ClusterUnit::new(
                Some(ClusterTopology::new(1)),
                NodeClients::default(),
                ClusterControlProfile::ManualControlled,
            )
            .with_node_control(Arc::new(TestControl::default()))
            .handle())
        }
    }

    impl NodeRuntimeInfoProvider for NodeRuntimeInfo {
        fn runtime_info(&self) -> Result<NodeRuntimeInfo, DynError> {
            Ok(self.clone())
        }
    }

    #[derive(Debug)]
    struct TestFactory;

    #[async_trait]
    impl ExternalDeploymentFactory for TestFactory {
        fn name(&self) -> &'static str {
            "test"
        }

        async fn deploy(&self, inputs: SharedDeployment) -> Result<LocalDeployment, DynError> {
            AppDeployer::new()
                .deploy(CucumberClusterApp {
                    app: TestApp,
                    inputs,
                })
                .await
        }
    }

    #[tokio::test]
    async fn external_factory_reads_runtime_info_for_nodes_beyond_initial_capacity() {
        let plan =
            DeploymentBuilder::new(TopologyConfig::with_node_numbers(1).with_blend_core_nodes(0))
                .build()
                .unwrap();
        let expected = SharedDeployment::from_plan(&plan).unwrap();
        let implementation = LocalImplementation::External(&TestFactory);

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

        let mut cluster = ClusterState::default();
        cluster.install_local(app).unwrap();
        assert!(cluster.logos_cluster().is_none());
        let reader = cluster.node_runtime_info.as_ref().unwrap();
        assert!(reader.read("second").is_err());

        let control = cluster.local_control().unwrap();
        control.start_node("first").await.unwrap();
        control.start_node("second").await.unwrap();
        let first = reader.read("first").unwrap();
        let second = reader.read("second").unwrap();
        assert_ne!(first.peer_id, second.peer_id);
        assert_eq!(first.slots_per_epoch.get(), 1);
        assert_eq!(second.slots_per_epoch.get(), 2);

        control.stop_node("second").await.unwrap();
        assert!(reader.read("second").is_err());
    }
}
