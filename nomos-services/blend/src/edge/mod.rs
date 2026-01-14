pub mod backends;
mod handlers;
pub(crate) mod service_components;
pub mod settings;
#[cfg(test)]
mod tests;

use std::{
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    time::Duration,
};

use backends::BlendBackend;
use chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use futures::{Stream, StreamExt as _};
use nomos_blend::{
    message::crypto::proofs::PoQVerificationInputsMinusSigningKey,
    proofs::quota::inputs::prove::{
        private::ProofOfLeadershipQuotaInputs,
        public::{CoreInputs, LeaderInputs},
    },
    scheduling::{
        membership::Membership,
        message_blend::provers::leader::LeaderProofsGenerator,
        session::{SessionEvent, UninitializedSessionEventStream},
        stream::UninitializedFirstReadyStream,
    },
};
use nomos_core::codec::SerializeOp as _;
use nomos_time::{SlotTick, TimeService, TimeServiceMessage};
use overwatch::{
    OpaqueServiceResourcesHandle,
    overwatch::OverwatchHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        resources::ServiceResourcesHandle,
        state::{NoOperator, NoState},
    },
};
use serde::{Serialize, de::DeserializeOwned};
pub(crate) use service_components::ServiceComponents;
use services_utils::wait_until_services_are_ready;
use settings::BlendConfig;
use tokio::sync::oneshot;
use tracing::{debug, error, info};

use crate::{
    edge::handlers::{Error, MessageHandler},
    epoch_info::{
        ChainApi, EpochEvent, EpochHandler, LeaderInputsMinusQuota, PolEpochInfo,
        PolInfoProvider as PolInfoProviderTrait,
    },
    membership::{self, MembershipInfo, ZkInfo},
    message::{NetworkMessage, ServiceMessage},
    settings::FIRST_STREAM_ITEM_READY_TIMEOUT,
};

const LOG_TARGET: &str = "blend::service::edge";

type Settings<Backend, NodeId, RuntimeServiceId> =
    BlendConfig<<Backend as BlendBackend<NodeId, RuntimeServiceId>>::Settings>;

pub struct BlendService<
    Backend,
    NodeId,
    BroadcastSettings,
    MembershipAdapter,
    ProofsGenerator,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    _phantom: PhantomData<(
        MembershipAdapter,
        ProofsGenerator,
        TimeBackend,
        ChainService,
        PolInfoProvider,
    )>,
}

impl<
    Backend,
    NodeId,
    BroadcastSettings,
    MembershipAdapter,
    ProofsGenerator,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> ServiceData
    for BlendService<
        Backend,
        NodeId,
        BroadcastSettings,
        MembershipAdapter,
        ProofsGenerator,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone,
{
    type Settings = BlendConfig<Backend::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ServiceMessage<BroadcastSettings>;
}

#[async_trait::async_trait]
impl<
    Backend,
    NodeId,
    BroadcastSettings,
    MembershipAdapter,
    ProofsGenerator,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> ServiceCore<RuntimeServiceId>
    for BlendService<
        Backend,
        NodeId,
        BroadcastSettings,
        MembershipAdapter,
        ProofsGenerator,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Send + Sync,
    NodeId: Clone + Eq + Hash + Send + Sync + 'static,
    BroadcastSettings: Serialize + DeserializeOwned + Send,
    MembershipAdapter: membership::Adapter<NodeId = NodeId, Error: Send + Sync + 'static> + Send,
    membership::ServiceMessage<MembershipAdapter>: Send + Sync + 'static,
    ProofsGenerator: LeaderProofsGenerator + Send,
    TimeBackend: nomos_time::backends::TimeBackend + Send,
    ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Send + Unpin + 'static> + Send,
    RuntimeServiceId: AsServiceId<<MembershipAdapter as membership::Adapter>::Service>
        + AsServiceId<Self>
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
        + AsServiceId<ChainService>
        + Display
        + Debug
        + Clone
        + Send
        + Sync
        + Unpin
        + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, overwatch::DynError> {
        Ok(Self {
            service_resources_handle,
            _phantom: PhantomData,
        })
    }

    async fn run(mut self) -> Result<(), overwatch::DynError> {
        let Self {
            service_resources_handle:
                ServiceResourcesHandle {
                    inbound_relay,
                    overwatch_handle,
                    settings_handle,
                    status_updater,
                    ..
                },
            ..
        } = self;

        let settings = settings_handle.notifier().get_updated_settings();

        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_secs(60)),
            TimeService<_, _>,
            <MembershipAdapter as membership::Adapter>::Service
        )
        .await?;

        // Initialize membership stream for session and core-related public PoQ inputs.
        let session_stream = MembershipAdapter::new(
            overwatch_handle
                .relay::<<MembershipAdapter as membership::Adapter>::Service>()
                .await
                .expect("Failed to get relay channel with membership service."),
            settings.crypto.non_ephemeral_signing_key.public_key(),
            // No ZK stuff needs to be computed by edge nodes, so no ZK key is specified here.
            None,
        )
        .subscribe()
        .await
        .expect("Failed to get membership stream from membership service.");

        // Initialize clock stream for epoch-related public PoQ inputs.
        let clock_stream = async {
            let time_relay = overwatch_handle
                .relay::<TimeService<_, _>>()
                .await
                .expect("Relay with time service should be available.");
            let (sender, receiver) = oneshot::channel();
            time_relay
                .send(TimeServiceMessage::Subscribe { sender })
                .await
                .expect("Failed to subscribe to slot clock.");
            receiver
                .await
                .expect("Should not fail to receive slot stream from time service.")
        }
        .await;

        let messages_to_blend_stream = inbound_relay.map(|ServiceMessage::Blend(message)| {
            NetworkMessage::<BroadcastSettings>::to_bytes(&message)
                .expect("NetworkMessage should be able to be serialized")
                .to_vec()
        });

        let epoch_handler = async {
            let chain_service = CryptarchiaServiceApi::<ChainService, _>::new(
                overwatch_handle
                    .relay::<ChainService>()
                    .await
                    .expect("Failed to establish channel with chain service."),
            );
            EpochHandler::new(
                chain_service,
                settings.time.epoch_transition_period_in_slots,
            )
        }
        .await;

        run::<Backend, _, ProofsGenerator, _, PolInfoProvider, _>(
            UninitializedSessionEventStream::new(
                session_stream,
                FIRST_STREAM_ITEM_READY_TIMEOUT,
                settings.time.session_transition_period(),
            ),
            UninitializedFirstReadyStream::new(clock_stream, FIRST_STREAM_ITEM_READY_TIMEOUT),
            messages_to_blend_stream,
            epoch_handler,
            &settings,
            &overwatch_handle,
            || {
                status_updater.notify_ready();
                info!(
                    target: LOG_TARGET,
                    "Service '{}' is ready.",
                    <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
                );
            },
        )
        .await
        .map_err(|e| {
            error!(target: LOG_TARGET, "Edge blend service is being terminated with error: {e:?}");
            e.into()
        })
    }
}

/// Run the event loop of the service.
///
/// It listens for new sessions and messages to blend.
/// It recreates the [`MessageHandler`] on each new session to handle messages
/// with the new membership.
/// It returns an [`Error`] if the new membership does not satisfy the edge node
/// condition.
///
/// # Panics
/// - If the initial membership is not yielded immediately from the session
///   stream.
/// - If the initial membership does not satisfy the edge node condition.
/// - If the initial epoch public info is not yielded immediately by the epoch
///   handler.
/// - If the initial secret `PoL` info is not yielded immediately by the `PoL`
///   info provider.
async fn run<Backend, NodeId, ProofsGenerator, ChainService, PolInfoProvider, RuntimeServiceId>(
    session_stream: UninitializedSessionEventStream<
        impl Stream<Item = MembershipInfo<NodeId>> + Unpin,
    >,
    clock_stream: UninitializedFirstReadyStream<impl Stream<Item = SlotTick> + Unpin>,
    mut incoming_message_stream: impl Stream<Item = Vec<u8>> + Send + Unpin,
    mut epoch_handler: EpochHandler<ChainService, RuntimeServiceId>,
    settings: &Settings<Backend, NodeId, RuntimeServiceId>,
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    notify_ready: impl Fn(),
) -> Result<(), Error>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Sync + Send,
    NodeId: Clone + Eq + Hash + Send + Sync + 'static,
    ProofsGenerator: LeaderProofsGenerator + Send,
    ChainService: ChainApi<RuntimeServiceId> + Send + Sync,
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Unpin>,
    RuntimeServiceId: Clone + Send + Sync,
{
    let (current_membership_info, mut remaining_session_stream) = session_stream
        .await_first_ready()
        .await
        .expect("The current session info must be available.");

    let (current_epoch_info, mut remaining_clock_stream) = async {
        let (slot_tick, remaining_clock_stream) = clock_stream
            .first()
            .await
            .expect("The clock system must be available.");

        let EpochEvent::NewEpoch(LeaderInputsMinusQuota {
            pol_epoch_nonce,
            pol_ledger_aged,
            total_stake,
        }) = epoch_handler
            .tick(slot_tick)
            .await
            .expect("There should be new epoch state associated with the latest epoch state.")
        else {
            panic!("The first event expected by the epoch handler is a `NewEpoch` event.");
        };
        (
            LeaderInputs {
                message_quota: settings.crypto.num_blend_layers.into(),
                pol_epoch_nonce,
                pol_ledger_aged,
                total_stake,
            },
            remaining_clock_stream,
        )
    }
    .await;

    info!(
        target: LOG_TARGET,
        "The current membership is ready: {} nodes.",
        current_membership_info.membership.size()
    );

    notify_ready();

    // A Blend edge service without the required secret `PoL` to generate proofs for
    // block proposals info is useless, hence we wait until the first secret PoL
    // info is made available. If an edge node has very little to no stake, this
    // `await` might hang for a long time, but that is fine, since that means there
    // will be no blocks to blend anyway.
    let (mut current_private_leader_info, mut remaining_secret_pol_info_stream) = async {
        // There might be services that depend on Blend to be ready before starting, so
        // we cannot wait for the stream to be sent before we signal we are
        // ready, hence this should always be called after `notify_ready();`.
        // Also, Blend services start even if such a stream is not immediately
        // available, since they will simply keep blending cover messages.
        let mut secret_pol_info_stream = PolInfoProvider::subscribe(overwatch_handle)
            .await
            .expect("Should not fail to subscribe to secret PoL info stream.");
        (
            secret_pol_info_stream
                .next()
                .await
                .expect("Secret PoL info stream should always return `Some` value."),
            secret_pol_info_stream,
        )
    }
    .await;

    let mut current_public_inputs = PoQVerificationInputsMinusSigningKey {
        core: CoreInputs {
            zk_root: current_membership_info.zk.root,
            quota: settings.cover.session_quota(
                &settings.crypto,
                &settings.time,
                current_membership_info.membership.size(),
            ),
        },
        leader: current_epoch_info,
        session: current_membership_info.session_number,
    };

    let mut message_handler =
        MessageHandler::<Backend, _, ProofsGenerator, _>::try_new_with_edge_condition_check(
            settings,
            current_membership_info.membership.clone(),
            current_public_inputs,
            current_private_leader_info.poq_private_inputs.clone(),
            overwatch_handle.clone(),
        )
        .expect("The initial membership should satisfy the edge node condition");

    loop {
        tokio::select! {
            Some(SessionEvent::NewSession(new_session_info)) = remaining_session_stream.next() => {
              let (new_message_handler, new_public_inputs) = handle_new_session(new_session_info, settings, current_private_leader_info.poq_private_inputs.clone(), overwatch_handle.clone(), current_public_inputs, message_handler)?;
              message_handler = new_message_handler;
              current_public_inputs = new_public_inputs;
            }
            Some(message) = incoming_message_stream.next() => {
                message_handler.handle_messages_to_blend(message).await;
            }
            Some(clock_tick) = remaining_clock_stream.next() => {
                let (new_message_handler, new_public_inputs) = handle_clock_event(clock_tick, settings, &current_private_leader_info, overwatch_handle, &current_membership_info.membership, &mut epoch_handler, message_handler, current_public_inputs).await;
                message_handler = new_message_handler;
                current_public_inputs = new_public_inputs;
            }
            Some(new_secret_pol_info) = remaining_secret_pol_info_stream.next() => {
                let (new_message_handler, new_private_leader_info) = handle_new_secret_epoch_info(new_secret_pol_info, settings, &current_public_inputs, overwatch_handle, &current_membership_info.membership, message_handler);
                message_handler = new_message_handler;
                current_private_leader_info = new_private_leader_info;
            }
        }
    }
}

/// Handle a new session.
///
/// It creates a new [`MessageHandler`] and new `PoQ` public inputs if the
/// membership satisfies all the edge node condition. Otherwise, it returns
/// [`Error`].
#[expect(
    clippy::type_complexity,
    reason = "There are too many generics. Any type alias would be as complicated."
)]
fn handle_new_session<Backend, NodeId, ProofsGenerator, RuntimeServiceId>(
    MembershipInfo {
        membership: new_membership,
        session_number: new_session_number,
        zk: ZkInfo {
            root: new_zk_root, ..
        },
    }: MembershipInfo<NodeId>,
    settings: &Settings<Backend, NodeId, RuntimeServiceId>,
    current_epoch_private_info: ProofOfLeadershipQuotaInputs,
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    current_public_inputs: PoQVerificationInputsMinusSigningKey,
    // Unused, but we want to consume it.
    _current_message_handler: MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
) -> Result<
    (
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
        PoQVerificationInputsMinusSigningKey,
    ),
    Error,
>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone + Eq + Hash + Send + 'static,
    ProofsGenerator: LeaderProofsGenerator,
    RuntimeServiceId: Clone,
{
    debug!(target: LOG_TARGET, "Trying to create a new message handler");
    // Update current public inputs with new session info.
    let new_public_inputs = PoQVerificationInputsMinusSigningKey {
        session: new_session_number,
        core: CoreInputs {
            quota: settings.cover.session_quota(
                &settings.crypto,
                &settings.time,
                new_membership.size(),
            ),
            zk_root: new_zk_root,
        },
        ..current_public_inputs
    };

    let new_handler = MessageHandler::try_new_with_edge_condition_check(
        settings,
        new_membership,
        new_public_inputs,
        current_epoch_private_info,
        overwatch_handle,
    )?;

    Ok((new_handler, new_public_inputs))
}

#[expect(
    clippy::too_many_arguments,
    reason = "TODO: Address this at some point."
)]
/// It fetches the new epoch state if a new epoch tick is provided, which will
/// result in new `PoQ` public inputs. A new handler is also created if the
/// secret `PoL` info for the same epoch has already been provided by the
/// `PoLInfoProvider`.
///
/// If the slot tick does not belong to a new, unprocessed epoch, the old
/// handler and inputs are returned instead.
/// In case this is called before the secret info is obtained, only the public
/// inputs are updated, and the message handler will be updated once the secret
/// info is received.
async fn handle_clock_event<Backend, NodeId, ProofsGenerator, ChainService, RuntimeServiceId>(
    slot_tick: SlotTick,
    settings: &Settings<Backend, NodeId, RuntimeServiceId>,
    PolEpochInfo {
        nonce: current_secret_inputs_nonce,
        poq_private_inputs: current_secret_inputs,
    }: &PolEpochInfo,
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    current_membership: &Membership<NodeId>,
    epoch_handler: &mut EpochHandler<ChainService, RuntimeServiceId>,
    current_message_handler: MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    current_public_inputs: PoQVerificationInputsMinusSigningKey,
) -> (
    MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    PoQVerificationInputsMinusSigningKey,
)
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Send,
    NodeId: Clone + Eq + Hash + Send + Sync + 'static,
    ProofsGenerator: LeaderProofsGenerator + Send,
    ChainService: ChainApi<RuntimeServiceId> + Send + Sync,
    RuntimeServiceId: Clone + Send + Sync,
{
    let Some(epoch_event) = epoch_handler.tick(slot_tick).await else {
        return (current_message_handler, current_public_inputs);
    };

    let new_leader_inputs = match epoch_event {
        EpochEvent::NewEpoch(LeaderInputsMinusQuota {
            pol_epoch_nonce,
            pol_ledger_aged,
            total_stake,
        })
        | EpochEvent::NewEpochAndOldEpochTransitionExpired(LeaderInputsMinusQuota {
            pol_epoch_nonce,
            pol_ledger_aged,
            total_stake,
        }) => LeaderInputs {
            message_quota: settings.crypto.num_blend_layers.into(),
            pol_epoch_nonce,
            pol_ledger_aged,
            total_stake,
        },
        // We don't handle the epoch transitions in edge node.
        EpochEvent::OldEpochTransitionPeriodExpired => current_public_inputs.leader,
    };

    let new_public_inputs = PoQVerificationInputsMinusSigningKey {
        leader: new_leader_inputs,
        ..current_public_inputs
    };

    // If the private info for the new epoch came in before, create a new message
    // handler with the new info.
    let new_message_handler = if new_public_inputs.leader.pol_epoch_nonce
        == *current_secret_inputs_nonce
    {
        MessageHandler::try_new_with_edge_condition_check(
            settings,
            current_membership.clone(),
            new_public_inputs,
            current_secret_inputs.clone(),
            overwatch_handle.clone(),
        )
        .expect("Should not fail to re-create message handler on epoch rotation after public inputs are set.")
    } else {
        current_message_handler
    };

    (new_message_handler, new_public_inputs)
}

/// Processes new secret `PoL` info.
///
/// In case the secret info is received before the public inputs, the message
/// handler is left unchanged. Else, a new message handler is created and
/// returned, that builds on the new epoch's public and private inputs.
fn handle_new_secret_epoch_info<Backend, NodeId, ProofsGenerator, RuntimeServiceId>(
    new_pol_info: PolEpochInfo,
    settings: &Settings<Backend, NodeId, RuntimeServiceId>,
    current_public_inputs: &PoQVerificationInputsMinusSigningKey,
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    current_membership: &Membership<NodeId>,
    current_message_handler: MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
) -> (
    MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    PolEpochInfo,
)
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone + Eq + Hash + Send + 'static,
    ProofsGenerator: LeaderProofsGenerator,
    RuntimeServiceId: Clone,
{
    // If the public info for the new epoch came in before, create a new message
    // handler with the new info.
    let new_handler = if current_public_inputs.leader.pol_epoch_nonce == new_pol_info.nonce {
        MessageHandler::try_new_with_edge_condition_check(
            settings,
            current_membership.clone(),
            *current_public_inputs,
            new_pol_info.poq_private_inputs.clone(),
            overwatch_handle.clone(),
        )
        .expect("Should not fail to re-create message handler on epoch rotation after private inputs are set.")
    } else {
        current_message_handler
    };

    (new_handler, new_pol_info)
}
