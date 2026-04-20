use lb_blend::scheduling::membership::Membership;
use lb_libp2p::NetworkBehaviour;
use libp2p::{PeerId, allow_block_list::BlockedPeers};

use crate::core::{
    backends::libp2p::Libp2pBlendBackendSettings, settings::RunningBlendConfig as BlendConfig,
};

#[derive(NetworkBehaviour)]
pub struct BlendBehaviour<ObservationWindowProvider> {
    pub blend: lb_blend::network::core::NetworkBehaviour<ObservationWindowProvider>,
    pub blocked_peers: libp2p::allow_block_list::Behaviour<BlockedPeers>,
}

impl<ObservationWindowProvider> BlendBehaviour<ObservationWindowProvider>
where
    ObservationWindowProvider: for<'c> From<(
        &'c BlendConfig<Libp2pBlendBackendSettings>,
        &'c Membership<PeerId>,
    )>,
{
    pub fn new(
        config: &BlendConfig<Libp2pBlendBackendSettings>,
        current_membership_info: (Membership<PeerId>, u64),
    ) -> Self {
        let observation_window_interval_provider =
            ObservationWindowProvider::from((config, &current_membership_info.0));
        let minimum_core_healthy_peering_degree =
            *config.backend.core_peering_degree.start() as usize;
        let maximum_core_peering_degree = *config.backend.core_peering_degree.end() as usize;
        let maximum_edge_incoming_connections =
            config.backend.max_edge_node_incoming_connections as usize;

        Self {
            blend: lb_blend::network::core::NetworkBehaviour::new(
                &lb_blend::network::core::Config {
                    with_core: lb_blend::network::core::with_core::behaviour::Config {
                        peering_degree: minimum_core_healthy_peering_degree
                            ..=maximum_core_peering_degree,
                        minimum_network_size: config.minimum_network_size.try_into().unwrap(),
                    },
                    with_edge: lb_blend::network::core::with_edge::behaviour::Config {
                        connection_timeout: config.backend.edge_node_connection_timeout,
                        max_incoming_connections: maximum_edge_incoming_connections,
                        minimum_network_size: config.minimum_network_size.try_into().unwrap(),
                    },
                },
                observation_window_interval_provider,
                current_membership_info,
                config.peer_id(),
                config.backend.protocol_name.clone().into_inner(),
            ),
            blocked_peers: libp2p::allow_block_list::Behaviour::default(),
        }
    }
}
