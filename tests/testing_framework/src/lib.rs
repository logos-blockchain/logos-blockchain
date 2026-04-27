//! Logos blockchain integration layer on top of `testing-framework`.
//!
//! Main entry points:
//! - scenario/deployer entry points from this crate root (or `prelude`)
//! - `configs::*` for topology and wallet configuration
//! - `NodeHttpClient` for node API calls

use std::sync::LazyLock;

mod diagnostics;
pub mod env;
mod framework;
pub use framework::local::USER_CONFIG_FILE;
mod node;
mod unique_persistent;
pub mod workloads;
pub use unique_persistent::{
    get_reserved_available_tcp_port, get_reserved_available_udp_port, hash_str,
    reap_all_stale_port_blocks, release_reserved_port_block, unique_test_context,
};

pub static IS_DEBUG_TRACING: LazyLock<bool> = LazyLock::new(env::debug_tracing);
pub const LOGOS_BLOCKCHAIN_LOG_LEVEL: &str = "LOGOS_BLOCKCHAIN_LOG_LEVEL";

pub use diagnostics::{
    FailureDiagnosticsExpectation, ScenarioRunDiagnosticsError, run_with_failure_diagnostics,
};
pub use framework::{
    BlockFeed, BlockFeedExtensionFactory, BlockFeedObservation, BlockFeedObserver,
    BlockFeedSnapshot, BlockFeedWaitError, BlockRecord, CoreBuilderExt, LbcComposeDeployer, LbcEnv,
    LbcK8sDeployer, LbcK8sManualCluster, LbcLocalDeployer, LbcManualCluster, NodeHeadSnapshot,
    ObservedBlock, ScenarioBuilder, ScenarioBuilderExt, block_feed_source_provider,
    block_feed_sources, named_block_feed_sources,
};
// Required by reused node-test config modules importing from crate root.
pub use node::configs::deployment::{DeploymentBuilder, TopologyConfig};
pub use node::{NodeHttpClient, configs};
pub use testing_framework_runner_compose::ComposeRunnerError;
pub use testing_framework_runner_k8s::{
    K8sRunnerError, ManualClusterError as K8sManualClusterError,
};
pub use workloads::{ClusterForkMonitor, ConsensusLiveness, inscription, transaction};

/// Internal helpers for sibling workspace crates.
#[doc(hidden)]
pub mod internal {
    pub use crate::{
        framework::apply_wallet_config_to_deployment,
        node::{DeploymentPlan, NodePlan},
    };
}

pub mod prelude {
    pub use crate::{
        CoreBuilderExt as _, LbcLocalDeployer, LbcManualCluster, ScenarioBuilder,
        ScenarioBuilderExt as _,
    };
}

#[must_use]
pub fn is_truthy_env(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}
