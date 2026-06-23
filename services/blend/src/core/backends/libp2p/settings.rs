use core::{pin::Pin, time::Duration};
use std::{num::NonZeroU64, ops::RangeInclusive};

use futures::{Stream, StreamExt as _, stream::pending};
use lb_libp2p::protocol_name::StreamProtocol;
use lb_utils::math::NonNegativeF64;
use libp2p::{Multiaddr, PeerId, identity::Keypair};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_stream::wrappers::IntervalStream;

use crate::core::settings::RunningBlendConfig as BlendConfig;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde_with::serde_as]
pub struct Libp2pBlendBackendSettings {
    pub listening_address: Multiaddr,
    pub core_peering_degree: RangeInclusive<u64>,
    pub minimum_messages_coefficient: NonZeroU64,
    pub normalization_constant: NonNegativeF64,
    #[serde_as(
        as = "lb_utils::bounded_duration::MinimalBoundedDuration<1, lb_utils::bounded_duration::SECOND>"
    )]
    pub edge_node_connection_timeout: Duration,
    pub max_edge_node_incoming_connections: u64,
    pub max_dial_attempts_per_peer: NonZeroU64,
    pub protocol_name: StreamProtocol,
    pub peering_degree_check_interval: Option<Duration>,
}

impl BlendConfig<Libp2pBlendBackendSettings> {
    #[must_use]
    pub fn keypair(&self) -> Keypair {
        let mut secret_key_bytes = *self.non_ephemeral_signing_key.as_bytes();
        Keypair::ed25519_from_bytes(&mut secret_key_bytes)
            .expect("Cryptographic secret key should be a valid Ed25519 private key.")
    }

    #[must_use]
    pub fn peer_id(&self) -> PeerId {
        self.keypair().public().to_peer_id()
    }

    #[must_use]
    pub fn peering_degree_check_clock(&self) -> Pin<Box<dyn Stream<Item = ()> + Send>> {
        let Some(interval_duration) = self.backend.peering_degree_check_interval else {
            // If no interval is configured, return a stream that never yields anything.
            return Box::pin(pending());
        };
        let mut interval = interval_at(
            Instant::now()
                .checked_add(interval_duration)
                .expect("Peering degree check interval value too large."),
            interval_duration,
        );
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Box::pin(IntervalStream::new(interval).map(|_| ()))
    }
}
