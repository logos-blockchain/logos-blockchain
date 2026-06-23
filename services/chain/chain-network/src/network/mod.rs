pub mod adapters;

use std::collections::HashSet;

use futures::Stream;
use lb_core::header::HeaderId;
use lb_cryptarchia_sync::GetTipResponse;
use lb_network_service::{NetworkService, backends::NetworkBackend, message::ChainSyncEvent};
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};

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
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self;
    async fn proposals_stream(&self) -> Result<BoxedStream<Self::Proposal>, DynError>;

    async fn chainsync_events_stream(&self) -> Result<BoxedStream<ChainSyncEvent>, DynError>;

    async fn request_tip(&self, peer: Self::PeerId) -> Result<GetTipResponse, DynError>;

    /// Sample up to `max_peers` currently-connected peers and request their
    /// chain tip via `GetTip`, concurrently. The returned stream yields each
    /// successful response as it resolves; per-peer failures are dropped.
    ///
    /// Used by the proactive tip-polling lag watchdog.
    async fn sample_tips(&self, max_peers: usize) -> BoxedStream<GetTipResponse>;

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
