use core::num::NonZeroU128;

use lb_blend::{primitives::time::RoundCount, scheduling::membership::Membership};
use lb_chain_service::Epoch;
use lb_libp2p::NetworkBehaviour;
use libp2p::PeerId;

use crate::core::{
    backends::libp2p::Libp2pBlendBackendSettings, settings::RunningBlendConfig as BlendConfig,
};

#[derive(NetworkBehaviour)]
pub struct BlendBehaviour<ProofsVerifier> {
    pub blend: lb_blend::network::core::NetworkBehaviour<ProofsVerifier>,
}

impl<ProofsVerifier> BlendBehaviour<ProofsVerifier>
where
    ProofsVerifier: Clone,
{
    pub fn new(
        config: &BlendConfig<Libp2pBlendBackendSettings>,
        current_membership_info: (Membership<PeerId>, Epoch),
        proofs_verifier: ProofsVerifier,
    ) -> Self {
        let maximum_edge_incoming_connections =
            config.backend.max_edge_node_incoming_connections.get() as usize;

        Self {
            blend: lb_blend::network::core::NetworkBehaviour::new(
                &lb_blend::network::core::Config {
                    common: lb_blend::network::core::CommonConfig {
                        minimum_network_size: config.minimum_network_size.try_into().unwrap(),
                        num_blend_layers: config.num_blend_layers,
                        round_duration_in_seconds: config.time.round_duration_in_seconds,
                    },
                    with_core: lb_blend::network::core::with_core::behaviour::Config {
                        target_peering_degree: (config.backend.target_peering_degree.get()
                            as usize)
                            .try_into()
                            .unwrap(),
                        liveness_window_in_rounds: config.time.rounds_per_observation_window,
                        connection_share_per_round: config.backend.connection_share_per_round,
                        send_deadline_in_rounds: RoundCount::new(NonZeroU128::from(
                            config.time.network_absorption_in_rounds,
                        )),
                        handshake_deadline_in_rounds: RoundCount::new(
                            config.time.core_handshake_deadline_in_rounds,
                        ),
                    },
                    with_edge: lb_blend::network::core::with_edge::behaviour::Config {
                        connection_timeout: config.backend.edge_node_connection_timeout,
                        max_incoming_connections: maximum_edge_incoming_connections,
                        accepted_connections_per_round: config
                            .backend
                            .accepted_edge_connections_per_round,
                    },
                },
                current_membership_info,
                proofs_verifier,
                config.peer_id(),
                config.backend.protocol_name.clone().into_inner(),
            ),
        }
    }
}
