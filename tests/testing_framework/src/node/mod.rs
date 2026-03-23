mod cfgsync;
pub mod configs;
mod http_client;

pub use http_client::NodeHttpClient;
use testing_framework_core::topology::generated::{
    DeploymentPlan as CoreDeploymentPlan, NodePlan as CoreNodePlan,
};

pub type NodePlan = CoreNodePlan<configs::Config>;
pub type DeploymentPlan = CoreDeploymentPlan<configs::deployment::TopologyConfig, configs::Config>;
