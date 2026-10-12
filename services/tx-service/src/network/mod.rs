pub mod adapters;

use futures::Stream;
use lb_network_service::{backends::NetworkBackend, message::BackendNetworkMsg};
use overwatch::services::relay::OutboundRelay;

#[async_trait::async_trait]
pub trait NetworkAdapter<RuntimeServiceId> {
    type Backend: NetworkBackend<RuntimeServiceId> + 'static;
    type Settings: Clone;
    type Payload: Send + Sync + 'static;
    type Key: Send + Sync + 'static;

    async fn new(
        settings: Self::Settings,
        network_relay: OutboundRelay<BackendNetworkMsg<Self::Backend, RuntimeServiceId>>,
    ) -> Self;

    async fn payload_stream(
        &self,
    ) -> Box<dyn Stream<Item = (Self::Key, Self::Payload)> + Unpin + Send>;

    async fn send(&self, payload: Self::Payload);
}
