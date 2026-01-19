#[cfg(feature = "libp2p")]
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
    /// Settings used to broadcast messages using the network service.
    type BroadcastSettings: Clone + Debug + Serialize + DeserializeOwned + Send + Sync + 'static;

    fn new(
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self;
    /// Broadcast a message to the network service using the specified broadcast
    /// settings.
    async fn broadcast(&self, message: Vec<u8>, broadcast_settings: Self::BroadcastSettings);
}
