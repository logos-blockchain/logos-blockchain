pub mod adapters;

use futures::Stream;
use lb_network_service::{NetworkService, backends::NetworkBackend};
use lb_time_service::backends::TimeBackend;
use overwatch::services::{ServiceData, relay::OutboundRelay};

/// The mempool's side of the network in one era: the items gossiped on its
/// topic. The adapter of an era lives while the era is in force, and retires
/// when it no longer is.
#[async_trait::async_trait]
pub trait NetworkAdapter<RuntimeServiceId>: Clone {
    type Backend: NetworkBackend<RuntimeServiceId> + 'static;
    type Settings: Clone;
    type Payload: Send + Sync + 'static;
    type Key: Send + Sync + 'static;
    /// The clock whose slots tell the eras in force.
    type TimeBackend: TimeBackend + 'static;

    async fn new(
        settings: Self::Settings,
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self;

    async fn payload_stream(
        &self,
    ) -> Box<dyn Stream<Item = (Self::Key, Self::Payload)> + Unpin + Send>;

    async fn send(&self, payload: Self::Payload);

    /// Leaves the era: its topic is no longer listened to.
    async fn retire(self);
}
