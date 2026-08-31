use std::{hash::Hash, marker::PhantomData};

use futures::{Stream, StreamExt as _};
use lb_blend::scheduling::epoch::EpochEvent;
use tracing::debug;

use crate::{
    core::dispatcher::PayloadDispatcher,
    membership::MembershipInfo,
    message::{NetworkInfo, ServiceMessage},
    modes::{LOG_TARGET, Mode},
};

pub struct BroadcastMode<Adapter, NodeId, RuntimeServiceId> {
    adapter: Adapter,
    node_id: NodeId,
    _phantom: PhantomData<fn() -> RuntimeServiceId>,
}

impl<Adapter, NodeId, RuntimeServiceId> BroadcastMode<Adapter, NodeId, RuntimeServiceId> {
    pub const fn new(adapter: Adapter, node_id: NodeId) -> Self {
        Self {
            adapter,
            node_id,
            _phantom: PhantomData,
        }
    }
}

impl<Adapter, NodeId, RuntimeServiceId> BroadcastMode<Adapter, NodeId, RuntimeServiceId>
where
    Adapter: PayloadDispatcher<RuntimeServiceId> + Send + Sync + 'static,
    NodeId: Clone + Send + Sync,
    RuntimeServiceId: Send + Sync + 'static,
{
    /// Answers a message from another service.
    pub async fn handle_inbound_message(&self, message: ServiceMessage<NodeId>) {
        match message {
            // A node in broadcast mode does no blending — it has no membership
            // to blend through — so the payload goes straight out.
            ServiceMessage::Blend(payload) => self.adapter.dispatch(payload).await,
            ServiceMessage::GetNetworkInfo { reply } => {
                drop(reply.send(Some(NetworkInfo {
                    node_id: self.node_id.clone(),
                    core_info: None,
                })));
            }
            // Nothing waits for a `PoW` solution here: a transaction goes
            // straight to the mempool instead of queueing for one, so there is
            // never anything pending to report.
            ServiceMessage::GetPendingTransactions { reply } => {
                drop(reply.send(Vec::new()));
            }
        }
    }
}

/// Runs the node for as long as it is a broadcast node, and reports the mode it
/// should switch to.
pub async fn run_broadcast_mode<Adapter, NodeId, RuntimeServiceId>(
    mode: BroadcastMode<Adapter, NodeId, RuntimeServiceId>,
    inbound_relay: &mut (impl Stream<Item = ServiceMessage<NodeId>> + Send + Unpin),
    epoch_stream: &mut (impl Stream<Item = EpochEvent<MembershipInfo<NodeId>>> + Send + Unpin),
    minimum_network_size: usize,
) -> Option<Mode>
where
    Adapter: PayloadDispatcher<RuntimeServiceId> + Send + Sync + 'static,
    NodeId: Clone + Eq + Hash + Send + Sync,
    RuntimeServiceId: Send + Sync + 'static,
{
    loop {
        tokio::select! {
            Some(message) = inbound_relay.next() => {
                mode.handle_inbound_message(message).await;
            }
            Some(epoch_event) = epoch_stream.next() => {
                // A transition period expiring is not a mode change: there is
                // nothing draining here to expire.
                if let EpochEvent::NewEpoch(MembershipInfo { membership, .. }) = epoch_event {
                    let next = Mode::choose(&membership, minimum_network_size);
                    if !matches!(next, Mode::Broadcast) {
                        return Some(next);
                    }
                }
            }
            else => {
                debug!(target: LOG_TARGET, "All input streams terminated, broadcast mode shutting down.");
                return None;
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use core::time::Duration;

    use lb_network_service::{
        NetworkService,
        backends::NetworkBackend,
        message::{BackendNetworkMsg, NetworkMsg},
    };
    use lb_services_utils::wait_until_services_are_ready;
    use overwatch::{
        DynError, OpaqueServiceResourcesHandle,
        overwatch::{OverwatchHandle, OverwatchRunner},
        services::{
            AsServiceId, ServiceCore, ServiceData,
            relay::OutboundRelay,
            state::{NoOperator, NoState},
        },
    };
    use tokio::sync::{mpsc, oneshot};
    use tokio_stream::wrappers::BroadcastStream;
    use tracing::{debug, info};

    use super::*;
    use crate::{message::BlendPayload, test_utils::mempool::TestMempoolService};

    // The supervisor builds one dispatcher and hands each mode its
    // own handle; this test does the same rather than having the mode
    // reach for relays itself.
    async fn dispatcher(handle: &OverwatchHandle<RuntimeServiceId>) -> TestPayloadDispatcher {
        <TestPayloadDispatcher as PayloadDispatcher<RuntimeServiceId>>::new(
            handle.relay::<TestNetworkService>().await.unwrap(),
            handle
                .relay::<TestMempoolService<RuntimeServiceId>>()
                .await
                .unwrap(),
            (),
        )
    }

    #[test_log::test(test)]
    fn broadcast_mode() {
        let app = OverwatchRunner::<Services>::run(settings(), None).unwrap();
        app.runtime().handle().block_on(async {
            // Start the network service first.
            app.handle().start_all_services().await.unwrap();
            wait_until_services_are_ready!(
                &app.handle(),
                Some(Duration::from_secs(5)),
                TestNetworkService
            )
            .await
            .unwrap();

            let mut mode =
                BroadcastMode::<_, (), RuntimeServiceId>::new(dispatcher(app.handle()).await, ());

            // Check if the mode broadcasts a message correctly.
            mode.handle_inbound_message(ServiceMessage::Blend(BlendPayload::BlockProposal(
                b"hello".to_vec(),
            )))
            .await;
            assert_eq!(
                mode.adapter
                    .broadcasted_messages_receiver
                    .recv()
                    .await
                    .unwrap(),
                b"hello".to_vec()
            );

            // A broadcast node still answers the two request messages, which
            // the old `try_into_*` shim left untested because the test message
            // could not represent them.
            let (reply, response) = oneshot::channel();
            mode.handle_inbound_message(ServiceMessage::GetNetworkInfo { reply })
                .await;
            let info = response
                .await
                .unwrap()
                .expect("a broadcast node reports itself");
            assert!(
                info.core_info.is_none(),
                "a broadcast node has no membership, so no core peers to report"
            );

            let (reply, response) = oneshot::channel();
            mode.handle_inbound_message(ServiceMessage::GetPendingTransactions { reply })
                .await;
            assert!(
                response.await.unwrap().is_empty(),
                "nothing queues for a `PoW` solution here: a transaction goes straight out"
            );

            // Check if the mode can be created again.
            let mut mode =
                BroadcastMode::<_, (), RuntimeServiceId>::new(dispatcher(app.handle()).await, ());
            mode.handle_inbound_message(ServiceMessage::Blend(BlendPayload::BlockProposal(
                b"world".to_vec(),
            )))
            .await;
            assert_eq!(
                mode.adapter
                    .broadcasted_messages_receiver
                    .recv()
                    .await
                    .unwrap(),
                b"world".to_vec()
            );
        });
    }

    #[overwatch::derive_services]
    struct Services {
        network: TestNetworkService,
        mempool: TestMempoolService<RuntimeServiceId>,
    }

    pub struct TestNetworkService {
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    }

    impl ServiceData for TestNetworkService {
        type Settings = ();
        type State = NoState<Self::Settings>;
        type StateOperator = NoOperator<Self::State>;
        type Message = BackendNetworkMsg<TestNetworkBackend, RuntimeServiceId>;
    }

    #[async_trait::async_trait]
    impl ServiceCore<RuntimeServiceId> for TestNetworkService {
        fn init(
            service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
            _: Self::State,
        ) -> Result<Self, DynError> {
            Ok(Self {
                service_resources_handle,
            })
        }

        async fn run(mut self) -> Result<(), DynError> {
            let Self {
                service_resources_handle:
                    OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                        ref mut inbound_relay,
                        ref status_updater,
                        ..
                    },
                ..
            } = self;

            let service_id = <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID;
            status_updater.notify_ready();
            info!("Service {service_id} is ready.",);

            while let Some(message) = inbound_relay.next().await {
                debug!("Service {service_id} received message: {message:?}");
            }

            Ok(())
        }
    }

    pub struct TestNetworkBackend;

    #[async_trait::async_trait]
    impl<RuntimeServiceId> NetworkBackend<RuntimeServiceId> for TestNetworkBackend {
        type Settings = ();
        type Message = Vec<u8>;
        type PubSubEvent = ();
        type ChainSyncEvent = ();

        fn new((): Self::Settings, _: OverwatchHandle<RuntimeServiceId>) -> Self {
            Self
        }

        async fn process(&self, _: Self::Message) {}

        async fn subscribe_to_pubsub(&mut self) -> BroadcastStream<Self::PubSubEvent> {
            unimplemented!()
        }

        async fn subscribe_to_chainsync(&mut self) -> BroadcastStream<Self::ChainSyncEvent> {
            unimplemented!()
        }
    }

    pub struct TestPayloadDispatcher {
        relay: OutboundRelay<
            <NetworkService<TestNetworkBackend, RuntimeServiceId> as ServiceData>::Message,
        >,
        broadcasted_messages_sender: mpsc::Sender<Vec<u8>>,
        broadcasted_messages_receiver: mpsc::Receiver<Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl<RuntimeServiceId> PayloadDispatcher<RuntimeServiceId> for TestPayloadDispatcher
    where
        RuntimeServiceId: Send + 'static,
    {
        type Backend = TestNetworkBackend;
        type MempoolService = TestMempoolService<RuntimeServiceId>;
        type Settings = ();

        fn new(
            relay: OutboundRelay<
                <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
            >,
            _mempool_relay: OutboundRelay<<Self::MempoolService as ServiceData>::Message>,
            (): Self::Settings,
        ) -> Self {
            let (broadcasted_messages_sender, broadcasted_messages_receiver) = mpsc::channel(100);
            Self {
                relay,
                broadcasted_messages_sender,
                broadcasted_messages_receiver,
            }
        }

        async fn dispatch(&self, payload: BlendPayload) {
            debug!("Dispatching payload: {payload:?}");
            let message = payload.body().to_vec();
            self.relay
                .send(NetworkMsg::Process(message.clone()))
                .await
                .unwrap();
            self.broadcasted_messages_sender
                .send(message)
                .await
                .unwrap();
        }
    }

    fn settings() -> ServicesServiceSettings {
        ServicesServiceSettings {
            network: (),
            mempool: (),
        }
    }
}
