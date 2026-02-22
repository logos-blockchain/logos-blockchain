//! Logos blockchain integration layer on top of `testing-framework`.
//!
//! Main entry points:
//! - scenario/deployer entry points from this crate root (or `prelude`)
//! - `configs::*` for topology and wallet configuration
//! - `NodeHttpClient` for node API calls

use std::{net::Ipv4Addr, sync::LazyLock};

use lb_libp2p::{Multiaddr, multiaddr};

pub mod env;
mod framework;
mod node;
pub mod workloads;

pub(crate) mod common {
    pub mod kms {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../src/common/kms.rs"));
    }
}

pub static IS_DEBUG_TRACING: LazyLock<bool> = LazyLock::new(testing_framework_env::debug_tracing);

fn node_address_from_port(port: u16) -> Multiaddr {
    multiaddr(Ipv4Addr::LOCALHOST, port)
}

pub use framework::{
    BlockFeed, BlockFeedSnapshot, BlockRecord, CoreBuilderExt, LbcEnv, LbcLocalDeployer,
    LbcManualCluster, NodeHeadSnapshot, ScenarioBuilder, ScenarioBuilderExt,
};
// Required by reused node-test config modules importing from crate root.
pub use node::configs::deployment::{DeploymentBuilder, TopologyConfig};
pub use node::{NodeHttpClient, configs};
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
