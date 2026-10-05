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
