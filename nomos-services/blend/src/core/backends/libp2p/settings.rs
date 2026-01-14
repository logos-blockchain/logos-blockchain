use core::time::Duration;
use std::{num::NonZeroU64, ops::RangeInclusive};

use libp2p::{Multiaddr, PeerId, identity::Keypair};
use nomos_libp2p::protocol_name::StreamProtocol;
use nomos_utils::math::NonNegativeF64;
use serde::{Deserialize, Serialize};

use crate::core::settings::BlendConfig;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde_with::serde_as]
pub struct Libp2pBlendBackendSettings {
    pub listening_address: Multiaddr,
    pub core_peering_degree: RangeInclusive<u64>,
    pub minimum_messages_coefficient: NonZeroU64,
    pub normalization_constant: NonNegativeF64,
    #[serde_as(
        as = "nomos_utils::bounded_duration::MinimalBoundedDuration<1, nomos_utils::bounded_duration::SECOND>"
    )]
    pub edge_node_connection_timeout: Duration,
    pub max_edge_node_incoming_connections: u64,
    pub max_dial_attempts_per_peer: NonZeroU64,
    pub protocol_name: StreamProtocol,
}

impl BlendConfig<Libp2pBlendBackendSettings> {
    #[must_use]
    pub fn keypair(&self) -> Keypair {
        let mut secret_key_bytes = *self.crypto.non_ephemeral_signing_key.as_bytes();
        Keypair::ed25519_from_bytes(&mut secret_key_bytes)
            .expect("Cryptographic secret key should be a valid Ed25519 private key.")
    }

    #[must_use]
    pub fn peer_id(&self) -> PeerId {
        self.keypair().public().to_peer_id()
    }
}
