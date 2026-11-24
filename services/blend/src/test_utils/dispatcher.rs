use async_trait::async_trait;
use futures::{StreamExt as _, stream::BoxStream};
use lb_cryptarchia_engine::Era;
use lb_network_service::{backends::NetworkBackend, message::BackendNetworkMsg};
use overwatch::{
    overwatch::OverwatchHandle,
    services::{ServiceData, relay::OutboundRelay},
};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

use crate::{
    core::dispatcher::PayloadDispatcher,
    message::DataPayload,
    test_utils::mocks::{TestChainNetworkService, TestMempoolService},
};

const CHANNEL_SIZE: usize = 32;

/// A stand-in for Blend's exit door, with both sides of it under the test's
/// control: what the node hands over is reported on one channel, and what the
/// broadcasting channel is carrying is fed in on the other.
pub struct TestPayloadDispatcher {
    dispatched: mpsc::UnboundedSender<DataPayload>,
    broadcasting_channel: broadcast::Sender<DataPayload>,
}

/// What a test holds on to: the payloads the node dispatched, and the handle it
/// puts payloads on the broadcasting channel with.
pub struct TestBroadcastingChannel {
    pub dispatched: mpsc::UnboundedReceiver<DataPayload>,
    pub carrying: broadcast::Sender<DataPayload>,
}

impl TestPayloadDispatcher {
    #[must_use]
    pub fn new() -> (Self, TestBroadcastingChannel) {
        let (dispatched, dispatched_receiver) = mpsc::unbounded_channel();
        let (broadcasting_channel, _) = broadcast::channel(CHANNEL_SIZE);
        (
            Self {
                dispatched,
                broadcasting_channel: broadcasting_channel.clone(),
            },
            TestBroadcastingChannel {
                dispatched: dispatched_receiver,
                carrying: broadcasting_channel,
            },
        )
    }
}

#[async_trait]
impl<RuntimeServiceId> PayloadDispatcher<RuntimeServiceId> for TestPayloadDispatcher
where
    RuntimeServiceId: Send + 'static,
{
    type Backend = TestNetworkBackend;
    type ChainNetworkService = TestChainNetworkService<RuntimeServiceId>;
    type MempoolService = TestMempoolService<RuntimeServiceId>;
    type Settings = ();

    fn new(
        _network_relay: OutboundRelay<BackendNetworkMsg<Self::Backend, RuntimeServiceId>>,
        _mempool_relay: OutboundRelay<<Self::MempoolService as ServiceData>::Message>,
        _chain_network_relay: OutboundRelay<<Self::ChainNetworkService as ServiceData>::Message>,
        (): Self::Settings,
        _current_era: Era,
    ) -> Self {
        Self::new().0
    }

    async fn rotate_era(&mut self, _era: Era) {}

    async fn complete_era_transition(&mut self, _ended_era: Era) {}

    async fn dispatch(&self, payload: DataPayload, _era: Era) {
        drop(self.dispatched.send(payload));
    }

    async fn observe_broadcasts(&self) -> BoxStream<'static, DataPayload> {
        BroadcastStream::new(self.broadcasting_channel.subscribe())
            .filter_map(async |payload: Result<DataPayload, BroadcastStreamRecvError>| payload.ok())
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

    fn new(
        (): Self::Settings,
        _current_era: Era,
        _overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Self {
        Self
    }

    async fn process(&self, (): Self::Message) {}

    async fn rotate_era(&mut self, _era: Era) {}

    async fn complete_era_transition(&mut self, _ended_era: Era) {}

    async fn subscribe_to_pubsub(&mut self) -> BroadcastStream<Self::PubSubEvent> {
        BroadcastStream::new(broadcast::channel(CHANNEL_SIZE).0.subscribe())
    }

    async fn subscribe_to_chainsync(&mut self) -> BroadcastStream<Self::ChainSyncEvent> {
        BroadcastStream::new(broadcast::channel(CHANNEL_SIZE).0.subscribe())
    }
}
