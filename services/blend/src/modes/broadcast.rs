use std::{
    fmt::{Debug, Display},
    marker::PhantomData,
    time::Duration,
};

use lb_network_service::message::BackendNetworkMsg;
use lb_services_utils::wait_until_services_are_ready;
use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, ServiceData},
};

use crate::{
    core::{dispatcher::PayloadDispatcher, service_components::MessageComponents},
    message::{BlendPayload, NetworkInfo},
    modes::Error,
};

pub struct BroadcastMode<Adapter, NodeId, RuntimeServiceId> {
    adapter: Adapter,
    node_id: NodeId,
    _phantom: PhantomData<(NodeId, RuntimeServiceId)>,
}

impl<Adapter, NodeId, RuntimeServiceId> BroadcastMode<Adapter, NodeId, RuntimeServiceId>
where
    Adapter: PayloadDispatcher<RuntimeServiceId> + Send + Sync,
{
    pub async fn new<NetworkService>(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
        node_id: NodeId,
        network_settings: Adapter::Settings,
    ) -> Result<Self, Error>
    where
        NetworkService:
            ServiceData<Message = BackendNetworkMsg<Adapter::Backend, RuntimeServiceId>>,
        RuntimeServiceId: AsServiceId<NetworkService>
            + AsServiceId<Adapter::MempoolService>
            + AsServiceId<Adapter::ChainNetworkService>
            + Debug
            + Display
            + Send
            + Sync
            + 'static,
    {
        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_mins(1)),
            NetworkService
        )
        .await?;
        let relay = overwatch_handle.relay::<NetworkService>().await?;
        let mempool_relay = overwatch_handle.relay::<Adapter::MempoolService>().await?;
        let chain_network_relay = overwatch_handle
            .relay::<Adapter::ChainNetworkService>()
            .await?;
        let adapter = Adapter::new(relay, mempool_relay, chain_network_relay, network_settings);
        Ok(Self {
            adapter,
            node_id,
            _phantom: PhantomData,
        })
    }
}

impl<Adapter, NodeId, RuntimeServiceId> BroadcastMode<Adapter, NodeId, RuntimeServiceId>
where
    Adapter: PayloadDispatcher<RuntimeServiceId> + Send + Sync + 'static,
    NodeId: Clone + Send + Sync,
    RuntimeServiceId: Send + Sync + 'static,
{
    pub async fn handle_inbound_message<Message>(&self, message: Message) -> Result<(), Error>
    where
        Message: MessageComponents<NodeId, Payload: Into<BlendPayload>> + Send + Sync + 'static,
    {
        match message.try_into_network_info_request() {
            Ok(reply) => {
                drop(reply.send(Some(NetworkInfo {
                    node_id: self.node_id.clone(),
                    core_info: None,
                })));
                Ok(())
            }
            // Nothing waits for a `PoW` solution here: a node in broadcast mode
            // does no blending, so a transaction goes straight to the mempool
            // instead of queueing for one.
            Err(message) => match message.try_into_pending_transactions_request() {
                Ok(reply) => {
                    drop(reply.send(Vec::new()));
                    Ok(())
                }
                Err(message) => {
                    // Forward directly to the dispatcher.
                    self.adapter.dispatch(message.into_payload().into()).await;
                    Ok(())
                }
            },
        }
    }
}

#[cfg(test)]
pub mod tests {
    use futures::{StreamExt as _, stream, stream::BoxStream};
    use lb_network_service::{NetworkService, backends::NetworkBackend, message::NetworkMsg};
    use overwatch::{
        DynError, OpaqueServiceResourcesHandle,
        overwatch::OverwatchRunner,
        services::{
            ServiceCore,
            relay::OutboundRelay,
            state::{NoOperator, NoState},
        },
    };
    use tokio::sync::{mpsc, oneshot};
    use tokio_stream::wrappers::BroadcastStream;
    use tracing::{debug, info};

    use super::*;
    use crate::test_utils::parked::{TestChainNetworkService, TestMempoolService};

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

            // Create the BroadcastMode
            let mut mode = BroadcastMode::<TestPayloadDispatcher, (), RuntimeServiceId>::new::<
                TestNetworkService,
            >(app.handle(), (), ())
            .await
            .unwrap();

            // Check if the mode broadcasts a message correctly.
            mode.handle_inbound_message(TestMessage(b"hello".to_vec()))
                .await
                .unwrap();
            assert_eq!(
                mode.adapter
                    .broadcasted_messages_receiver
                    .recv()
                    .await
                    .unwrap(),
                b"hello".to_vec()
            );

            // Check if the mode can be created again.
            let mut mode = BroadcastMode::<TestPayloadDispatcher, (), RuntimeServiceId>::new::<
                TestNetworkService,
            >(app.handle(), (), ())
            .await
            .unwrap();
            mode.handle_inbound_message(TestMessage(b"world".to_vec()))
                .await
                .unwrap();
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
        chain_network: TestChainNetworkService<RuntimeServiceId>,
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
        type ChainNetworkService = TestChainNetworkService<RuntimeServiceId>;
        type MempoolService = TestMempoolService<RuntimeServiceId>;
        type Settings = ();

        fn new(
            relay: OutboundRelay<
                <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
            >,
            _mempool_relay: OutboundRelay<<Self::MempoolService as ServiceData>::Message>,
            _chain_network_relay: OutboundRelay<
                <Self::ChainNetworkService as ServiceData>::Message,
            >,
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

        async fn observe_broadcasts(&self) -> BoxStream<'static, BlendPayload> {
            stream::empty().boxed()
        }
    }

    #[derive(Debug)]
    pub struct TestMessage(Vec<u8>);

    impl<NodeId> MessageComponents<NodeId> for TestMessage {
        type Payload = BlendPayload;

        fn into_payload(self) -> Self::Payload {
            BlendPayload::BlockProposal(self.0)
        }

        fn try_into_pending_transactions_request(
            self,
        ) -> Result<oneshot::Sender<Vec<Vec<u8>>>, Self>
        where
            Self: Sized,
        {
            Err(self)
        }

        fn try_into_network_info_request(
            self,
        ) -> Result<oneshot::Sender<Option<NetworkInfo<NodeId>>>, Self>
        where
            Self: Sized,
        {
            Err(self)
        }
    }

    fn settings() -> ServicesServiceSettings {
        ServicesServiceSettings {
            network: (),
            mempool: (),
            chain_network: (),
        }
    }
}
