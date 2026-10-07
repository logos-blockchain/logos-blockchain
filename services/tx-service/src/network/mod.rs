pub mod adapters;

use futures::{Stream, future::BoxFuture};
use lb_network_service::{NetworkService, backends::NetworkBackend};
use lb_time_service::backends::TimeBackend;
use overwatch::services::{ServiceData, relay::OutboundRelay};

/// The mempool's side of the network in one era: the items gossiped on its
/// topic. Dropping the adapter leaves the era.
#[async_trait::async_trait]
pub trait NetworkAdapter<RuntimeServiceId> {
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

    /// The broadcast of `payload` on the era's topic. It does not borrow the
    /// adapter, so it can run apart from the caller.
    fn send(&self, payload: Self::Payload) -> BoxFuture<'static, ()>;
}
