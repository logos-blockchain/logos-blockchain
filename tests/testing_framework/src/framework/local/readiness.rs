use std::{collections::HashSet, time::Duration};

use lb_libp2p::{Multiaddr, Protocol};
use testing_framework_core::scenario::{DynError, StabilizationConfig, wait_until_stable};
use testing_framework_runner_local::env::Node;
use thiserror::Error;

use crate::framework::LbcEnv;

const READINESS_STABILIZATION_TIMEOUT: Duration = Duration::from_mins(1);
const READINESS_STABILIZATION_POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Error)]
enum ReadinessError {
    #[error("node-{node_index}: network_info failed during readiness stabilization: {detail}")]
    NetworkInfo { node_index: usize, detail: String },
}

pub(super) async fn wait_readiness_stable(nodes: &[Node<LbcEnv>]) -> Result<(), DynError> {
    if nodes.is_empty() {
        return Ok(());
    }

    let expected_peer_counts = expected_peer_counts(nodes);
    let config = StabilizationConfig::new(
        READINESS_STABILIZATION_TIMEOUT,
        READINESS_STABILIZATION_POLL,
    );

    wait_until_stable(config, async || {
        Ok::<_, DynError>(collect_readiness_failures(nodes, &expected_peer_counts).await)
    })
    .await
    .map_err(|error| Box::new(error) as DynError)
}

fn expected_peer_counts(nodes: &[Node<LbcEnv>]) -> Vec<usize> {
    let listen_ports = nodes
        .iter()
        .map(|node| node.config().user.network.backend.swarm.port)
        .collect::<Vec<_>>();

    let initial_peer_ports = nodes
        .iter()
        .map(|node| {
            node.config()
                .user
                .network
                .backend
                .initial_peers
                .iter()
                .filter_map(multiaddr_port)
                .collect::<HashSet<_>>()
        })
        .collect::<Vec<_>>();

    find_expected_peer_counts(&listen_ports, &initial_peer_ports)
}

async fn collect_readiness_failures(
    nodes: &[Node<LbcEnv>],
    expected_peer_counts: &[usize],
) -> Vec<String> {
    // A single-node topology is stable immediately once health/readiness endpoint
    // is up.
    if nodes.len() <= 1 {
        return Vec::new();
    }

    let mut failures = Vec::new();
    for (idx, node) in nodes.iter().enumerate() {
        let expected = expected_peer_counts.get(idx).copied().unwrap_or(0);
        if let Some(failure) = readiness_failure_for_node(idx, node, expected).await {
            failures.push(failure);
        }
    }

    failures
}

async fn readiness_failure_for_node(
    index: usize,
    node: &Node<LbcEnv>,
    expected_peers: usize,
) -> Option<String> {
    match node.client_ref().network_info().await {
        Ok(info) => (info.n_peers < expected_peers).then_some(format!(
            "node-{index}: peers={} expected>={expected_peers}",
            info.n_peers
        )),
        Err(error) => Some(
            ReadinessError::NetworkInfo {
                node_index: index,
                detail: error.to_string(),
            }
            .to_string(),
        ),
    }
}

fn find_expected_peer_counts(
    listen_ports: &[u16],
    initial_peer_ports: &[HashSet<u16>],
) -> Vec<usize> {
    let mut expected: Vec<HashSet<usize>> = vec![HashSet::new(); initial_peer_ports.len()];

    for (idx, ports) in initial_peer_ports.iter().enumerate() {
        for port in ports {
            let Some(peer_idx) = listen_ports.iter().position(|p| p == port) else {
                continue;
            };

            if peer_idx == idx {
                continue;
            }

            expected[idx].insert(peer_idx);
            expected[peer_idx].insert(idx);
        }
    }

    expected.into_iter().map(|set| set.len()).collect()
}

fn multiaddr_port(addr: &Multiaddr) -> Option<u16> {
    addr.iter().find_map(|protocol| match protocol {
        Protocol::Udp(port) | Protocol::Tcp(port) => Some(port),
        _ => None,
    })
}
