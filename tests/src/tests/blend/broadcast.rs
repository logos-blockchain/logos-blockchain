use core::time::Duration;
use std::collections::HashSet;

use futures::{StreamExt as _, stream};
use logos_blockchain_tests::{
    common::time::max_block_propagation_time,
    topology::{Topology, TopologyConfig},
};
use serial_test::serial;

async fn blend_broadcast_test(topology: &Topology) {
    let nodes = topology.validators();
    let config = nodes[0].config();

    let n_blocks = config.deployment.cryptarchia.security_param.get();
    let timeout = max_block_propagation_time(
        n_blocks,
        nodes.len().try_into().unwrap(),
        &config.deployment,
        3.0,
    );
    println!("waiting for {n_blocks} blocks: timeout:{timeout:?}");
    let timeout = tokio::time::sleep(timeout);

    {
        let mut tick: u32 = 0;
        tokio::select! {
            () = timeout => panic!("timed out waiting for nodes to produce {} blocks", n_blocks),
            () = async { while stream::iter(nodes)
                .any(async |n| (n.consensus_info(tick == 0).await.height as u32) < n_blocks)
                .await
            {
                if tick.is_multiple_of(10) {
                    println!(
                        "waiting... {}",
                        stream::iter(nodes)
                            .then(async |n| { format!("{}", n.consensus_info(false).await.height) })
                            .collect::<Vec<_>>()
                            .await
                            .join(" | ")
                    );
                }
                tick = tick.wrapping_add(1);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            } => {}
        }
    }

    println!("{:?}", nodes[0].consensus_info(true).await);

    let infos = stream::iter(nodes)
        .then(async |n| n.get_headers(None, None, true).await)
        // TODO: this can actually fail if the one node is slightly behind, we should really either
        // get the block at a specific height, but we currently lack the API for that
        .map(|blocks| blocks.last().copied().unwrap()) // we're getting the LIB
        .collect::<HashSet<_>>()
        .await;

    assert_eq!(infos.len(), 1, "consensus not reached");
}

/// Tests Blend in Broadcast mode: no SDP blend providers are declared,
/// so `membership.size()` is 0, which is below `minimum_network_size` (1). All
/// nodes therefore operate in Broadcast mode, directly forwarding messages
/// without blend mixing.
///
/// Verifies that consensus still makes forward progress when the blend network
/// is inactive and nodes fall back to plain broadcast.
#[tokio::test]
#[serial]
async fn blend_broadcast() {
    // 4 nodes, 0 declared as SDP blend providers.
    // membership.size() = 0 < minimum_network_size = 1 → all nodes use Broadcast
    // mode.
    let topology = Topology::spawn(
        TopologyConfig::n_validators_with_m_blend_node(4, 0),
        Some("blend_broadcast_e2e"),
    )
    .await;
    blend_broadcast_test(&topology).await;
}
