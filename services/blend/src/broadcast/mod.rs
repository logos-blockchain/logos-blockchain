use core::{fmt::Debug, hash::Hash, marker::PhantomData, num::NonZeroU64, time::Duration};
use std::fmt::Display;

use async_trait::async_trait;
use futures::{Stream, StreamExt as _};
use lb_blend::scheduling::{
    epoch::{EpochEvent, EpochEventStream},
    stream::UninitializedFirstReadyStream,
};
use lb_chain_service::{Epoch, api::CryptarchiaServiceData};
use lb_cryptarchia_engine::{Era, EraSchedule};
use lb_key_management_system_service::{api::KmsServiceApi, keys::PublicKeyEncoding};
use lb_log_targets::blend;
use lb_network_service::NetworkService;
use lb_services_utils::wait_until_services_are_ready;
use lb_time_service::{EraTransitionEnd, TimeService, api::TimeServiceApi};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        resources::ServiceResourcesHandle,
        state::{NoOperator, NoState},
    },
};
use tracing::{debug, info};

use crate::{
    broadcast::settings::BroadcastServiceSettings,
    core::dispatcher::PayloadDispatcher,
    kms::PreloadKmsService,
    membership::{self, MembershipInfo, node_id},
    message::{NetworkInfo, ServiceMessage},
    mode::{Mode, ModeMembership},
};

pub mod service_components;
pub mod settings;

const LOG_TARGET: &str = blend::service::BROADCAST;

/// The Blend service in broadcast mode.
///
/// A node whose membership is too small to blend through still has payloads to
/// get onto the wire; this puts them there unblended and answers the same
/// queries the other modes answer. It used to be a plain struct owned by the
/// orchestrator while core and edge were services — so the subsystem was two
/// and a half services, and broadcast was the one mode that could not be
/// started, stopped or reasoned about like the others.
pub struct BlendService<NodeId, Dispatcher, TimeBackend, ChainService, RuntimeServiceId>
where
    Dispatcher: PayloadDispatcher<RuntimeServiceId>,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    #[expect(clippy::type_complexity, reason = "Marker field.")]
    _phantom: PhantomData<fn() -> (NodeId, Dispatcher, TimeBackend, ChainService)>,
}

impl<NodeId, Dispatcher, TimeBackend, ChainService, RuntimeServiceId> ServiceData
    for BlendService<NodeId, Dispatcher, TimeBackend, ChainService, RuntimeServiceId>
where
    Dispatcher: PayloadDispatcher<RuntimeServiceId>,
{
    type Settings = BroadcastServiceSettings<Dispatcher::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ServiceMessage<NodeId>;
}

#[async_trait]
impl<NodeId, Dispatcher, TimeBackend, ChainService, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for BlendService<NodeId, Dispatcher, TimeBackend, ChainService, RuntimeServiceId>
where
    NodeId: Clone + Debug + Eq + Hash + Send + Sync + node_id::TryFrom + 'static,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend + Send,
    ChainService: CryptarchiaServiceData<Tx: Send>,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>
        + AsServiceId<ChainService>
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
        + AsServiceId<NetworkService<Dispatcher::Backend, TimeBackend, RuntimeServiceId>>
        + AsServiceId<Dispatcher::MempoolService>
        + AsServiceId<Dispatcher::ChainNetworkService>
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

    #[expect(
        clippy::too_many_lines,
        reason = "The service sets up its components in sequence before running."
    )]
    async fn run(mut self) -> Result<(), DynError> {
        let Self {
            service_resources_handle:
                ServiceResourcesHandle {
                    ref mut inbound_relay,
                    ref overwatch_handle,
                    ref settings_handle,
                    ref status_updater,
                    ..
                },
            ..
        } = self;

        let BroadcastServiceSettings {
            non_ephemeral_signing_key_id,
            dispatcher: dispatcher_settings,
            era_schedule,
        } = settings_handle.notifier().get_updated_settings();

        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_mins(1)),
            NetworkService<_, _, _>,
            TimeService<_, _>,
            PreloadKmsService<_>,
            ChainService
        )
        .await?;

        let kms = KmsServiceApi::<PreloadKmsService<_>, RuntimeServiceId>::new(
            overwatch_handle.relay::<PreloadKmsService<_>>().await?,
        );
        let PublicKeyEncoding::Ed25519(signing_public_key) = kms
            .public_key(non_ephemeral_signing_key_id)
            .await
            .expect("KMS does not have key with the specified ID.")
        else {
            panic!("Non-ephemeral signing key must be an Ed25519 key");
        };
        let local_node_id =
            <NodeId as node_id::TryFrom>::try_from_provider_id(signing_public_key.as_bytes())
                .expect("non-ephemeral signing public key should decode into a valid node id");

        // No zk key: a broadcast node never mints a proof, so it has no use for
        // a Merkle path into the core tree.
        let membership_and_epoch_number_stream =
            membership::chain::subscribe::<ChainService, NodeId, TimeBackend, RuntimeServiceId>(
                overwatch_handle,
                signing_public_key,
                None,
                "blend_broadcast_service",
            )
            .await
            .map(|(epoch_state, membership_info)| (epoch_state.epoch, membership_info));
        let ((epoch, membership_info), remaining_membership_stream) =
            UninitializedFirstReadyStream::new(membership_and_epoch_number_stream)
                .first()
                .await
                .expect("The current epoch state must be ready");
        // The epoch the service starts in sets the era everything starts in.
        let current_era = era_schedule.at_epoch(epoch);

        let mut era_transition_ends =
            TimeServiceApi::<TimeService<TimeBackend, RuntimeServiceId>>::new(overwatch_handle)
                .await?
                .subscribe_to_era_transition_ends()
                .await?;

        let mut payload_dispatcher = <Dispatcher as PayloadDispatcher<RuntimeServiceId>>::new(
            overwatch_handle
                .relay::<NetworkService<_, _, _>>()
                .await
                .expect("Relay with network service should be available."),
            overwatch_handle
                .relay::<Dispatcher::MempoolService>()
                .await
                .expect("Relay with mempool service should be available."),
            overwatch_handle
                .relay::<Dispatcher::ChainNetworkService>()
                .await
                .expect("Relay with chain network service should be available."),
            dispatcher_settings,
            current_era.era,
        );
        let mut remaining_membership_stream = EpochEventStream::new(
            remaining_membership_stream,
            current_era.parameters.epoch_transition_period,
        );

        // Each epoch runs under the settings of its era.
        let minimum_network_sizes = era_schedule.map(|era| era.parameters.minimum_network_size);
        let current_epoch_minimum_network_size = *minimum_network_sizes.at_epoch(epoch).parameters;
        assert!(
            ModeMembership::resolve(membership_info, current_epoch_minimum_network_size).mode()
                == Mode::Broadcast,
            "The initial membership must satisfy the broadcast node condition."
        );

        status_updater.notify_ready();
        info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        run(
            inbound_relay,
            &mut remaining_membership_stream,
            &mut era_transition_ends,
            &mut payload_dispatcher,
            current_era.era,
            &local_node_id,
            &minimum_network_sizes,
        )
        .await;

        Ok(())
    }
}

/// Answers queries and puts payloads on the wire until the membership calls for
/// another mode.
///
/// The stopping condition is [`ModeMembership::resolve`] — the same function
/// the orchestrator uses to decide what to start, so the two cannot disagree
/// about what this node should be doing — with the minimum network size of
/// each epoch's era.
///
/// The payload dispatcher, in `era` when this starts, enters the era of every
/// epoch that opens one, and drops what it keeps of the era before when the
/// clock reports the end of its transition period.
async fn run<NodeId, Dispatcher, RuntimeServiceId>(
    inbound_relay: &mut (impl Stream<Item = ServiceMessage<NodeId>> + Send + Unpin),
    membership_stream: &mut (
             impl Stream<Item = EpochEvent<(Epoch, MembershipInfo<NodeId>)>> + Send + Unpin
         ),
    era_transition_ends: &mut (impl Stream<Item = EraTransitionEnd> + Send + Unpin),
    payload_dispatcher: &mut Dispatcher,
    mut era: Era,
    local_node_id: &NodeId,
    minimum_network_sizes: &EraSchedule<NonZeroU64>,
) where
    NodeId: Clone + Eq + Hash + Sync,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
{
    loop {
        tokio::select! {
            Some(message) = inbound_relay.next() => {
                handle_inbound_message(message, payload_dispatcher, local_node_id, era).await;
            }
            Some(epoch_event) = membership_stream.next() => {
                // A transition period expiring is not a mode change: there is
                // nothing draining here to expire.
                let EpochEvent::NewEpoch((epoch, membership_info)) = epoch_event else {
                    continue;
                };
                let epoch_era = minimum_network_sizes.at_epoch(epoch);
                if ModeMembership::resolve(membership_info, *epoch_era.parameters).mode() != Mode::Broadcast {
                    info!(target: LOG_TARGET, "New membership no longer calls for broadcast mode, shutting down.");
                    return;
                }
                if epoch_era.era > era {
                    era = epoch_era.era;
                    payload_dispatcher.rotate_era(era).await;
                }
            }
            Some(transition_end) = era_transition_ends.next() => {
                assert!(transition_end.era() < era, "Blend broadcast has not rotated before the old era's transition expired");
                payload_dispatcher.complete_era_transition(transition_end.era()).await;
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
    era: Era,
) where
    NodeId: Clone + Sync,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
{
    match message {
        // A broadcast node does no blending — it has no membership to blend
        // through — so the payload goes straight out, in the era in force.
        ServiceMessage::Blend(payload) => payload_dispatcher.dispatch(payload, era).await,
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
    use lb_network_service::{backends::NetworkBackend, message::BackendNetworkMsg};
    use overwatch::{overwatch::OverwatchHandle, services::relay::OutboundRelay};
    use tokio::sync::{mpsc, oneshot};
    use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};

    use super::*;
    use crate::{
        message::DataPayload,
        test_utils::{
            era::{schedule, single_era},
            membership::membership,
            mocks::{TestChainNetworkService, TestMempoolService},
        },
    };

    type NodeId = [u8; 32];
    const LOCAL: NodeId = [99; 32];
    const OTHER: NodeId = [1; 32];

    fn minimum(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).expect("test minimum is non-zero")
    }

    fn epoch(number: u32, members: &[NodeId]) -> EpochEvent<(Epoch, MembershipInfo<NodeId>)> {
        EpochEvent::NewEpoch((Epoch::new(number), membership(members, LOCAL).into()))
    }

    /// A broadcast node's only collaborator, recording what it was handed.
    struct RecordingDispatcher(mpsc::UnboundedSender<DataPayload>);

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
            _: OutboundRelay<BackendNetworkMsg<Self::Backend, RuntimeServiceId>>,
            _: OutboundRelay<<Self::MempoolService as ServiceData>::Message>,
            _: OutboundRelay<<Self::ChainNetworkService as ServiceData>::Message>,
            (): Self::Settings,
            _: Era,
        ) -> Self {
            unimplemented!("these tests construct the dispatcher directly")
        }

        async fn rotate_era(&mut self, _: Era) {}

        async fn complete_era_transition(&mut self, _: Era) {}

        async fn dispatch(&self, payload: DataPayload, _: Era) {
            self.0.send(payload).expect("receiver kept alive");
        }

        /// A broadcast node never waits on delivery: it is not blending, so
        /// there is nothing to detect the failure of.
        async fn observe_broadcasts(&self) -> BoxStream<'static, DataPayload> {
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

        fn new((): Self::Settings, _: Era, _: OverwatchHandle<RuntimeServiceId>) -> Self {
            Self
        }
        async fn process(&self, _: Self::Message) {}
        async fn rotate_era(&mut self, _: Era) {}
        async fn complete_era_transition(&mut self, _ended_era: Era) {}
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

        handle_inbound_message::<_, _, ()>(
            ServiceMessage::Blend(DataPayload::BlockProposal(b"proposal".to_vec())),
            &dispatcher,
            &LOCAL,
            Era::genesis(),
        )
        .await;
        assert_eq!(
            dispatched.recv().await.expect("a payload was dispatched"),
            DataPayload::BlockProposal(b"proposal".to_vec()),
            "a broadcast node puts a payload straight on the wire"
        );

        let (reply, response) = oneshot::channel();
        handle_inbound_message::<_, _, ()>(
            ServiceMessage::GetNetworkInfo { reply },
            &dispatcher,
            &LOCAL,
            Era::genesis(),
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
            Era::genesis(),
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
            .send(epoch(1, &[LOCAL, OTHER]))
            .await
            .expect("channel open");

        tokio::time::timeout(
            Duration::from_secs(5),
            run::<_, _, ()>(
                &mut ReceiverStream::new(inbound),
                &mut ReceiverStream::new(epochs),
                &mut stream::pending(),
                &mut RecordingDispatcher(dispatched_sender),
                Era::genesis(),
                &LOCAL,
                &single_era(minimum(2)),
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
            .send(epoch(1, &[OTHER]))
            .await
            .expect("channel open");

        let outcome = tokio::time::timeout(
            Duration::from_millis(200),
            run::<_, _, ()>(
                &mut ReceiverStream::new(inbound),
                &mut ReceiverStream::new(epochs),
                &mut stream::pending(),
                &mut RecordingDispatcher(dispatched_sender),
                Era::genesis(),
                &LOCAL,
                &single_era(minimum(2)),
            ),
        )
        .await;
        assert!(
            outcome.is_err(),
            "the membership still calls for broadcast, so the mode should still be running"
        );
    }

    /// Each epoch is judged by the minimum network size of its own era.
    #[test_log::test(tokio::test)]
    async fn an_epoch_is_judged_by_the_minimum_network_size_of_its_era() {
        let (_inbound_sender, inbound) = mpsc::channel(1);
        let (epoch_sender, epochs) = mpsc::channel(1);
        let (dispatched_sender, _dispatched) = mpsc::unbounded_channel();

        // Two members are too few to blend through in era 0, and enough in
        // era 1, from epoch 1.
        epoch_sender
            .send(epoch(1, &[LOCAL, OTHER]))
            .await
            .expect("channel open");

        tokio::time::timeout(
            Duration::from_secs(5),
            run::<_, _, ()>(
                &mut ReceiverStream::new(inbound),
                &mut ReceiverStream::new(epochs),
                &mut stream::pending(),
                &mut RecordingDispatcher(dispatched_sender),
                Era::genesis(),
                &LOCAL,
                &schedule(minimum(3), [(1, minimum(2))]),
            ),
        )
        .await
        .expect("the membership of epoch 1 calls for another mode under its era");
    }
}
