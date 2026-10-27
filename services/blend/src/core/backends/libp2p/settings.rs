use core::{num::NonZeroU32, pin::Pin, time::Duration};
use std::num::NonZeroU64;

use futures::{Stream, StreamExt as _, stream::pending};
use lb_cryptarchia_engine::EraSchedule;
use lb_libp2p::protocol_name::StreamProtocol;
use libp2p::Multiaddr;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tokio_stream::wrappers::IntervalStream;

use crate::settings::EraTimingSettings;

#[derive(Clone, Debug)]
pub struct Libp2pBlendBackendSettings {
    pub listening_address: Multiaddr,
    pub max_dial_attempts_per_peer: NonZeroU64,
    pub peering_degree_check_interval: Option<Duration>,
    pub era_schedule: EraSchedule<Libp2pBlendBackendEraSettings>,
}

/// The libp2p backend's settings in an era.
#[derive(Clone, Debug)]
pub struct Libp2pBlendBackendEraSettings {
    /// `Φ_CC`: the peering degree this node maintains with other core nodes.
    pub target_peering_degree: NonZeroU32,
    /// `r₁`: the messages a core connection may carry in one round, in each
    /// direction.
    pub connection_share_per_round: NonZeroU64,
    pub edge_node_connection_timeout: Duration,
    /// `Φ_CE^Max`: the edge connections this node holds at once.
    pub max_edge_node_incoming_connections: NonZeroU64,
    /// `r_E`: the edge connections this node accepts in one round.
    pub accepted_edge_connections_per_round: NonZeroU64,
    pub protocol_name: StreamProtocol,
    pub minimum_network_size: NonZeroU64,
    pub num_blend_layers: NonZeroU64,
    pub time: EraTimingSettings,
}

impl Libp2pBlendBackendSettings {
    pub fn peering_degree_check_clock(&self) -> Pin<Box<dyn Stream<Item = ()> + Send>> {
        let Some(interval_duration) = self.peering_degree_check_interval else {
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
