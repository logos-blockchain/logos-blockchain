#[cfg(feature = "libp2p")]
use lb_libp2p::libp2p::{Swarm, swarm::NetworkBehaviour};

#[cfg(feature = "libp2p")]
pub fn network_dial_failures() {
    lb_tracing::increase_counter_u64!(network_dial_failures_total, 1);
}

#[cfg(feature = "libp2p")]
pub fn consensus_peers_connected(peers_connected: usize) {
    lb_tracing::metric_gauge_u64!(
        consensus_peers_connected,
        u64::try_from(peers_connected).unwrap_or(u64::MAX)
    );
}

#[cfg(feature = "libp2p")]
pub fn consensus_connections(connections: u32) {
    lb_tracing::metric_gauge_u64!(consensus_connections, u64::from(connections));
}

#[cfg(feature = "libp2p")]
pub fn consensus_report_connectivity<B: NetworkBehaviour>(swarm: &Swarm<B>) {
    let network_info = swarm.network_info();
    let counters = network_info.connection_counters();

    consensus_peers_connected(network_info.num_peers());
    consensus_connections(counters.num_connections());
}
