use core::num::NonZeroU128;

use lb_blend::{primitives::time::RoundCount, scheduling::membership::Membership};
use lb_chain_service::Epoch;
use lb_libp2p::NetworkBehaviour;
use libp2p::PeerId;

use crate::core::backends::libp2p::Libp2pBlendBackendEraSettings;

#[derive(NetworkBehaviour)]
pub struct BlendBehaviour<ProofsVerifier> {
    pub blend: lb_blend::network::core::NetworkBehaviour<ProofsVerifier>,
}

impl<ProofsVerifier> BlendBehaviour<ProofsVerifier>
where
    ProofsVerifier: Clone,
{
    pub fn new(
        era_settings: &Libp2pBlendBackendEraSettings,
        local_peer_id: PeerId,
        current_membership_info: (Membership<PeerId>, Epoch),
        proofs_verifier: ProofsVerifier,
    ) -> Self {
        Self {
            blend: lb_blend::network::core::NetworkBehaviour::new(
                &network_config(era_settings),
                current_membership_info,
                proofs_verifier,
                local_peer_id,
                era_settings.protocol_name.clone().into_inner(),
            ),
        }
    }
}

/// The settings of the Blend network behaviour in an era.
pub fn network_config(
    era_settings: &Libp2pBlendBackendEraSettings,
) -> lb_blend::network::core::Config {
    let maximum_edge_incoming_connections =
        era_settings.max_edge_node_incoming_connections.get() as usize;
    lb_blend::network::core::Config {
        common: lb_blend::network::core::CommonConfig {
            minimum_network_size: era_settings.minimum_network_size.try_into().unwrap(),
            num_blend_layers: era_settings.num_blend_layers,
            round_duration_in_seconds: era_settings.time.round_duration_in_seconds,
        },
        with_core: lb_blend::network::core::with_core::behaviour::Config {
            target_peering_degree: (era_settings.target_peering_degree.get() as usize)
                .try_into()
                .unwrap(),
            liveness_window_in_rounds: era_settings.time.rounds_per_observation_window,
            connection_share_per_round: era_settings.connection_share_per_round,
            send_deadline_in_rounds: RoundCount::new(NonZeroU128::from(
                era_settings.time.network_absorption_in_rounds,
            )),
            handshake_deadline_in_rounds: RoundCount::new(
                era_settings.time.core_handshake_deadline_in_rounds,
            ),
        },
        with_edge: lb_blend::network::core::with_edge::behaviour::Config {
            connection_timeout: era_settings.edge_node_connection_timeout,
            max_incoming_connections: maximum_edge_incoming_connections,
            accepted_connections_per_round: era_settings.accepted_edge_connections_per_round,
        },
    }
}
