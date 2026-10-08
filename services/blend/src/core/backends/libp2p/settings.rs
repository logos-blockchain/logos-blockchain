use core::{num::NonZeroU32, pin::Pin, time::Duration};
use std::num::NonZeroU64;

use futures::{Stream, StreamExt as _, stream::pending};
use lb_blend::message::encap::encapsulated_message_encoded_size;
use lb_libp2p::protocol_name::StreamProtocol;
use libp2p::{Multiaddr, PeerId, identity::Keypair};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_stream::wrappers::IntervalStream;

use crate::core::settings::RunningBlendConfig as BlendConfig;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde_with::serde_as]
pub struct Libp2pBlendBackendSettings {
    pub listening_address: Multiaddr,
    /// `Φ_CC`: the peering degree this node maintains with other core nodes.
    pub target_peering_degree: NonZeroU32,
    /// `r₁`: the messages a core connection may carry in one round, in each
    /// direction.
    pub connection_share_per_round: NonZeroU64,
    #[serde_as(
        as = "lb_utils::bounded_duration::MinimalBoundedDuration<1, lb_utils::bounded_duration::SECOND>"
    )]
    pub edge_node_connection_timeout: Duration,
    /// `Φ_CE^Max`: the edge connections this node holds at once.
    pub max_edge_node_incoming_connections: NonZeroU64,
    /// `r_E`: the edge connections this node accepts in one round.
    pub accepted_edge_connections_per_round: NonZeroU64,
    pub max_dial_attempts_per_peer: NonZeroU64,
    pub protocol_name: StreamProtocol,
    pub peering_degree_check_interval: Option<Duration>,
    /// The bytes a stream may hold for this node before its sender feels
    /// backpressure: the QUIC receive window. The transport is built once,
    /// when the swarm starts, so the window is the same in every era: the
    /// largest [`connection_receive_window`] of the chain's eras.
    pub receive_window: u32,
}

/// The bytes a connection may hold for us before its sender feels backpressure.
///
/// One round's share, which a neighbour sending at the rate the protocol
/// expects may have in flight before this node has read it, plus the `η` rounds
/// that neighbour waits for a stalled connection before giving up on a message.
/// Sized this way, a pause in reading shorter than the sender's own patience
/// costs nothing, and one longer than it is felt within a round or two instead
/// of being swallowed by buffer.
#[must_use]
pub fn connection_receive_window(
    connection_share_per_round: NonZeroU64,
    network_absorption_in_rounds: NonZeroU64,
    num_blend_layers: NonZeroU64,
) -> u32 {
    let frame_size = encapsulated_message_encoded_size(num_blend_layers).get();

    let rounds_of_slack = network_absorption_in_rounds.get().saturating_add(1);
    u32::try_from(
        connection_share_per_round
            .get()
            .saturating_mul(rounds_of_slack)
            .saturating_mul(u64::try_from(frame_size).unwrap()),
    )
    .unwrap_or(u32::MAX)
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
