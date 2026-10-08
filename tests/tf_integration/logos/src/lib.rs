//! Logos blockchain integration layer on top of `testing-framework`.
//!
//! Main entry points:
//! - scenario/deployer entry points from this crate root (or `prelude`)
//! - `preparation` for shared network material and wallet inputs
//! - `config` for native configuration; `local`, Compose and Kubernetes
//!   adapters deploy the resulting Logos nodes
//! - `configs::*` retains the existing Logos configuration imports
//! - `NodeHttpClient` for node API calls

use std::sync::LazyLock;

mod app;
mod block_feed;
mod cfgsync;
mod compose;
pub mod config;
mod constants;
mod deployment_artifacts;
mod diagnostics;
pub mod env;
mod http_client;
mod image;
mod k8s;
pub mod local;
mod runtime_info;
mod scenario;
mod snapshot;
pub use blockchain_test_support::{
    get_reserved_available_tcp_port, get_reserved_available_udp_port, hash_str, preparation,
    release_reserved_port_block, unique_test_context,
};
pub use local::{
    LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_SHA256, LOGOS_BLOCKCHAIN_NODE_DOWNLOAD_URL, USER_CONFIG_FILE,
    ensure_node_binary_built,
};
pub mod workloads;

pub static IS_DEBUG_TRACING: LazyLock<bool> = LazyLock::new(env::debug_tracing);
pub const LOG_LEVEL: &str = "LOG_LEVEL";

pub use app::LbcClusterApp;
pub use block_feed::{
    BlockFeed, BlockFeedCollector, BlockFeedCollectorRuntime, BlockFeedObservation,
    BlockFeedObserver, BlockFeedSnapshot, BlockFeedWaitError, BlockRecord, BoxedBlockFeedCollector,
    NodeHeadSnapshot, ObservedBlock, block_feed_sources, named_block_feed_sources,
};
pub use config as configs;
// Required by reused node-test config modules importing from crate root.
pub use config::deployment::{DeploymentBuilder, TopologyConfig, resolve_automatic_genesis_time};
pub use diagnostics::{
    FailureDiagnosticsExpectation, ScenarioRunDiagnosticsError, record_system_monitor_event,
    register_system_monitor_output_file, run_with_failure_diagnostics,
    unregister_system_monitor_output_file,
};
pub use http_client::NodeHttpClient;
pub use preparation::SharedDeployment;
pub(crate) use scenario::apply_wallet_config_to_deployment;
pub use scenario::{
    DeploymentPlan, LbcClusterBackend, LbcEnv, LbcK8sManualCluster, LbcManualCluster, LbcScenario,
    NodePlan, ScenarioBuilder, ScenarioBuilderExt,
};
pub use snapshot::NodeStateSnapshotStore;
pub use testing_framework_app::{
    AppHost, AppHostDeployError, AppHostDeployer, AppHostEnv, AppRunContextExt,
};
pub use testing_framework_runner_compose::ComposeRunnerError;
pub use testing_framework_runner_k8s::ManualClusterError as K8sManualClusterError;
pub use workloads::{ClusterForkMonitor, ConsensusLiveness, inscription, transaction};

/// Internal helpers for sibling workspace crates.
#[doc(hidden)]
pub mod internal {
    pub use crate::{DeploymentPlan, NodePlan, scenario::apply_wallet_config_to_deployment};
}

pub mod prelude {
    pub use crate::{AppHostDeployer, LbcManualCluster, ScenarioBuilder, ScenarioBuilderExt as _};
}

#[must_use]
pub fn is_truthy_env(key: &str) -> bool {
    std::env::var(key)
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}
