pub mod adapters;

use std::collections::HashSet;

use futures::Stream;
use lb_binary_codec::canonical::BinaryDecode;
use lb_core::header::HeaderId;
use lb_cryptarchia_sync::GetTipResponse;
use lb_network_service::{NetworkService, backends::NetworkBackend, message::ChainSyncEvent};
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};

pub(crate) type BoxedStream<T> = Box<dyn Stream<Item = T> + Send + Unpin>;

/// The chain's network part that does not depend on eras: chain sync.
#[async_trait::async_trait]
pub trait NetworkAdapter<RuntimeServiceId> {
    type Backend: NetworkBackend<RuntimeServiceId> + 'static;
    type Settings;
    type PeerId;
    type Block;

    async fn new(
        settings: Self::Settings,
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self;

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

/// The chain's network in one era: the proposals gossiped on its topic.
/// Dropping the adapter leaves the era.
#[async_trait::async_trait]
pub trait EraNetworkAdapter<RuntimeServiceId> {
    type Backend: NetworkBackend<RuntimeServiceId> + 'static;
    type Settings;

    async fn new(
        settings: Self::Settings,
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self;

    /// The era's proposals, decoded with `decoding_context` as `Proposal`: the
    /// proposal type of the era's version.
    async fn proposals_stream<Proposal>(
        &self,
        proposal_decoding_context: Proposal::Context,
    ) -> Result<BoxedStream<Proposal>, DynError>
    where
        Proposal: BinaryDecode<Context: Send + 'static> + Send + 'static;
}
