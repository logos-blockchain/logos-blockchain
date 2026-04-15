use std::time::Duration;

use lb_libp2p::{Multiaddr, ed25519};
use lb_node::config::network::serde as network;
use lb_testing_framework::get_reserved_available_udp_port;

use crate::node_address_from_port;

#[derive(Default)]
pub enum Libp2pNetworkLayout {
    #[default]
    Star,
    Chain,
    Full,
}

#[derive(Default)]
pub struct NetworkParams {
    pub libp2p_network_layout: Libp2pNetworkLayout,
}

pub type GeneralNetworkConfig = network::Config;

fn default_swarm_config() -> network::SwarmConfig {
    network::SwarmConfig::default()
}

#[must_use]
pub fn create_network_configs(
    ids: &[[u8; 32]],
    network_params: &NetworkParams,
) -> Vec<GeneralNetworkConfig> {
    let swarm_configs: Vec<network::SwarmConfig> = ids
        .iter()
        .map(|id| {
            let mut node_key_bytes = *id;
            let node_key = ed25519::SecretKey::try_from_bytes(&mut node_key_bytes)
                .expect("Failed to generate secret key from bytes");

            network::SwarmConfig {
                node_key,
                port: get_reserved_available_udp_port().unwrap(),
                chain_sync: network::chainsync::Config {
                    peer_response_timeout: Duration::from_secs(60),
                },
                ..default_swarm_config()
            }
        })
        .collect();

    let all_initial_peers = initial_peers_by_network_layout(&swarm_configs, network_params);

    swarm_configs
        .iter()
        .zip(all_initial_peers)
        .map(|(swarm_config, initial_peers)| GeneralNetworkConfig {
            backend: network::BackendSettings {
                initial_peers,
                swarm: swarm_config.to_owned(),
            },
        })
        .collect()
}

fn initial_peers_by_network_layout(
    swarm_configs: &[network::SwarmConfig],
    network_params: &NetworkParams,
) -> Vec<Vec<Multiaddr>> {
    let mut all_initial_peers = vec![];

    match network_params.libp2p_network_layout {
        Libp2pNetworkLayout::Star => {
            // First node is the hub - has no initial peers
            all_initial_peers.push(vec![]);
            let first_addr = node_address_from_port(swarm_configs[0].port);

            // All other nodes connect to the first node
            for _ in 1..swarm_configs.len() {
                all_initial_peers.push(vec![first_addr.clone()]);
            }
        }
        Libp2pNetworkLayout::Chain => {
            // First node has no initial peers
            all_initial_peers.push(vec![]);

            // Each subsequent node connects to the previous one
            for i in 1..swarm_configs.len() {
                let prev_addr = node_address_from_port(swarm_configs[i - 1].port);
                all_initial_peers.push(vec![prev_addr]);
            }
        }
        Libp2pNetworkLayout::Full => {
            // Each node connects to all previous nodes, unidirectional connections
            for i in 0..swarm_configs.len() {
                let mut peers = vec![];
                for swarm_config in swarm_configs.iter().take(i) {
                    peers.push(node_address_from_port(swarm_config.port));
                }
                all_initial_peers.push(peers);
            }
        }
    }

    all_initial_peers
}
