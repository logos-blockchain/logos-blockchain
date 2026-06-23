//! Verify that the proactive tip-poll lag watchdog can prevent permanent
//! forks when publishing proposals to the blend network doesn't work
//! for some reason.
//!
//! The four nodes are spawned from a node binary built with
//! `--features testing-disable-proposal-publish`, which disables publishing
//! block proposals to the blend network while keep self-applying them to the
//! local chain.
//!
//! With that feature on, each node grow their local chain with blocks proposed
//! by themselves, until the watchdog triggers catch-up downloads.
//! This test confirms that all nodes eventually converge on a common tip
//! through the watchdog's tip-polling and catch-up downloads.
//!
//! Run locally:
//!
//! ```text
//! cargo build -p logos-blockchain-node --release \
//!   --features testing-disable-proposal-publish
//! LOGOS_BLOCKCHAIN_NODE_BIN=$(pwd)/target/release/logos-blockchain-node \
//!   cargo test -p logos-blockchain-tests --features tip_poll_self_heal \
//!   --test test_cryptarchia_tip_poll_self_heal -- --nocapture
//! ```

use std::{num::NonZeroU64, path::PathBuf, time::Duration};

use futures::stream::{self, StreamExt as _};
use lb_node::config::RunConfig;
use lb_testing_framework::{DeploymentBuilder, LbcEnv, TopologyConfig as TfTopologyConfig};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::{
    common::manual_cluster::{
        LocalManualClusterHarnessBase, ManualNodeLayout, start_local_manual_cluster_with_layout,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use serial_test::serial;
use testing_framework_core::scenario::{DynError, StartedNode};

#[tokio::test]
#[serial]
async fn silent_cluster_converges_via_tip_poll() {
    let (_base, nodes) = start_silent_cluster("silent_cluster_converges_via_tip_poll").await;
    wait_for_tip_convergence(&nodes, Duration::from_mins(10)).await;
}

const NODE_COUNT: usize = 4;

async fn start_silent_cluster(
    test_name: &str,
) -> (LocalManualClusterHarnessBase, Vec<StartedNode<LbcEnv>>) {
    start_local_manual_cluster_with_layout(
        test_name,
        "tip-poll-self-heal",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(NODE_COUNT)
                .with_test_context(Some(test_name.to_owned())),
        ),
        NODE_COUNT,
        ManualNodeLayout::SelectNodeSeed(0),
        |cfg| Ok::<_, DynError>(config(cfg)),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    )
    .await
}

fn config(mut config: RunConfig) -> RunConfig {
    config.deployment.time.slot_duration = Duration::from_secs(1);
    config
        .user
        .cryptarchia
        .service
        .bootstrap
        .prolonged_bootstrap_period = Duration::ZERO;
    config.deployment.cryptarchia.security_param = 5.try_into().unwrap();
    // Fast block production to speed up the test
    config.deployment.cryptarchia.slot_activation_coeff =
        NonNegativeRatio::new(1, 2.try_into().unwrap());
    // Aggressive watchdog to speed up the test
    config
        .user
        .cryptarchia
        .network
        .sync
        .tip_poll
        .lag_threshold_blocks = NonZeroU64::new(1).unwrap();
    // Catch-up downloads must reach as many peers as possible to speed up the test
    let network = &mut config.user.cryptarchia.network.network;
    network.max_connected_peers_to_try_download = NODE_COUNT;
    network.max_discovered_peers_to_try_download = NODE_COUNT;

    config
}

const MIN_HEIGHT_TO_WAIT: u64 = 20;
const POLL_INTERVAL: Duration = Duration::from_millis(500);

async fn wait_for_tip_convergence(nodes: &[StartedNode<LbcEnv>], timeout: Duration) {
    let work = async {
        let mut tick: u32 = 0;
        loop {
            if let Some(infos) = collect_infos(nodes).await {
                let node0_tip = infos[0].tip;
                let node0_height = infos[0].height;
                let all_at_min_height = infos.iter().all(|info| info.height >= MIN_HEIGHT_TO_WAIT);
                let all_share_tip = infos.iter().all(|info| info.tip == node0_tip);

                if all_at_min_height && all_share_tip {
                    println!(
                        "converged: all {NODE_COUNT} nodes agree on tip {node0_tip:?} at height {node0_height}",
                    );
                    return;
                }

                if tick.is_multiple_of(10) {
                    let summary = infos
                        .iter()
                        .enumerate()
                        .map(|(i, info)| {
                            format!("n{i}=h{}/s{}", info.height, info.slot.into_inner())
                        })
                        .collect::<Vec<_>>()
                        .join(" | ");
                    println!("waiting on tip convergence: {summary}");
                }
                tick = tick.wrapping_add(1);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    };

    tokio::time::timeout(timeout, work)
        .await
        .unwrap_or_else(|_| panic!("nodes did not converge on a shared tip within {timeout:?}"));
}

async fn collect_infos(
    nodes: &[StartedNode<LbcEnv>],
) -> Option<Vec<lb_chain_service::CryptarchiaInfo>> {
    let infos = stream::iter(nodes)
        .then(async |node| node.client.consensus_info().await.ok())
        .collect::<Vec<_>>()
        .await;
    infos.into_iter().collect::<Option<Vec<_>>>().map(|infos| {
        infos
            .into_iter()
            .map(|info| info.cryptarchia_info)
            .collect()
    })
}
