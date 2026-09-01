use core::{fmt::Debug, hash::Hash, marker::PhantomData, time::Duration};
use std::fmt::Display;

use async_trait::async_trait;
use futures::{Stream, StreamExt as _};
use lb_blend::scheduling::epoch::{EpochEvent, UninitializedEpochEventStream};
use lb_chain_service::api::CryptarchiaServiceData;
use lb_key_management_system_service::{api::KmsServiceApi, keys::PublicKeyEncoding};
use lb_log_targets::blend;
use lb_network_service::NetworkService;
use lb_services_utils::wait_until_services_are_ready;
use lb_time_service::TimeService;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use tracing::{debug, info};

use crate::{
    broadcast::settings::StartingBlendConfig,
    core::dispatcher::PayloadDispatcher,
    kms::PreloadKmsService,
    membership::{self, MembershipInfo, chain::BlendEpochState, node_id},
    message::{NetworkInfo, ServiceMessage},
    mode::Mode,
};

pub mod settings;

const LOG_TARGET: &str = blend::service::BROADCAST;

pub trait Components<RuntimeServiceId> {
    /// How this node is identified in a membership.
    type NodeId;
    /// Where a payload goes. The only collaborator this mode really has.
    type Dispatcher;
    /// Where slot ticks come from, for the epoch stream.
    type TimeBackend;
    /// Where membership comes from, so the mode can tell when it should stop.
    type ChainService;
}

/// The Blend service in broadcast mode.
///
/// A node whose membership is too small to blend through still has payloads to
/// get onto the wire; this puts them there unblended and answers the same
/// queries the other modes answer. It used to be a plain struct owned by the
/// orchestrator while core and edge were services — so the subsystem was two
/// and a half services, and broadcast was the one mode that could not be
/// started, stopped or reasoned about like the others.
pub struct BlendService<C, RuntimeServiceId>
where
    C: Components<RuntimeServiceId, Dispatcher: PayloadDispatcher<RuntimeServiceId>>,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    _phantom: PhantomData<fn() -> C>,
}

impl<C, RuntimeServiceId> ServiceData for BlendService<C, RuntimeServiceId>
where
    C: Components<RuntimeServiceId, Dispatcher: PayloadDispatcher<RuntimeServiceId>>,
{
    type Settings =
        StartingBlendConfig<<C::Dispatcher as PayloadDispatcher<RuntimeServiceId>>::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ServiceMessage<C::NodeId>;
}

#[async_trait]
impl<C, RuntimeServiceId> ServiceCore<RuntimeServiceId> for BlendService<C, RuntimeServiceId>
where
    C: Components<
            RuntimeServiceId,
            NodeId: Clone + Debug + Eq + Hash + Send + Sync + node_id::TryFrom + 'static,
            Dispatcher: PayloadDispatcher<RuntimeServiceId> + Send + Sync,
            TimeBackend: lb_time_service::backends::TimeBackend + Send,
            ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
        > + Send
        + 'static,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>
        + AsServiceId<C::ChainService>
        + AsServiceId<TimeService<C::TimeBackend, RuntimeServiceId>>
        + AsServiceId<
            NetworkService<
                <C::Dispatcher as PayloadDispatcher<RuntimeServiceId>>::Backend,
                RuntimeServiceId,
            >,
        > + AsServiceId<<C::Dispatcher as PayloadDispatcher<RuntimeServiceId>>::MempoolService>
        + AsServiceId<<C::Dispatcher as PayloadDispatcher<RuntimeServiceId>>::ChainNetworkService>
        + Clone
        + Debug
        + Display
        + Send
        + Sync
        + Unpin
        + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self {
            service_resources_handle,
            _phantom: PhantomData,
        })
    }

    async fn run(mut self) -> Result<(), DynError> {
        let Self {
            service_resources_handle:
                OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                    ref mut inbound_relay,
                    ref overwatch_handle,
                    ref settings_handle,
                    ref status_updater,
                    ..
                },
            ..
        } = self;

        let settings = settings_handle.notifier().get_updated_settings();

        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_mins(1)),
            NetworkService<_, _>,
            TimeService<_, _>,
            PreloadKmsService<_>,
            C::ChainService
        )
        .await?;

        let payload_dispatcher = <C::Dispatcher as PayloadDispatcher<RuntimeServiceId>>::new(
            overwatch_handle
                .relay::<NetworkService<_, _>>()
                .await
                .expect("Relay with network service should be available."),
            overwatch_handle
                .relay::<<C::Dispatcher as PayloadDispatcher<RuntimeServiceId>>::MempoolService>()
                .await
                .expect("Relay with mempool service should be available."),
            overwatch_handle
                .relay::<<C::Dispatcher as PayloadDispatcher<RuntimeServiceId>>::ChainNetworkService>()
                .await
                .expect("Relay with chain network service should be available."),
            settings.network,
        );

        let kms = KmsServiceApi::<PreloadKmsService<_>, RuntimeServiceId>::new(
            overwatch_handle.relay::<PreloadKmsService<_>>().await?,
        );
        let PublicKeyEncoding::Ed25519(signing_public_key) = kms
            .public_key(settings.non_ephemeral_signing_key_id)
            .await
            .expect("KMS does not have key with the specified ID.")
        else {
            panic!("Non-ephemeral signing key must be an Ed25519 key");
        };
        let local_node_id =
            <C::NodeId as node_id::TryFrom>::try_from_provider_id(signing_public_key.as_bytes())
                .expect("non-ephemeral signing public key should decode into a valid node id");

        // No zk key: a broadcast node never mints a proof, so it has no use for
        // a Merkle path into the core tree.
        let membership_stream = membership::chain::subscribe::<
            C::ChainService,
            C::NodeId,
            C::TimeBackend,
            RuntimeServiceId,
        >(
            overwatch_handle,
            signing_public_key,
            None,
            "blend_broadcast_service",
        )
        .await
        .map(
            |BlendEpochState {
                 membership_info, ..
             }| membership_info,
        );
        let (_, mut remaining_membership_stream) = UninitializedEpochEventStream::new(
            membership_stream,
            settings.time.epoch_transition_period,
        )
        .await_first_ready()
        .await
        .expect("The current epoch state must be ready");

        status_updater.notify_ready();
        info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        run(
            inbound_relay,
            &mut remaining_membership_stream,
            &payload_dispatcher,
            &local_node_id,
            settings.minimum_network_size,
        )
        .await;

        Ok(())
    }
}

/// Answers queries and puts payloads on the wire until the membership calls for
/// another mode.
///
/// The stopping condition is [`Mode::choose`] — the same function the
/// orchestrator uses to decide what to start, so the two cannot disagree about
/// what this node should be doing.
async fn run<NodeId, Dispatcher, RuntimeServiceId>(
    inbound_relay: &mut (impl Stream<Item = ServiceMessage<NodeId>> + Send + Unpin),
    membership_stream: &mut (impl Stream<Item = EpochEvent<MembershipInfo<NodeId>>> + Send + Unpin),
    payload_dispatcher: &Dispatcher,
    local_node_id: &NodeId,
    minimum_network_size: core::num::NonZeroU64,
) where
    NodeId: Clone + Eq + Hash + Sync,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
{
    loop {
        tokio::select! {
            Some(message) = inbound_relay.next() => {
                handle_inbound_message(message, payload_dispatcher, local_node_id).await;
            }
            Some(epoch_event) = membership_stream.next() => {
                // A transition period expiring is not a mode change: there is
                // nothing draining here to expire.
                if let EpochEvent::NewEpoch(MembershipInfo { membership, .. }) = epoch_event
                    && Mode::choose(&membership, minimum_network_size) != Mode::Broadcast
                {
                    info!(target: LOG_TARGET, "New membership no longer calls for broadcast mode, shutting down.");
                    return;
                }
            }
            else => {
                debug!(target: LOG_TARGET, "All input streams terminated, broadcast service shutting down.");
                return;
            }
        }
    }
}

/// Answers a message from another service.
///
/// Infallible, and the three cases are matched exhaustively: a fourth
/// [`ServiceMessage`] variant is a compile error here rather than something
/// silently treated as a payload.
async fn handle_inbound_message<NodeId, Dispatcher, RuntimeServiceId>(
    message: ServiceMessage<NodeId>,
    payload_dispatcher: &Dispatcher,
    local_node_id: &NodeId,
) where
    NodeId: Clone + Sync,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
{
    match message {
        // A broadcast node does no blending — it has no membership to blend
        // through — so the payload goes straight out.
        ServiceMessage::Blend(payload) => payload_dispatcher.dispatch(payload).await,
        ServiceMessage::GetNetworkInfo { reply } => {
            drop(reply.send(Some(NetworkInfo {
                node_id: local_node_id.clone(),
                core_info: None,
            })));
        }
        // Nothing waits for a `PoW` solution here: a transaction goes straight
        // to the mempool instead of queueing for one, so there is never
        // anything pending to report.
        ServiceMessage::GetPendingTransactions { reply } => {
            drop(reply.send(Vec::new()));
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::{stream, stream::BoxStream};
    use lb_network_service::backends::NetworkBackend;
    use overwatch::{overwatch::OverwatchHandle, services::relay::OutboundRelay};
    use tokio::sync::{mpsc, oneshot};
    use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};

    use super::*;
    use crate::{
        message::BlendPayload,
        test_utils::{
            membership::membership,
            mocks::{TestChainNetworkService, TestMempoolService},
        },
    };

    type NodeId = [u8; 32];
    const LOCAL: NodeId = [99; 32];
    const OTHER: NodeId = [1; 32];

    fn minimum(n: u64) -> core::num::NonZeroU64 {
        core::num::NonZeroU64::new(n).expect("test minimum is non-zero")
    }

    fn epoch(members: &[NodeId]) -> EpochEvent<MembershipInfo<NodeId>> {
        EpochEvent::NewEpoch(MembershipInfo {
            membership: membership(members, LOCAL),
            zk: None,
        })
    }

    /// A broadcast node's only collaborator, recording what it was handed.
    struct RecordingDispatcher(mpsc::UnboundedSender<BlendPayload>);

    #[async_trait]
    impl<RuntimeServiceId> PayloadDispatcher<RuntimeServiceId> for RecordingDispatcher
    where
        RuntimeServiceId: Send + 'static,
    {
        type Backend = TestNetworkBackend;
        type MempoolService = TestMempoolService<RuntimeServiceId>;
        type ChainNetworkService = TestChainNetworkService<RuntimeServiceId>;
        type Settings = ();

        fn new(
            _: OutboundRelay<
                <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
            >,
            _: OutboundRelay<<Self::MempoolService as ServiceData>::Message>,
            _: OutboundRelay<<Self::ChainNetworkService as ServiceData>::Message>,
            (): Self::Settings,
        ) -> Self {
            unimplemented!("these tests construct the dispatcher directly")
        }

        async fn dispatch(&self, payload: BlendPayload) {
            self.0.send(payload).expect("receiver kept alive");
        }

        /// A broadcast node never waits on delivery: it is not blending, so
        /// there is nothing to detect the failure of.
        async fn observe_broadcasts(&self) -> BoxStream<'static, BlendPayload> {
            stream::empty().boxed()
        }
    }

    struct TestNetworkBackend;

    #[async_trait]
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

    /// All three message kinds are answered, including the two that the mode's
    /// predecessor never had a test for: its test double could not represent
    /// them.
    #[test_log::test(tokio::test)]
    async fn answers_every_message_kind() {
        let (dispatched_sender, mut dispatched) = mpsc::unbounded_channel();
        let dispatcher = RecordingDispatcher(dispatched_sender);

        PayloadDispatcher::<()>::dispatch(
            &dispatcher,
            BlendPayload::BlockProposal(b"proposal".to_vec()),
        )
        .await;
        assert_eq!(
            dispatched.recv().await.expect("a payload was dispatched"),
            BlendPayload::BlockProposal(b"proposal".to_vec()),
            "a broadcast node puts a payload straight on the wire"
        );

        let (reply, response) = oneshot::channel();
        handle_inbound_message::<_, _, ()>(
            ServiceMessage::GetNetworkInfo { reply },
            &dispatcher,
            &LOCAL,
        )
        .await;
        let info = response
            .await
            .unwrap()
            .expect("a broadcast node reports itself");
        assert_eq!(info.node_id, LOCAL);
        assert!(
            info.core_info.is_none(),
            "a broadcast node has no membership, so no core peers to report"
        );

        let (reply, response) = oneshot::channel();
        handle_inbound_message::<_, _, ()>(
            ServiceMessage::GetPendingTransactions { reply },
            &dispatcher,
            &LOCAL,
        )
        .await;
        assert!(
            response.await.unwrap().is_empty(),
            "nothing queues for a `PoW` solution here: a transaction goes straight out"
        );
    }

    /// The stopping condition is the shared rule, so the service stops exactly
    /// when the orchestrator would have started something else.
    #[test_log::test(tokio::test)]
    async fn stops_when_the_membership_calls_for_another_mode() {
        let (_inbound_sender, inbound) = mpsc::channel(1);
        let (epoch_sender, epochs) = mpsc::channel(1);
        let (dispatched_sender, _dispatched) = mpsc::unbounded_channel();

        // Big enough to blend through, and this node is in it: that is core.
        epoch_sender
            .send(epoch(&[LOCAL, OTHER]))
            .await
            .expect("channel open");

        tokio::time::timeout(
            Duration::from_secs(5),
            run::<_, _, ()>(
                &mut ReceiverStream::new(inbound),
                &mut ReceiverStream::new(epochs),
                &RecordingDispatcher(dispatched_sender),
                &LOCAL,
                minimum(2),
            ),
        )
        .await
        .expect("broadcast mode must stop once the membership calls for another mode");
    }

    /// And only then: a membership that still means broadcast is not a reason
    /// to stop.
    #[test_log::test(tokio::test)]
    async fn keeps_running_while_broadcast_is_still_the_mode() {
        let (_inbound_sender, inbound) = mpsc::channel(1);
        let (epoch_sender, epochs) = mpsc::channel(1);
        let (dispatched_sender, _dispatched) = mpsc::unbounded_channel();

        // Below the minimum, so still broadcast.
        epoch_sender
            .send(epoch(&[OTHER]))
            .await
            .expect("channel open");

        let outcome = tokio::time::timeout(
            Duration::from_millis(200),
            run::<_, _, ()>(
                &mut ReceiverStream::new(inbound),
                &mut ReceiverStream::new(epochs),
                &RecordingDispatcher(dispatched_sender),
                &LOCAL,
                minimum(2),
            ),
        )
        .await;
        assert!(
            outcome.is_err(),
            "the membership still calls for broadcast, so the mode should still be running"
        );
    }
}
