use lb_libp2p::{Multiaddr, SwarmConfig};

#[derive(Clone, Debug)]
pub struct Libp2pConfig {
    pub inner: SwarmConfig,
    // Initial peers to connect to
    pub initial_peers: Vec<Multiaddr>,
}
