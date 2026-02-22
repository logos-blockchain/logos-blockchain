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
use futures::{Stream, StreamExt as _};
use lb_blend::{
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
use lb_chain_service::{
    Epoch,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_core::codec::SerializeOp as _;
use lb_key_management_system_service::{
    api::KmsServiceApi, keys::KeyOperators,
    operators::ed25519::exfiltrate_secret_key::LeakSecretKeyOperator,
};
use lb_services_utils::wait_until_services_are_ready;
use lb_time_service::{SlotTick, TimeService, TimeServiceMessage};
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
use settings::StartingBlendConfig;
use tokio::sync::oneshot;
use tracing::{debug, error, info};

use crate::{
    edge::{
        handlers::{Error, MessageHandler},
        settings::RunningBlendConfig,
    },
    epoch_info::{
        ChainApi, EpochEvent, EpochHandler, LeaderInputsMinusQuota, PolEpochInfo,
        PolInfoProvider as PolInfoProviderTrait,
    },
    kms::PreloadKmsService,
    membership::{self, MembershipInfo},
    message::{NetworkMessage, ServiceMessage},
    settings::FIRST_STREAM_ITEM_READY_TIMEOUT,
};

const LOG_TARGET: &str = "blend::service::edge";

type RunningSettings<Backend, NodeId, RuntimeServiceId> =
    RunningBlendConfig<<Backend as BlendBackend<NodeId, RuntimeServiceId>>::Settings>;

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
    type Settings = StartingBlendConfig<Backend::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ServiceMessage<BroadcastSettings>;
}

#[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
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
    NodeId: Clone + Debug + Eq + Hash + Send + Sync + 'static,
    BroadcastSettings: Serialize + DeserializeOwned + Send,
    MembershipAdapter: membership::Adapter<NodeId = NodeId, Error: Send + Sync + 'static> + Send,
    membership::ServiceMessage<MembershipAdapter>: Send + Sync + 'static,
    ProofsGenerator: LeaderProofsGenerator + Send,
    TimeBackend: lb_time_service::backends::TimeBackend + Send,
    ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Send + Unpin + 'static> + Send,
    RuntimeServiceId: AsServiceId<<MembershipAdapter as membership::Adapter>::Service>
        + AsServiceId<Self>
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
        + AsServiceId<ChainService>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>
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
            <MembershipAdapter as membership::Adapter>::Service,
            PreloadKmsService<_>
        )
        .await?;

        let kms = KmsServiceApi::<PreloadKmsService<_>, RuntimeServiceId>::new(
            overwatch_handle.relay::<PreloadKmsService<_>>().await?,
        );

        // TODO: This will go once we do not need to pass the secret key anymore, i.e.,
        // when we have libp2p integration with KMS.
        let non_ephemeral_signing_key = {
            let (sender, receiver) = oneshot::channel();
            kms.execute(
                settings.non_ephemeral_signing_key_id,
                KeyOperators::Ed25519(Box::new(LeakSecretKeyOperator::new(sender))),
            )
            .await
            .expect("Failed to interact with KMS to fetch non-ephemeral signing key.");
            receiver
                .await
                .expect("Failed to retrieve non-ephemeral signing key from KMS.")
        };

        // Initialize membership stream for session and core-related public PoQ inputs.
        let session_stream = MembershipAdapter::new(
            overwatch_handle
                .relay::<<MembershipAdapter as membership::Adapter>::Service>()
                .await
                .expect("Failed to get relay channel with membership service."),
            non_ephemeral_signing_key.public_key(),
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
            RunningSettings::<Backend, _, _> {
                backend: settings.backend,
                cover: settings.cover,
                non_ephemeral_signing_key,
                num_blend_layers: settings.num_blend_layers,
                minimum_network_size: settings.minimum_network_size,
                time: settings.time,
                data_replication_factor: settings.data_replication_factor,
            },
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
#[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
async fn run<Backend, NodeId, ProofsGenerator, ChainService, PolInfoProvider, RuntimeServiceId>(
    session_stream: UninitializedSessionEventStream<
        impl Stream<Item = MembershipInfo<NodeId>> + Unpin,
    >,
    clock_stream: UninitializedFirstReadyStream<impl Stream<Item = SlotTick> + Unpin>,
    mut incoming_message_stream: impl Stream<Item = Vec<u8>> + Send + Unpin,
    mut epoch_handler: EpochHandler<ChainService, RuntimeServiceId>,
    settings: RunningSettings<Backend, NodeId, RuntimeServiceId>,
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    notify_ready: impl Fn(),
) -> Result<(), Error>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Sync + Send,
    NodeId: Clone + Debug + Eq + Hash + Send + Sync + 'static,
    ProofsGenerator: LeaderProofsGenerator + Send,
    ChainService: ChainApi<RuntimeServiceId> + Send + Sync,
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Unpin>,
    RuntimeServiceId: Clone + Send + Sync,
{
    let (mut current_membership_info, mut remaining_session_stream) = session_stream
        .await_first_ready()
        .await
        .expect("The current session info must be available.");

    info!(
        target: LOG_TARGET,
        "The current membership is ready: {:?}",
        current_membership_info
    );

    let ((current_epoch_info, current_epoch), mut remaining_clock_stream) = async {
        let (slot_tick, remaining_clock_stream) = clock_stream
            .first()
            .await
            .expect("The clock system must be available.");

        let EpochEvent::NewEpoch((
            LeaderInputsMinusQuota {
                pol_epoch_nonce,
                pol_ledger_aged,
                lottery_0,
                lottery_1,
            },
            epoch,
        )) = epoch_handler
            .tick(slot_tick)
            .await
            .expect("There should be new epoch state associated with the latest epoch state.")
        else {
            panic!("The first event expected by the epoch handler is a `NewEpoch` event.");
        };
        (
            (
                LeaderInputs {
                    message_quota: settings.session_leadership_quota(),
                    pol_epoch_nonce,
                    pol_ledger_aged,
                    lottery_0,
                    lottery_1,
                },
                epoch,
            ),
            remaining_clock_stream,
        )
    }
    .await;

    debug!(target: LOG_TARGET, "Current epoch info: {:?}", current_epoch_info);

    notify_ready();

    // A Blend edge service without the required secret `PoL` to generate proofs for
    // block proposals info is useless, hence we wait until the first secret PoL
    // info is made available. If an edge node has very little to no stake, this
    // `await` might hang for a long time, but that is fine, since that means there
    // will be no blocks to blend anyway.
    let (current_private_leader_info, mut remaining_secret_pol_info_stream) = async {
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

    debug!(target: LOG_TARGET, "Current secret leader info: {:?}", current_private_leader_info);

    let mut current_public_inputs = PoQVerificationInputsMinusSigningKey {
        core: CoreInputs {
            zk_root: current_membership_info
                .zk
                .expect("Membership should have ZK info")
                .root,
            quota: settings.cover.session_core_quota(
                settings.num_blend_layers,
                &settings.time,
                current_membership_info.membership.size(),
            ),
        },
        leader: current_epoch_info,
        session: current_membership_info.session_number,
    };

    let mut message_handler = Some(
        MessageHandler::<Backend, _, ProofsGenerator, _>::try_new_with_edge_condition_check(
            settings.clone(),
            current_membership_info.membership.clone(),
            current_public_inputs,
            current_private_leader_info.poq_private_inputs.clone(),
            overwatch_handle.clone(),
            current_epoch,
        )
        .expect("The initial membership should satisfy the edge node condition"),
    );

    loop {
        tokio::select! {
            Some(SessionEvent::NewSession(new_session_info)) = remaining_session_stream.next() => {
                match handle_new_session(new_session_info, settings.clone(), &current_private_leader_info.poq_private_inputs, overwatch_handle.clone(), current_public_inputs, current_epoch, message_handler) {
                    Ok((new_message_handler, new_public_inputs, new_membership_info)) => {
                        message_handler = Some(new_message_handler);
                        current_public_inputs = new_public_inputs;
                        current_membership_info = new_membership_info;
                    },
                    Err(Error::NetworkIsTooSmall(_)) => {
                        info!(target: LOG_TARGET, "New membership does not satisfy edge node condition, edge service shutting down.");
                        return Ok(());
                    }
                    Err(e) => {
                        error!(target: LOG_TARGET, "Error when handling new session: {e:?}, edge service shutting down.");
                        return Err(e);
                    }
                }
            }
            Some(message) = incoming_message_stream.next() => {
                let message_copies = settings.data_replication_factor.checked_add(1).unwrap();
                let handler = message_handler.as_mut().expect("Message handler should be available at the time a new message is propagated.");
                for _ in 0..message_copies {
                    handler.handle_message_to_blend(message.clone()).await;
                }
            }
            Some(clock_tick) = remaining_clock_stream.next() => {
                message_handler = handle_clock_event(clock_tick, &mut epoch_handler, message_handler).await;
            }
            Some(new_secret_pol_info) = remaining_secret_pol_info_stream.next() => {
                let (new_message_handler, new_public_inputs) = handle_new_secret_epoch_info(new_secret_pol_info, current_public_inputs, settings.clone(), overwatch_handle, &current_membership_info.membership, message_handler);
                message_handler = Some(new_message_handler);
                current_public_inputs = new_public_inputs;
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
    new_membership_info: MembershipInfo<NodeId>,
    settings: RunningSettings<Backend, NodeId, RuntimeServiceId>,
    current_epoch_private_info: &ProofOfLeadershipQuotaInputs,
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    current_public_inputs: PoQVerificationInputsMinusSigningKey,
    epoch: Epoch,
    // Unused, but we want to consume it.
    _current_message_handler: Option<
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    >,
) -> Result<
    (
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
        PoQVerificationInputsMinusSigningKey,
        MembershipInfo<NodeId>,
    ),
    Error,
>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone + Eq + Hash + Send + 'static,
    ProofsGenerator: LeaderProofsGenerator,
    RuntimeServiceId: Clone,
{
    let Some(zk_info) = &new_membership_info.zk else {
        return Err(Error::NetworkIsTooSmall(0));
    };
    debug!(target: LOG_TARGET, "Trying to create a new message handler");
    // Update current public inputs with new session info.
    let new_public_inputs = PoQVerificationInputsMinusSigningKey {
        session: new_membership_info.session_number,
        core: CoreInputs {
            quota: settings.cover.session_core_quota(
                settings.num_blend_layers,
                &settings.time,
                new_membership_info.membership.size(),
            ),
            zk_root: zk_info.root,
        },
        ..current_public_inputs
    };

    let new_handler = MessageHandler::try_new_with_edge_condition_check(
        settings,
        new_membership_info.membership.clone(),
        new_public_inputs,
        current_epoch_private_info.clone(),
        overwatch_handle,
        epoch,
    )?;

    Ok((new_handler, new_public_inputs, new_membership_info))
}

// If public info about a new epoch is available, then shut down the message
// handler until secret info for the same epoch is also available.
async fn handle_clock_event<Backend, NodeId, ProofsGenerator, ChainService, RuntimeServiceId>(
    slot_tick: SlotTick,
    epoch_handler: &mut EpochHandler<ChainService, RuntimeServiceId>,
    current_message_handler: Option<
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    >,
) -> Option<MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>>
where
    ChainService: ChainApi<RuntimeServiceId> + Send + Sync,
    RuntimeServiceId: Clone + Send + Sync,
{
    let Some(epoch_event) = epoch_handler.tick(slot_tick).await else {
        return current_message_handler;
    };

    let current_message_handler = current_message_handler?;

    // Disable the current message handler if a new epoch public info is received
    // before the secret info for the same epoch.
    match epoch_event {
        EpochEvent::NewEpoch((_, new_epoch))
        | EpochEvent::NewEpochAndOldEpochTransitionExpired((_, new_epoch))
            if new_epoch > current_message_handler.epoch() =>
        {
            debug!(target: LOG_TARGET, "New epoch detected: {epoch_event:?}, shutting down message handler until new secret PoL info is available.");
            None
        }
        // If it's not a new epoch event, or if the new epoch has already been processed when the
        // secret info was received, keep the current message handler.
        _ => Some(current_message_handler),
    }
}

/// Processes new secret `PoL` info.
///
/// In case the secret info is received before the public inputs, the message
/// handler is left unchanged. Else, a new message handler is created and
/// returned, that builds on the new epoch's public and private inputs.
fn handle_new_secret_epoch_info<Backend, NodeId, ProofsGenerator, RuntimeServiceId>(
    PolEpochInfo {
        epoch,
        poq_private_inputs,
        poq_public_inputs,
    }: PolEpochInfo,
    current_session_public_inputs: PoQVerificationInputsMinusSigningKey,
    settings: RunningSettings<Backend, NodeId, RuntimeServiceId>,
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    current_membership: &Membership<NodeId>,
    // Not used, but we want to consume it.
    _current_message_handler: Option<
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    >,
) -> (
    MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    PoQVerificationInputsMinusSigningKey,
)
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone + Eq + Hash + Send + 'static,
    ProofsGenerator: LeaderProofsGenerator,
    RuntimeServiceId: Clone,
{
    let new_session_public_inputs = PoQVerificationInputsMinusSigningKey {
        leader: LeaderInputs {
            lottery_0: poq_public_inputs.lottery_0,
            lottery_1: poq_public_inputs.lottery_1,
            pol_epoch_nonce: poq_public_inputs.epoch_nonce,
            pol_ledger_aged: poq_public_inputs.aged_root,
            message_quota: settings.session_leadership_quota(),
        },
        ..current_session_public_inputs
    };
    (
        MessageHandler::try_new_with_edge_condition_check(
            settings,
            current_membership.clone(),
            new_session_public_inputs,
            poq_private_inputs,
            overwatch_handle.clone(),
            epoch,
        ).expect("Should not fail to re-create message handler on epoch rotation after private inputs are set."),
        new_session_public_inputs,
    )
}
