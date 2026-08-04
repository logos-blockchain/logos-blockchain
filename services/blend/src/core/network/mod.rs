pub mod libp2p;

use std::fmt::Debug;

use lb_network_service::{NetworkService, backends::NetworkBackend};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Serialize, de::DeserializeOwned};

/// A trait for communicating with the network service, which is used to
/// broadcast fully unwrapped messages returned from the blend backend.
#[async_trait::async_trait]
pub trait NetworkAdapter<RuntimeServiceId> {
    /// The network backend used by the network service.
    type Backend: NetworkBackend<RuntimeServiceId> + 'static;
    /// What the adapter needs in order to publish a message — for libp2p, the
    /// gossipsub topic.
    ///
    /// Deployment configuration held by the receiving node, not something a
    /// sender chooses, so it never travels over the Blend network and is never
    /// persisted: it may legitimately change across restarts. Keeping it out of
    /// the payload is what lets the payload be exactly a block proposal.
    type Settings: Clone + Debug + Serialize + DeserializeOwned + Send + Sync + 'static;

    fn new(
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
        settings: Self::Settings,
    ) -> Self;
    /// Broadcast a message to the network service.
    async fn broadcast(&self, message: Vec<u8>);
}
