use std::env;

use lb_testing_framework::{LbcClusterApp, SharedDeployment, internal::DeploymentPlan};
use testing_framework_app::AppDeployer;
use testing_framework_core::topology::FixedDeploymentProvider;

use super::{CucumberClusterApp, LocalDeployment, runtime_info::NodeRuntimeInfo};
use crate::cucumber::error::StepError;

/// Selects an adapter at the runner boundary; shared steps use TF control.
#[derive(Clone, Copy, Debug, Default)]
pub enum LocalImplementation {
    #[default]
    Logos,
}

impl LocalImplementation {
    pub fn from_env() -> Result<Self, StepError> {
        match env::var("CUCUMBER_IMPLEMENTATION").as_deref() {
            Err(env::VarError::NotPresent) | Ok("logos") => Ok(Self::Logos),
            value => Err(StepError::InvalidArgument {
                message: format!("invalid CUCUMBER_IMPLEMENTATION: {value:?}"),
            }),
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
        }
    }
}
