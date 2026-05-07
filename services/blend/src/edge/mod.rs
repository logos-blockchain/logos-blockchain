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
    proofs::quota::inputs::prove::public::{CoreInputs, LeaderInputs},
    scheduling::{
        message_blend::provers::leader::LeaderProofsGenerator,
        session::{SessionEvent, UninitializedSessionEventStream},
    },
};
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::codec::SerializeOp as _;
use lb_key_management_system_service::{
    api::KmsServiceApi, keys::KeyOperators,
    operators::ed25519::exfiltrate_secret_key::LeakSecretKeyOperator,
};
use lb_log_targets::blend;
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
        ChainApi, EpochEvent, EpochHandler, PolEpochInfo, PolInfoProvider as PolInfoProviderTrait,
    },
    kms::PreloadKmsService,
    membership::{self, MembershipInfo, node_id},
    message::{NetworkInfo, NetworkMessage, ServiceMessage},
    settings::FIRST_STREAM_ITEM_READY_TIMEOUT,
};

const LOG_TARGET: &str = blend::service::EDGE;

type RunningSettings<Backend, NodeId, RuntimeServiceId> =
    RunningBlendConfig<<Backend as BlendBackend<NodeId, RuntimeServiceId>>::Settings>;

type EpochInfoAndHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId> = (
    PolEpochInfo,
    MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
);

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
    type Message = ServiceMessage<BroadcastSettings, NodeId>;
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
    NodeId: Clone + Debug + Eq + Hash + Send + Sync + node_id::TryFrom + 'static,
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
            Some(Duration::from_mins(1)),
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
        let local_node_id =
            NodeId::try_from_provider_id(&non_ephemeral_signing_key.public_key().to_bytes())
                .expect("non-ephemeral signing key should decode into a valid node id");

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

        // Initialize clock stream for detecting epoch transitions.
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

        let messages_to_blend_stream = Box::pin(inbound_relay.filter_map(async |msg| {
            match msg {
                ServiceMessage::Blend(message) => Some(
                    NetworkMessage::<BroadcastSettings>::to_bytes(&message)
                        .expect("NetworkMessage should be able to be serialized")
                        .to_vec(),
                ),
                ServiceMessage::GetNetworkInfo { reply } => {
                    drop(reply.send(Some(NetworkInfo {
                        node_id: local_node_id.clone(),
                        core_info: None,
                    })));
                    None
                }
            }
        }));

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
            clock_stream,
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
/// The event loop handles three types of events:
/// - **Session changes**: resets the message handler with the new session but
///   the current epoch info. If the handler was shut down (waiting for secret
///   epoch info), it stays shut down.
/// - **Clock ticks (epoch transitions)**: on a new epoch, shuts down the
///   message handler until secret `PoL` info for that epoch is received. If
///   secret info was already provided for the new epoch, the handler is kept.
/// - **Secret `PoL` info**: always (re)creates the message handler with the new
///   epoch's public and private inputs, preserving the current session.
///
/// Returns an [`Error`] if a new membership does not satisfy the edge node
/// condition.
///
/// # Panics
/// - If the initial membership is not yielded immediately from the session
///   stream.
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
async fn run<Backend, NodeId, ProofsGenerator, ChainService, PolInfoProvider, RuntimeServiceId>(
    session_stream: UninitializedSessionEventStream<
        impl Stream<Item = MembershipInfo<NodeId>> + Unpin,
    >,
    mut clock_stream: impl Stream<Item = SlotTick> + Unpin,
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
        session = current_membership_info.session_number,
        members = current_membership_info.membership.size(),
        local_node_index = current_membership_info.membership.local_index(),
        has_zk = current_membership_info.zk.is_some(),
        "current membership is ready"
    );

    notify_ready();

    // No need to wait for the PoL stream to return an element. We just move on and
    // will have a `None` handler until secret info for an epoch is passed to this
    // service.
    let mut secret_pol_info_stream = PolInfoProvider::subscribe(overwatch_handle)
        .await
        .expect("Should not fail to subscribe to secret PoL info stream.");

    let mut current_pol_info_and_message_handler: Option<(
        PolEpochInfo,
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    )> = None;

    loop {
        tokio::select! {
            Some(SessionEvent::NewSession(new_session_info)) = remaining_session_stream.next() => {
                match handle_new_session(&new_session_info, settings.clone(), &mut current_pol_info_and_message_handler, overwatch_handle.clone()) {
                    Err(Error::NetworkIsTooSmall(_)) => {
                        info!(target: LOG_TARGET, "New membership does not satisfy edge node condition, edge service shutting down.");
                        return Ok(());
                    }
                    Err(e) => {
                        error!(target: LOG_TARGET, "Error when handling new session: {e:?}, edge service shutting down.");
                        return Err(e);
                    }
                    Ok(()) => {
                        // We need to keep track of this for now because message handlers are initialized with a membership info. Exposing a simple `rotate_epoch` will allow us to avoid tracking this value here.
                        current_membership_info = new_session_info;
                    }
                }
            }
            Some(message) = incoming_message_stream.next() => {
                // TODO: Investigate why secret PoL info at times arrives after the block proposal.
                let Some(handler) = current_pol_info_and_message_handler.as_mut().map(|(_, handler)| handler) else {
                    tracing::warn!(target: LOG_TARGET, "Received a message to blend, but no active message handler is available to process it because the secret PoL info for the current epoch is not yet available. Ignoring the message.");
                    continue;
                };
                let message_copies = settings.data_replication_factor.checked_add(1).unwrap();
                for _ in 0..message_copies {
                    handler.handle_message_to_blend(message.clone()).await;
                }
            }
            Some(clock_tick) = clock_stream.next() => {
                handle_clock_event(clock_tick, &mut epoch_handler, &mut current_pol_info_and_message_handler).await;
            }
            Some(new_secret_pol_info) = secret_pol_info_stream.next() => {
                handle_new_secret_epoch_info(&new_secret_pol_info, settings.clone(), overwatch_handle, &current_membership_info, &mut current_pol_info_and_message_handler);
            }
        }
    }
}

/// Handle a new session.
///
/// If the message handler was active, it is recreated with the new session's
/// membership and core info, preserving the current epoch's leader inputs and
/// private inputs. If it was `None` (no secret epoch info yet, or shut down
/// after an epoch transition), it stays `None` — only the membership info
/// tracked by the caller is updated for when the handler is later recreated
/// by [`handle_new_secret_epoch_info`].
///
/// Returns [`Error`] if the new membership does not satisfy the edge node
/// condition.
#[expect(
    clippy::type_complexity,
    reason = "There are too many generics. Any type alias would be as complicated."
)]
fn handle_new_session<Backend, NodeId, ProofsGenerator, RuntimeServiceId>(
    new_membership_info: &MembershipInfo<NodeId>,
    settings: RunningSettings<Backend, NodeId, RuntimeServiceId>,
    current_epoch_info_and_message_handler: &mut Option<(
        PolEpochInfo,
        MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    )>,
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
) -> Result<(), Error>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone + Eq + Hash + Send + 'static,
    ProofsGenerator: LeaderProofsGenerator,
    RuntimeServiceId: Clone,
{
    let Some(zk_info) = &new_membership_info.zk else {
        return Err(Error::NetworkIsTooSmall(0));
    };
    debug!(target: LOG_TARGET, "New session received, trying to create a new message handler");

    // Update session and core public inputs, preserving the current epoch's
    // leader inputs.
    let Some((current_epoch_private_info, _)) = current_epoch_info_and_message_handler.take()
    else {
        debug!(target: LOG_TARGET, "No current epoch private info available. Ignoring new session event to create a new message handler.");
        return Ok(());
    };

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
        leader: LeaderInputs {
            lottery_0: current_epoch_private_info.poq_public_inputs.lottery_0,
            lottery_1: current_epoch_private_info.poq_public_inputs.lottery_1,
            pol_epoch_nonce: current_epoch_private_info.poq_public_inputs.epoch_nonce,
            pol_ledger_aged: current_epoch_private_info.poq_public_inputs.aged_root,
            message_quota: settings.session_leadership_quota(),
        },
    };

    let new_handler = MessageHandler::try_new_with_edge_condition_check(
        settings,
        new_membership_info.membership.clone(),
        new_public_inputs,
        current_epoch_private_info.poq_private_inputs.clone(),
        overwatch_handle,
        current_epoch_private_info.epoch,
    )?;

    *current_epoch_info_and_message_handler = Some((current_epoch_private_info, new_handler));

    Ok(())
}

/// Handles a clock tick by forwarding it to the epoch handler.
///
/// If the tick reveals a new epoch that is ahead of the last received secret
/// `PoL` info (`current_epoch`), the message handler is shut down until
/// [`handle_new_secret_epoch_info`] provides the secret info for the new epoch.
/// If secret info was already received for the new epoch, or if the handler was
/// already `None`, it is left unchanged.
async fn handle_clock_event<Backend, NodeId, ProofsGenerator, ChainService, RuntimeServiceId>(
    slot_tick: SlotTick,
    epoch_handler: &mut EpochHandler<ChainService, RuntimeServiceId>,
    current_epoch_info_and_message_handler: &mut Option<
        EpochInfoAndHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    >,
) where
    ChainService: ChainApi<RuntimeServiceId> + Send + Sync,
    RuntimeServiceId: Clone + Send + Sync,
{
    let Some(epoch_event) = epoch_handler.tick(slot_tick).await else {
        return;
    };

    let Some(current_epoch) = current_epoch_info_and_message_handler
        .as_ref()
        .map(|(epoch_info, _)| epoch_info.epoch)
    else {
        return;
    };

    // Shut down the message handler if a new epoch is detected for which we
    // have not yet received secret `PoL` info.
    match epoch_event {
        EpochEvent::NewEpoch((_, new_epoch))
        | EpochEvent::NewEpochAndOldEpochTransitionExpired((_, new_epoch))
            if new_epoch > current_epoch =>
        {
            debug!(target: LOG_TARGET, "New epoch detected: {epoch_event:?}, shutting down message handler until new secret PoL info is available.");
            *current_epoch_info_and_message_handler = None;
        }
        // If it's not a new epoch event, or if the new epoch has already been processed when the
        // secret info was received, keep the current message handler.
        _ => {}
    }
}

/// Processes new secret `PoL` info.
///
/// Always creates a new message handler using the new epoch's public and
/// private inputs from the `PoL` info, while preserving the current session.
fn handle_new_secret_epoch_info<Backend, NodeId, ProofsGenerator, RuntimeServiceId>(
    new_pol_epoch_info: &PolEpochInfo,
    settings: RunningSettings<Backend, NodeId, RuntimeServiceId>,
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    current_membership_info: &MembershipInfo<NodeId>,
    current_epoch_info_and_message_handler: &mut Option<
        EpochInfoAndHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
    >,
) where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone + Eq + Hash + Send + 'static,
    ProofsGenerator: LeaderProofsGenerator,
    RuntimeServiceId: Clone,
{
    let Some(zk_root) = current_membership_info.zk.as_ref().map(|zk| zk.root) else {
        *current_epoch_info_and_message_handler = None;
        return;
    };

    let current_membership = current_membership_info.membership.clone();
    let new_public_inputs = PoQVerificationInputsMinusSigningKey {
        leader: LeaderInputs {
            lottery_0: new_pol_epoch_info.poq_public_inputs.lottery_0,
            lottery_1: new_pol_epoch_info.poq_public_inputs.lottery_1,
            pol_epoch_nonce: new_pol_epoch_info.poq_public_inputs.epoch_nonce,
            pol_ledger_aged: new_pol_epoch_info.poq_public_inputs.aged_root,
            message_quota: settings.session_leadership_quota(),
        },
        core: CoreInputs {
            quota: settings.cover.session_core_quota(
                settings.num_blend_layers,
                &settings.time,
                current_membership.size(),
            ),
            zk_root,
        },
        session: current_membership_info.session_number,
    };
    let new_handler = MessageHandler::try_new_with_edge_condition_check(
        settings,
        current_membership,
        new_public_inputs,
        new_pol_epoch_info.poq_private_inputs.clone(),
        overwatch_handle.clone(),
        new_pol_epoch_info.epoch,
    ).expect("Should not fail to re-create message handler on epoch rotation after private inputs are set.");

    *current_epoch_info_and_message_handler = Some((new_pol_epoch_info.clone(), new_handler));
}
