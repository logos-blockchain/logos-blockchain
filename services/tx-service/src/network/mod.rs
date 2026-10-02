pub mod adapters;

use futures::Stream;
use lb_cryptarchia_engine::Slot;
use lb_network_service::{NetworkService, backends::NetworkBackend};
use lb_time_service::backends::TimeBackend;
use overwatch::services::{ServiceData, relay::OutboundRelay};

/// The mempool's side of the network. Its clones share what it follows.
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

    /// Follows the eras in force at `slot`: receives the items gossiped on
    /// the topics of the era in force and of the era it retires, if any, and
    /// broadcasts on the era in force's.
    async fn follow_eras_at(&self, slot: Slot);

    async fn payload_stream(
        &self,
    ) -> Box<dyn Stream<Item = (Self::Key, Self::Payload)> + Unpin + Send>;

    async fn send(&self, payload: Self::Payload);
}
