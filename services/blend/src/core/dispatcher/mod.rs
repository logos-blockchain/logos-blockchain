pub mod libp2p;

use std::fmt::Debug;

use lb_network_service::{NetworkService, backends::NetworkBackend};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Serialize, de::DeserializeOwned};

use crate::message::BlendPayload;

/// Hands a fully decapsulated payload over to the local service that owns it.
///
/// This is Blend's exit door: whatever comes through it has finished blending
/// and travels onwards in the clear. Where "onwards" is depends on what the
/// payload carries — a block proposal is republished under the chain's
/// gossipsub topic, a transaction goes to the mempool, which validates it and
/// gossips it on from there. That routing is why this is not a plain broadcast.
#[async_trait::async_trait]
pub trait PayloadDispatcher<RuntimeServiceId> {
    /// The network backend used by the network service.
    type Backend: NetworkBackend<RuntimeServiceId> + 'static;
    /// The mempool service transactions are handed over to.
    ///
    /// An associated type rather than a parameter on the Blend service: the
    /// mempool service's own generics stay behind it, so wiring a dispatcher
    /// costs the Blend service one bound and no new type parameters.
    type MempoolService: ServiceData<Message: Send + 'static> + 'static;
    /// Settings used to broadcast messages using the network service.
    type Settings: Clone + Debug + Serialize + DeserializeOwned + Send + Sync + 'static;

    fn new(
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
        mempool_relay: OutboundRelay<<Self::MempoolService as ServiceData>::Message>,
        settings: Self::Settings,
    ) -> Self;

    /// Deliver a decapsulated payload to the local service that owns it.
    async fn dispatch(&self, payload: BlendPayload);
}
