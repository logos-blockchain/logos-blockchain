use lb_blend::scheduling::membership::Membership;
use lb_libp2p::NetworkBehaviour;
use libp2p::{PeerId, allow_block_list::BlockedPeers, connection_limits::ConnectionLimits};

use crate::core::{
    backends::libp2p::Libp2pBlendBackendSettings, settings::RunningBlendConfig as BlendConfig,
};

#[derive(NetworkBehaviour)]
pub struct BlendBehaviour<ProofsVerifier, ObservationWindowProvider> {
    pub blend: lb_blend::network::core::NetworkBehaviour<ProofsVerifier, ObservationWindowProvider>,
    pub limits: libp2p::connection_limits::Behaviour,
    pub blocked_peers: libp2p::allow_block_list::Behaviour<BlockedPeers>,
}

impl<ProofsVerifier, ObservationWindowProvider>
    BlendBehaviour<ProofsVerifier, ObservationWindowProvider>
where
    ProofsVerifier: Clone,
    ObservationWindowProvider: for<'c> From<(
        &'c BlendConfig<Libp2pBlendBackendSettings>,
        &'c Membership<PeerId>,
    )>,
{
    pub fn new(
        config: &BlendConfig<Libp2pBlendBackendSettings>,
        current_membership: Membership<PeerId>,
        poq_verifier: ProofsVerifier,
    ) -> Self {
        let observation_window_interval_provider =
            ObservationWindowProvider::from((config, &current_membership));
        let minimum_core_healthy_peering_degree =
            *config.backend.core_peering_degree.start() as usize;
        let maximum_core_peering_degree = *config.backend.core_peering_degree.end() as usize;
        let maximum_edge_incoming_connections =
            config.backend.max_edge_node_incoming_connections as usize;

        // We double max core peering degree for session transition period
        let maximum_established_outgoing_connections =
            maximum_core_peering_degree.saturating_mul(2);
        let maximum_established_connections = maximum_established_outgoing_connections
            .saturating_add(maximum_edge_incoming_connections);

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
                current_membership,
                config.peer_id(),
                config.backend.protocol_name.clone().into_inner(),
                poq_verifier,
            ),
            limits: libp2p::connection_limits::Behaviour::new(
                ConnectionLimits::default()
                    .with_max_established(Some(maximum_established_connections as u32))
                    // Max established incoming = max established.
                    .with_max_established_incoming(Some(maximum_established_connections as u32))
                    .with_max_established_outgoing(Some(
                        maximum_established_outgoing_connections as u32,
                    )),
            ),
            blocked_peers: libp2p::allow_block_list::Behaviour::default(),
        }
    }
}
