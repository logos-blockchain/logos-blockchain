use std::fmt::Debug;

use async_trait::async_trait;
use lb_testing_framework::{
    LbcClusterApp, SharedDeployment,
    configs::{PreparedDeployment, build_plan},
};
use testing_framework_app::AppDeployer;
use testing_framework_core::{scenario::DynError, topology::FixedDeploymentProvider};

use super::{CucumberClusterApp, LocalDeployment};
use crate::cucumber::error::StepError;

impl CucumberClusterApp<LbcClusterApp> {
    /// Renders Logos configuration from the scenario's prepared network and
    /// leaves process startup to the Cucumber lifecycle steps.
    pub fn from_logos(deployment: PreparedDeployment) -> Result<Self, DynError> {
        let inputs = deployment.shared_inputs()?;
        let plan = build_plan(deployment)?;
        let app =
            LbcClusterApp::new(Box::new(FixedDeploymentProvider::new(plan))).with_on_demand_start();

        Ok(Self { app, inputs })
    }
}

/// Prepared network input for the selected implementation.
/// Native rendering stays with the adapter.
pub struct DeploymentInput {
    deployment: PreparedDeployment,
}

impl DeploymentInput {
    pub fn shared_inputs(&self) -> Result<SharedDeployment, DynError> {
        self.deployment.shared_inputs()
    }

    pub async fn deploy_logos(self) -> Result<LocalDeployment, DynError> {
        AppDeployer::new()
            .deploy(CucumberClusterApp::from_logos(self.deployment)?)
            .await
    }
}

/// Deploys the scenario through an integration's selected apps.
#[async_trait]
pub trait ExternalDeploymentFactory: Debug + Send + Sync {
    fn name(&self) -> &'static str;

    async fn deploy(&self, deployment: DeploymentInput) -> Result<LocalDeployment, DynError>;
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

    pub async fn deploy(
        self,
        deployment: PreparedDeployment,
    ) -> Result<LocalDeployment, StepError> {
        let deployment = DeploymentInput { deployment };

        match self {
            Self::Logos => Ok(deployment.deploy_logos().await?),
            Self::External(factory) => Ok(factory.deploy(deployment).await?),
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
    use lb_testing_framework::{DeploymentBuilder, LbcEnv, TopologyConfig};
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

    impl NodeRuntimeInfoProvider for TestEnv {
        fn runtime_info(config: &NodeRuntimeInfo) -> Result<NodeRuntimeInfo, DynError> {
            Ok(config.clone())
        }
    }

    #[derive(Debug)]
    struct TestFactory;

    #[async_trait]
    impl ExternalDeploymentFactory for TestFactory {
        fn name(&self) -> &'static str {
            "test"
        }

        async fn deploy(&self, deployment: DeploymentInput) -> Result<LocalDeployment, DynError> {
            AppDeployer::new()
                .deploy(CucumberClusterApp {
                    app: TestApp,
                    inputs: deployment.shared_inputs()?,
                })
                .await
        }
    }

    #[derive(Debug)]
    struct ExternalLogosFactory;

    #[async_trait]
    impl ExternalDeploymentFactory for ExternalLogosFactory {
        fn name(&self) -> &'static str {
            "external-logos"
        }

        async fn deploy(&self, deployment: DeploymentInput) -> Result<LocalDeployment, DynError> {
            deployment.deploy_logos().await
        }
    }

    #[tokio::test]
    async fn external_logos_factory_does_not_expose_typed_logos_control() {
        for implementation in [
            LocalImplementation::Logos,
            LocalImplementation::External(&ExternalLogosFactory),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let deployment = DeploymentBuilder::new(
                TopologyConfig::with_node_numbers(1).with_blend_core_nodes(0),
            )
            .scenario_base_dir(dir.path().to_owned())
            .prepare()
            .unwrap();
            let app = implementation.deploy(deployment).await.unwrap();

            // Both deployments contain a real Logos handle. External selection
            // must still keep Cucumber on its implementation-independent path.
            assert!(app.runtime().get::<ClusterHandle<LbcEnv>>().is_some());

            let mut cluster = ClusterState {
                implementation,
                ..Default::default()
            };
            cluster.install_local(app).unwrap();

            assert_eq!(cluster.logos_cluster().is_some(), implementation.is_logos());
            assert!(cluster.local_control().is_ok());
            assert!(cluster.node_runtime_info.is_some());
        }
    }

    #[tokio::test]
    async fn external_factory_reads_runtime_info_for_nodes_beyond_initial_capacity() {
        let plan =
            DeploymentBuilder::new(TopologyConfig::with_node_numbers(1).with_blend_core_nodes(0))
                .prepare()
                .unwrap();
        let expected = plan.shared_inputs().unwrap();
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

        let mut cluster = ClusterState {
            implementation,
            ..Default::default()
        };
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
