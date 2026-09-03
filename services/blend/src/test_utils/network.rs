//! A network adapter for tests: both sides of Blend's exit door in the test's
//! hands.

use async_trait::async_trait;
use futures::{StreamExt as _, stream::BoxStream};
use lb_network_service::{NetworkService, backends::NetworkBackend};
use overwatch::{
    overwatch::handle::OverwatchHandle,
    services::{ServiceData, relay::OutboundRelay},
};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::BroadcastStream;

use crate::{core::network::NetworkAdapter, message::NetworkMessage};

const CHANNEL_SIZE: usize = 16;

/// The two ends of the broadcasting channel a test drives: what this node put
/// on it, and what the test says the rest of the network put on it.
pub struct TestBroadcastingChannel {
    /// What Blend's exit door published, this node's direct broadcasts among
    /// it.
    pub broadcasted: mpsc::UnboundedReceiver<NetworkMessage<()>>,
    /// What the test wants this node to see arriving on the channel.
    pub carrying: broadcast::Sender<NetworkMessage<()>>,
}

pub struct TestNetworkAdapter {
    broadcasted: mpsc::UnboundedSender<NetworkMessage<()>>,
    carrying: broadcast::Sender<NetworkMessage<()>>,
}

impl TestNetworkAdapter {
    #[must_use]
    pub fn new() -> (Self, TestBroadcastingChannel) {
        let (broadcasted_sender, broadcasted) = mpsc::unbounded_channel();
        let (carrying, _) = broadcast::channel(CHANNEL_SIZE);
        (
            Self {
                broadcasted: broadcasted_sender,
                carrying: carrying.clone(),
            },
            TestBroadcastingChannel {
                broadcasted,
                carrying,
            },
        )
    }
}

#[async_trait]
impl<RuntimeServiceId> NetworkAdapter<RuntimeServiceId> for TestNetworkAdapter
where
    RuntimeServiceId: Send + Sync + 'static,
{
    type Backend = TestNetworkBackend;
    type BroadcastSettings = ();

    fn new(
        _network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self {
        Self::new().0
    }

    async fn broadcast(&self, message: Vec<u8>, broadcast_settings: Self::BroadcastSettings) {
        drop(self.broadcasted.send(NetworkMessage {
            message,
            broadcast_settings,
        }));
    }

    async fn observe_broadcasts(
        &self,
    ) -> BoxStream<'static, NetworkMessage<Self::BroadcastSettings>> {
        BroadcastStream::new(self.carrying.subscribe())
            .filter_map(async |observed| observed.ok())
            .boxed()
    }
}

pub struct TestNetworkBackend;

#[async_trait]
impl<RuntimeServiceId> NetworkBackend<RuntimeServiceId> for TestNetworkBackend {
    type Settings = ();
    type Message = ();
    type PubSubEvent = ();
    type ChainSyncEvent = ();

    fn new(_config: Self::Settings, _overwatch_handle: OverwatchHandle<RuntimeServiceId>) -> Self {
        Self
    }

    async fn process(&self, (): Self::Message) {}

    async fn subscribe_to_pubsub(&mut self) -> BroadcastStream<Self::PubSubEvent> {
        unimplemented!()
    }

    async fn subscribe_to_chainsync(&mut self) -> BroadcastStream<Self::ChainSyncEvent> {
        unimplemented!()
    }
}
