use std::fmt::Debug;

use futures::stream::BoxStream;
use lb_network_service::{NetworkService, backends::NetworkBackend};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Serialize, de::DeserializeOwned};

use crate::message::BlendPayload;

pub mod libp2p;

/// Hands a fully decapsulated payload over to the local service that owns it.
///
/// This is Blend's exit door: whatever comes through it has finished blending
/// and travels onwards in the clear. Where "onwards" is depends on what the
/// payload carries — a block proposal is republished under the chain's
/// gossipsub topic, a transaction goes to the mempool, which validates it and
/// gossips it on from there.
#[async_trait::async_trait]
pub trait PayloadDispatcher<RuntimeServiceId> {
    /// The network backend used by the network service.
    type Backend: NetworkBackend<RuntimeServiceId> + 'static;
    /// The mempool service transactions are handed over to, and asked about
    /// the ones it accepts.
    type MempoolService: ServiceData<Message: Send + 'static> + 'static;
    /// The chain-network service asked about the block proposals it receives.
    type ChainNetworkService: ServiceData<Message: Send + 'static> + 'static;
    /// Settings used to broadcast messages using the network service.
    type Settings: Clone + Debug + Serialize + DeserializeOwned + Send + Sync + 'static;

    fn new(
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
        mempool_relay: OutboundRelay<<Self::MempoolService as ServiceData>::Message>,
        chain_network_relay: OutboundRelay<<Self::ChainNetworkService as ServiceData>::Message>,
        settings: Self::Settings,
    ) -> Self;

    /// Deliver a decapsulated payload to the local service that owns it.
    async fn dispatch(&self, payload: BlendPayload);

    /// The payloads appearing on the broadcasting channel, whichever node's
    /// exit door put them there — this node's own included.
    async fn observe_broadcasts(&self) -> BoxStream<'static, BlendPayload>;
}
