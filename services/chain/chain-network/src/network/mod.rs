pub mod adapters;

use std::collections::HashSet;

use futures::Stream;
use lb_core::header::HeaderId;
use lb_cryptarchia_engine::Era;
use lb_cryptarchia_sync::TipResponse;
use lb_network_service::{
    backends::NetworkBackend,
    message::{BackendNetworkMsg, ChainSyncEvent},
};
use overwatch::{DynError, services::relay::OutboundRelay};

pub(crate) type BoxedStream<T> = Box<dyn Stream<Item = T> + Send + Unpin>;

#[async_trait::async_trait]
pub trait NetworkAdapter<RuntimeServiceId> {
    type Backend: NetworkBackend<RuntimeServiceId> + 'static;
    type Settings: Clone + 'static;
    type PeerId;
    type Block;
    type Proposal;

    async fn new(
        settings: Self::Settings,
        current_era: Era,
        network_relay: OutboundRelay<BackendNetworkMsg<Self::Backend, RuntimeServiceId>>,
    ) -> Self;

    async fn rotate_era(&mut self, era: Era);
    async fn complete_era_transition(&mut self, ended_era: Era);

    async fn proposals_stream(&self) -> Result<BoxedStream<Self::Proposal>, DynError>;

    async fn chainsync_events_stream(&self) -> Result<BoxedStream<ChainSyncEvent>, DynError>;

    async fn request_tip(&self, peer: Self::PeerId) -> Result<TipResponse, DynError>;

    /// Sample up to `max_peers` currently-connected peers and request their
    /// chain tip via tip requests, concurrently. The returned stream yields
    /// each successful response as it resolves; per-peer failures are
    /// dropped.
    ///
    /// Used by the proactive tip-polling lag watchdog.
    async fn sample_tips(&self, max_peers: usize) -> BoxedStream<TipResponse>;

    async fn request_blocks_from_peer(
        &self,
        peer: Self::PeerId,
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: HashSet<HeaderId>,
    ) -> Result<BoxedStream<Result<(HeaderId, Self::Block), DynError>>, DynError>;

    async fn request_blocks_from_peers(
        &self,
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: HashSet<HeaderId>,
    ) -> Result<BoxedStream<Result<(HeaderId, Self::Block), DynError>>, DynError>;
}
