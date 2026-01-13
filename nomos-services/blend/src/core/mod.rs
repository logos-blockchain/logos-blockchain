use std::{
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    time::Duration,
};

use async_trait::async_trait;
use backends::BlendBackend;
use chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use fork_stream::StreamExt as _;
use futures::{
    FutureExt as _, Stream, StreamExt as _,
    future::{BoxFuture, join_all},
};
use key_management_system_service::{api::KmsServiceApi, keys::PublicKeyEncoding};
use network::NetworkAdapter;
use nomos_blend::{
    crypto::random_sized_bytes,
    message::{
        PayloadType,
        encap::{
            ProofsVerifier as ProofsVerifierTrait, encapsulated::EncapsulatedMessage,
            validated::EncapsulatedMessageWithVerifiedPublicHeader,
        },
        reward::{
            self, ActivityProof, BlendingToken, OldSessionBlendingTokenCollector,
            SessionBlendingTokenCollector,
        },
    },
    proofs::quota::inputs::prove::{
        private::ProofOfLeadershipQuotaInputs,
        public::{CoreInputs, LeaderInputs},
    },
    scheduling::{
        SessionMessageScheduler,
        message_blend::provers::core_and_leader::CoreAndLeaderProofsGenerator,
        message_scheduler::{
            OldSessionMessageScheduler, ProcessedMessageScheduler,
            round_info::{RoundInfo, RoundReleaseType},
            session_info::SessionInfo as SchedulerSessionInfo,
        },
        session::{SessionEvent, UninitializedSessionEventStream},
        stream::UninitializedFirstReadyStream,
    },
};
use nomos_core::{
    codec::{DeserializeOp as _, SerializeOp as _},
    sdp::ActivityMetadata,
};
use nomos_network::NetworkService;
use nomos_sdp::SdpMessage;
use nomos_time::{SlotTick, TimeService, TimeServiceMessage};
use nomos_utils::blake_rng::BlakeRng;
use overwatch::{
    OpaqueServiceResourcesHandle,
    overwatch::OverwatchHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        relay::{OutboundRelay, RelayError},
        state::StateUpdater,
    },
};
use rand::{RngCore, SeedableRng as _, seq::SliceRandom as _};
use serde::{Deserialize, Serialize};
use services_utils::{
    overwatch::{JsonFileBackend, RecoveryOperator},
    wait_until_services_are_ready,
};
use tokio::sync::oneshot;
use tracing::{error, info};

use crate::{
    core::{
        backends::{PublicInfo, SessionInfo},
        kms::{KmsPoQAdapter, PreloadKMSBackendCorePoQGenerator, PreloadKmsService},
        processor::{
            CoreCryptographicProcessor, DecapsulatedMessageType, Error,
            MultiLayerDecapsulationOutput,
        },
        scheduler::SchedulerWrapper,
        settings::BlendConfig,
        state::{RecoveryServiceState, ServiceState, StateUpdater as ServiceStateUpdater},
    },
    epoch_info::{
        ChainApi, EpochEvent, EpochHandler, LeaderInputsMinusQuota, PolEpochInfo,
        PolInfoProvider as PolInfoProviderTrait,
    },
    membership::{self, MembershipInfo, ZkInfo},
    message::{NetworkMessage, ProcessedMessage, ServiceMessage},
    session::{CoreSessionInfo, CoreSessionPublicInfo},
    settings::FIRST_STREAM_ITEM_READY_TIMEOUT,
};

pub mod backends;
pub mod kms;
pub mod network;
pub mod settings;

pub(super) mod service_components;

mod processor;
mod scheduler;
mod state;
#[cfg(test)]
mod tests;
pub use state::RecoveryServiceState as CoreServiceState;

const LOG_TARGET: &str = "blend::service::core";

/// A blend service that sends messages to the blend network
/// and broadcasts fully unwrapped messages through the [`NetworkService`].
///
/// The blend backend and the network adapter are generic types that are
/// independent of each other. For example, the blend backend can use the
/// libp2p network stack, while the network adapter can use the other network
/// backend.
pub struct BlendService<
    Backend,
    NodeId,
    Network,
    MembershipAdapter,
    SdpService,
    ProofsGenerator,
    ProofsVerifier,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> where
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId>,
    Network: NetworkAdapter<RuntimeServiceId>,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    last_saved_state: Option<ServiceState<Backend::Settings, Network::BroadcastSettings>>,
    _phantom: PhantomData<(
        Backend,
        MembershipAdapter,
        SdpService,
        ProofsGenerator,
        TimeBackend,
        ChainService,
        PolInfoProvider,
    )>,
}

impl<
    Backend,
    NodeId,
    Network,
    MembershipAdapter,
    SdpService,
    ProofsGenerator,
    ProofsVerifier,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> ServiceData
    for BlendService<
        Backend,
        NodeId,
        Network,
        MembershipAdapter,
        SdpService,
        ProofsGenerator,
        ProofsVerifier,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId>,
    Network: NetworkAdapter<RuntimeServiceId>,
{
    type Settings = BlendConfig<Backend::Settings>;
    type State = RecoveryServiceState<Backend::Settings, Network::BroadcastSettings>;
    type StateOperator = RecoveryOperator<
        JsonFileBackend<
            RecoveryServiceState<Backend::Settings, Network::BroadcastSettings>,
            BlendConfig<Backend::Settings>,
        >,
    >;
    type Message = ServiceMessage<Network::BroadcastSettings>;
}

#[async_trait]
impl<
    Backend,
    NodeId,
    Network,
    MembershipAdapter,
    SdpService,
    ProofsGenerator,
    ProofsVerifier,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> ServiceCore<RuntimeServiceId>
    for BlendService<
        Backend,
        NodeId,
        Network,
        MembershipAdapter,
        SdpService,
        ProofsGenerator,
        ProofsVerifier,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId> + Send + Sync,
    NodeId: Clone + Send + Eq + Hash + Sync + 'static,
    Network: NetworkAdapter<RuntimeServiceId, BroadcastSettings: Eq + Hash + Unpin> + Send + Sync,
    MembershipAdapter: membership::Adapter<NodeId = NodeId, Error: Send + Sync + 'static> + Send,
    membership::ServiceMessage<MembershipAdapter>: Send + Sync + 'static,
    ProofsGenerator:
        CoreAndLeaderProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>> + Send,
    SdpService: ServiceData<Message = SdpMessage> + Send,
    ProofsVerifier: ProofsVerifierTrait + Clone + Send,
    TimeBackend: nomos_time::backends::TimeBackend + Send,
    ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Send + Unpin + 'static> + Send,
    RuntimeServiceId: AsServiceId<NetworkService<Network::Backend, RuntimeServiceId>>
        + AsServiceId<<MembershipAdapter as membership::Adapter>::Service>
        + AsServiceId<SdpService>
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
        + AsServiceId<ChainService>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>
        + AsServiceId<Self>
        + Clone
        + Debug
        + Display
        + Sync
        + Send
        + Unpin
        + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        recovery_initial_state: Self::State,
    ) -> Result<Self, overwatch::DynError> {
        let state_updater = service_resources_handle.state_updater.clone();
        Ok(Self {
            service_resources_handle,
            // We consume the serializable state into the state type we interact with in the
            // service.
            last_saved_state: recovery_initial_state.service_state.map(|s| {
                s.try_into_state_with_state_updater(state_updater)
                    .expect("Stored state should be valid")
            }),
            _phantom: PhantomData,
        })
    }

    #[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
    async fn run(mut self) -> Result<(), overwatch::DynError> {
        let Self {
            service_resources_handle:
                OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                    ref mut inbound_relay,
                    ref overwatch_handle,
                    ref settings_handle,
                    ref status_updater,
                    state_updater,
                },
            last_saved_state,
            ..
        } = self;

        let blend_config = settings_handle.notifier().get_updated_settings();

        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_secs(60)),
            NetworkService<_, _>,
            TimeService<_, _>,
            <MembershipAdapter as membership::Adapter>::Service,
            SdpService,
            PreloadKmsService<_>
        )
        .await?;

        let network_adapter = async {
            let network_relay = overwatch_handle
                .relay::<NetworkService<_, _>>()
                .await
                .expect("Relay with network service should be available.");
            Network::new(network_relay)
        }
        .await;

        let mut epoch_handler = async {
            let chain_service = CryptarchiaServiceApi::<ChainService, _>::new(
                overwatch_handle
                    .relay::<ChainService>()
                    .await
                    .expect("Failed to establish channel with chain service."),
            );
            EpochHandler::new(
                chain_service,
                blend_config.time.epoch_transition_period_in_slots,
            )
        }
        .await;

        let kms_api = async {
            let kms_outbound_relay = overwatch_handle
                .relay::<PreloadKmsService<_>>()
                .await
                .expect("Relay with KMS service should be available.");

            KmsServiceApi::new(kms_outbound_relay)
        }
        .await;

        let PublicKeyEncoding::Zk(zk_public_key) = kms_api
            .public_key(blend_config.zk.secret_key_kms_id.clone())
            .await
            .expect("ZK public key for provided ID should be stored in KMS.")
        else {
            panic!("Key with specified ID is not a ZK key.");
        };

        let membership_stream = MembershipAdapter::new(
            overwatch_handle
                .relay::<<MembershipAdapter as membership::Adapter>::Service>()
                .await
                .expect("Failed to get relay channel with membership service."),
            blend_config.crypto.non_ephemeral_signing_key.public_key(),
            Some(zk_public_key),
        )
        .subscribe()
        .await
        .expect("Failed to get membership stream from membership service.");

        let sdp_relay = overwatch_handle
            .relay::<SdpService>()
            .await
            .expect("Relay with SDP service should be available.");

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

        // Initialize components for the service.
        let (
            mut remaining_session_stream,
            mut remaining_clock_stream,
            current_public_info,
            crypto_processor,
            current_recovery_checkpoint,
            message_scheduler,
            mut backend,
            mut rng,
        ) = initialize::<
            NodeId,
            Backend,
            Network,
            CryptarchiaServiceApi<ChainService, RuntimeServiceId>,
            ProofsGenerator,
            ProofsVerifier,
            KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
            RuntimeServiceId,
        >(
            blend_config.clone(),
            membership_stream,
            clock_stream,
            &mut epoch_handler,
            overwatch_handle.clone(),
            kms_api,
            &sdp_relay,
            last_saved_state,
            state_updater,
        )
        .await;

        status_updater.notify_ready();
        tracing::info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        // Initialize more components that can be successfully created after
        // `notify_ready()`.
        let secret_pol_info_stream = post_initialize::<PolInfoProvider, _>(overwatch_handle).await;

        let mut blend_messages = backend.listen_to_incoming_messages();

        // Run the main event loop while the node is a core node across multiple
        // sessions. When the node becomes a non-core node in a new session, the
        // components for the last session transition period are returned.
        let (
            old_session_crypto_processor,
            old_session_message_scheduler,
            old_session_blending_token_collector,
            old_session_public_info,
        ) = run_event_loop(
            inbound_relay,
            &mut blend_messages,
            &mut remaining_clock_stream,
            secret_pol_info_stream,
            &mut remaining_session_stream,
            &blend_config,
            &mut backend,
            &network_adapter,
            &sdp_relay,
            &mut epoch_handler,
            message_scheduler.into(),
            &mut rng,
            crypto_processor,
            current_public_info,
            current_recovery_checkpoint,
        )
        .await;

        // The main event loop has ended because the node is no longer a core node
        // in the new session.
        // Before terminating the service, complete the old session during a single
        // session transition period.
        retire(
            blend_messages,
            remaining_clock_stream,
            remaining_session_stream,
            &blend_config,
            backend,
            network_adapter,
            sdp_relay,
            epoch_handler,
            old_session_message_scheduler,
            rng,
            old_session_blending_token_collector,
            old_session_crypto_processor,
            old_session_public_info,
        )
        .await;

        Ok(())
    }
}

/// Initialize the components for the [`BlendService`].
#[expect(clippy::too_many_lines, reason = "Need to initialize many components")]
#[expect(
    clippy::cognitive_complexity,
    reason = "Need to initialize many components"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "Need to initialize many components."
)]
async fn initialize<
    NodeId,
    Backend,
    NetAdapter,
    ChainService,
    ProofsGenerator,
    ProofsVerifier,
    KmsAdapter,
    RuntimeServiceId,
>(
    blend_config: BlendConfig<Backend::Settings>,
    membership_stream: impl Stream<Item = MembershipInfo<NodeId>> + Send + Unpin + 'static,
    clock_stream: impl Stream<Item = SlotTick> + Send + Sync + Unpin + 'static,
    epoch_handler: &mut EpochHandler<ChainService, RuntimeServiceId>,
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    kms_adapter: KmsAdapter,
    sdp_relay: &OutboundRelay<SdpMessage>,
    mut last_saved_state: Option<ServiceState<Backend::Settings, NetAdapter::BroadcastSettings>>,
    state_updater: StateUpdater<
        Option<RecoveryServiceState<Backend::Settings, NetAdapter::BroadcastSettings>>,
    >,
) -> (
    impl Stream<Item = SessionEvent<CoreSessionInfo<NodeId, KmsAdapter::CorePoQGenerator>>>
    + Unpin
    + Send
    + 'static,
    impl Stream<Item = SlotTick> + Unpin + Send + Sync + 'static,
    PublicInfo<NodeId>,
    CoreCryptographicProcessor<
        NodeId,
        KmsAdapter::CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    ServiceState<Backend::Settings, NetAdapter::BroadcastSettings>,
    SchedulerWrapper<
        BlakeRng,
        ProcessedMessage<NetAdapter::BroadcastSettings>,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    Backend,
    BlakeRng,
)
where
    NodeId: Clone + Eq + Hash + Send + 'static,
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId> + Sync,
    NetAdapter: NetworkAdapter<RuntimeServiceId, BroadcastSettings: Eq + Hash + Unpin>,
    ChainService: ChainApi<RuntimeServiceId> + Sync,
    ProofsGenerator: CoreAndLeaderProofsGenerator<KmsAdapter::CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
    // To avoid bubbling up generics everywhere in the configs (current Overwatch limitation), we
    // know the final key ID type is a `String`, so we constraint the trait impl here instead.
    KmsAdapter: KmsPoQAdapter<RuntimeServiceId, KeyId = String, CorePoQGenerator: Clone + Send + Sync>
        + Send
        + 'static,
    RuntimeServiceId: Clone + Send + Sync + 'static,
{
    // Initialize membership stream for session and core-related public PoQ inputs.
    let session_stream = async {
        let config = blend_config.clone();
        let zk_sk_id = config.zk.secret_key_kms_id.clone();
        membership_stream.map(
            move |MembershipInfo {
                      membership,
                      session_number,
                      zk:
                          ZkInfo {
                              core_and_path_selectors,
                              root: zk_root,
                          },
                  }| CoreSessionInfo {
                public: CoreSessionPublicInfo {
                    poq_core_public_inputs: CoreInputs {
                        quota: config.session_quota(membership.size()),
                        zk_root,
                    },
                    membership,
                    session: session_number,
                },
                core_poq_generator: kms_adapter.core_poq_generator(
                    zk_sk_id.clone(),
                    Box::new(
                        core_and_path_selectors
                            .expect("Core merkle path should be present for a core node."),
                    ),
                ),
            },
        )
    }
    .await;
    let (current_membership_info, remaining_session_stream) = Box::pin(
        UninitializedSessionEventStream::new(
            session_stream,
            FIRST_STREAM_ITEM_READY_TIMEOUT,
            blend_config.time.session_transition_period(),
        )
        .await_first_ready(),
    )
    .await
    .map(|(membership_info, remaining_session_stream)| {
        (membership_info, remaining_session_stream.fork())
    })
    .expect("The current session info must be available.");

    let (
        LeaderInputsMinusQuota {
            pol_epoch_nonce,
            pol_ledger_aged,
            total_stake,
        },
        remaining_clock_stream,
    ) = async {
        let (clock_tick, remaining_clock_stream) =
            UninitializedFirstReadyStream::new(clock_stream, Duration::from_secs(5))
                .first()
                .await
                .expect("The clock system must be available.");
        let Some(EpochEvent::NewEpoch(new_epoch_info)) = epoch_handler.tick(clock_tick).await
        else {
            panic!("First poll result of epoch stream should be a `NewEpoch` event.");
        };
        (new_epoch_info, remaining_clock_stream)
    }
    .await;

    info!(
        target: LOG_TARGET,
        "The current membership is ready: {} nodes.",
        current_membership_info.public.membership.size()
    );

    let current_public_info = PublicInfo {
        epoch: LeaderInputs {
            pol_ledger_aged,
            pol_epoch_nonce,
            message_quota: blend_config.crypto.num_blend_layers.into(),
            total_stake,
        },
        session: SessionInfo {
            membership: current_membership_info.public.membership.clone(),
            session_number: current_membership_info.public.session,
            core_public_inputs: current_membership_info.public.poq_core_public_inputs,
        },
    };

    let crypto_processor = CoreCryptographicProcessor::<
        _,
        KmsAdapter::CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >::try_new_with_core_condition_check(
        current_membership_info.public.membership.clone(),
        blend_config.minimum_network_size,
        &blend_config.crypto,
        current_public_info.clone().into(),
        current_membership_info.core_poq_generator,
    )
    .expect("The initial membership should satisfy the core node condition");

    // Initialize the current session state. If the session matches the stored one,
    // retrieves the tracked consumed core quota. Else, fallback to `0`.
    let current_recovery_checkpoint = if let Some(saved_state) = last_saved_state.take()
        && saved_state.last_seen_session() == current_membership_info.public.session
    {
        tracing::debug!(target: LOG_TARGET, "Found recovery state for session {:?}: {saved_state:?}", current_membership_info.public.session);
        saved_state
    } else {
        tracing::debug!(target: LOG_TARGET, "No recovery state found for session {:?}. Initializing a new one.", current_membership_info.public.session);

        ServiceState::with_session(
            current_membership_info.public.session,
            SessionBlendingTokenCollector::new(
                &reward::SessionInfo::new(
                    current_membership_info.public.session,
                    &pol_epoch_nonce,
                    current_membership_info.public.membership.size() as u64,
                    current_membership_info.public.poq_core_public_inputs.quota,
                ).expect("Reward session info must be created successfully. Panicking since the service cannot continue with this session")
            ),
            None,
            state_updater,
        ).expect("service state should be created successfully")
    };

    // If there is the old session token collector loaded from `last_saved_state`,
    // compute/submit its activity proof because we won't collect more tokens for
    // the old session after this initialization step because we are not
    // establishing connections for the old session.
    let mut state_updater = current_recovery_checkpoint.start_updating();
    if let Some(old_session_token_collector) = state_updater.clear_old_session_token_collector() {
        tracing::info!(target: LOG_TARGET, "Old session token collector loaded. Computing activity proof");
        compute_and_submit_activity_proof(old_session_token_collector, sdp_relay).await;
    }
    let current_recovery_checkpoint = state_updater.commit_changes();

    let message_scheduler = SchedulerWrapper::new_with_initial_messages(
        SchedulerSessionInfo {
            core_quota: blend_config
                .session_quota(current_membership_info.public.membership.size())
                .saturating_sub(current_recovery_checkpoint.spent_quota()),
            session_number: u128::from(current_membership_info.public.session).into(),
        },
        BlakeRng::from_entropy(),
        blend_config.scheduler_settings(),
        // We don't consume the map because we will remove the items one by one once they
        // will be scheduled for release.
        current_recovery_checkpoint
            .unsent_processed_messages()
            .clone()
            .into_iter(),
        current_recovery_checkpoint
            .unsent_data_messages()
            .clone()
            .into_iter(),
    );

    let backend = Backend::new(
        blend_config.clone(),
        overwatch_handle,
        current_public_info.clone(),
        BlakeRng::from_entropy(),
    );

    // Rng for releasing messages.
    let rng = BlakeRng::from_entropy();

    (
        remaining_session_stream,
        remaining_clock_stream,
        current_public_info,
        crypto_processor,
        current_recovery_checkpoint,
        message_scheduler,
        backend,
        rng,
    )
}

/// Post-initialization step that must be performed after signaling the service
/// readiness to Overwatch.
async fn post_initialize<PolInfoProvider, RuntimeServiceId>(
    overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
) -> impl Stream<Item = PolEpochInfo>
where
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Send + Unpin + 'static> + Send,
{
    // There might be services that depend on Blend to be ready before starting, so
    // we cannot wait for the stream to be sent before we signal we are
    // ready, hence this should always be called after `notify_ready();`.
    // Also, Blend services start even if such a stream is not immediately
    // available, since they will simply keep blending cover messages.
    PolInfoProvider::subscribe(overwatch_handle)
        .await
        .expect("Should not fail to subscribe to secret PoL info stream.")
}

// Run the main event loop that persists while the node is a core node.
// This can span across multiple sessions.
#[expect(clippy::too_many_arguments, reason = "categorize args")]
async fn run_event_loop<
    NodeId,
    Backend,
    Rng,
    NetAdapter,
    ChainService,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    mut inbound_relay: impl Stream<Item = ServiceMessage<NetAdapter::BroadcastSettings>> + Unpin,
    blend_messages: &mut (
             impl Stream<Item = EncapsulatedMessageWithVerifiedPublicHeader> + Send + Unpin + 'static
         ),
    remaining_clock_stream: &mut (impl Stream<Item = SlotTick> + Send + Sync + Unpin + 'static),
    mut secret_pol_info_stream: impl Stream<Item = PolEpochInfo> + Unpin,
    remaining_session_stream: &mut (
             impl Stream<Item = SessionEvent<CoreSessionInfo<NodeId, CorePoQGenerator>>> + Unpin
         ),

    blend_config: &BlendConfig<Backend::Settings>,
    backend: &mut Backend,
    network_adapter: &NetAdapter,
    sdp_relay: &OutboundRelay<SdpMessage>,
    epoch_handler: &mut EpochHandler<ChainService, RuntimeServiceId>,
    mut message_scheduler: SessionMessageScheduler<
        Rng,
        ProcessedMessage<NetAdapter::BroadcastSettings>,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    rng: &mut Rng,

    mut crypto_processor: CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    mut public_info: PublicInfo<NodeId>,
    mut recovery_checkpoint: ServiceState<Backend::Settings, NetAdapter::BroadcastSettings>,
) -> (
    CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    OldSessionMessageScheduler<Rng, ProcessedMessage<NetAdapter::BroadcastSettings>>,
    OldSessionBlendingTokenCollector,
    PublicInfo<NodeId>,
)
where
    NodeId: Clone + Eq + Hash + Send + 'static,
    Rng: rand::Rng + Clone + Send + Unpin,
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId> + Sync,
    NetAdapter: NetworkAdapter<
            RuntimeServiceId,
            BroadcastSettings: Serialize
                                   + for<'de> Deserialize<'de>
                                   + Debug
                                   + Eq
                                   + Hash
                                   + Clone
                                   + Send
                                   + Sync
                                   + Unpin,
        > + Sync,
    ChainService: ChainApi<RuntimeServiceId> + Sync,
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
    RuntimeServiceId: Sync,
{
    // An optional crypto processor to handle the old session during transition
    // period.
    let mut old_session_crypto_processor: Option<
        CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    > = None;
    let mut old_session_message_scheduler: Option<
        OldSessionMessageScheduler<Rng, ProcessedMessage<NetAdapter::BroadcastSettings>>,
    > = None;

    loop {
        tokio::select! {
            Some(local_data_message) = inbound_relay.next() => {
                recovery_checkpoint = handle_local_data_message(local_data_message, &mut crypto_processor, &mut message_scheduler, recovery_checkpoint).await;
            }
            Some(incoming_message) = blend_messages.next() => {
                recovery_checkpoint = handle_incoming_blend_message(incoming_message, &mut message_scheduler, old_session_message_scheduler.as_mut(), &crypto_processor, old_session_crypto_processor.as_ref(),  recovery_checkpoint);
            }
            Some(round_info) = message_scheduler.next() => {
                recovery_checkpoint = handle_release_round(round_info, &mut crypto_processor, rng, backend, network_adapter,  recovery_checkpoint).await;
            }
            Some(processed_messages_to_release) = async {
                if let Some(old_scheduler) = &mut old_session_message_scheduler {
                    old_scheduler.next().await
                } else {
                    None
                }
            } => {
                handle_release_round_for_old_session(processed_messages_to_release, rng, backend, network_adapter).await;
            }
            Some(clock_tick) = remaining_clock_stream.next() => {
                public_info = handle_clock_event(clock_tick, blend_config, epoch_handler, &mut crypto_processor, backend, public_info).await;
            }
            Some(pol_info) = secret_pol_info_stream.next() => {
                handle_new_secret_epoch_info(pol_info.poq_private_inputs, &mut crypto_processor);
            }
            Some(session_event) = remaining_session_stream.next() => {
                match handle_session_event(session_event, blend_config, crypto_processor, message_scheduler, public_info, recovery_checkpoint, backend, sdp_relay).await {
                    HandleSessionEventOutput::Transitioning { new_crypto_processor, old_crypto_processor, new_scheduler, old_scheduler, new_public_info, new_recovery_checkpoint } => {
                        crypto_processor = new_crypto_processor;
                        old_session_crypto_processor = Some(old_crypto_processor);
                        message_scheduler = new_scheduler;
                        old_session_message_scheduler = Some(old_scheduler);
                        public_info = new_public_info;
                        recovery_checkpoint = new_recovery_checkpoint;
                    },
                    HandleSessionEventOutput::TransitionCompleted { current_crypto_processor, current_scheduler, current_public_info, new_recovery_checkpoint } => {
                        crypto_processor = current_crypto_processor;
                        old_session_crypto_processor = None;
                        message_scheduler = current_scheduler;
                        old_session_message_scheduler = None;
                        public_info = current_public_info;
                        recovery_checkpoint = new_recovery_checkpoint;
                    },
                    HandleSessionEventOutput::Retiring { old_crypto_processor, old_scheduler, old_token_collector, old_public_info } => {
                        tracing::info!(target: LOG_TARGET, "Exiting from the main event loop");
                        return (
                            old_crypto_processor,
                            old_scheduler,
                            old_token_collector,
                            old_public_info,
                        );
                    },
                }
            }
        }
    }
}

/// Processes the old session during the session transition period
/// before retiring the core service.
#[expect(clippy::too_many_arguments, reason = "categorize args")]
async fn retire<
    NodeId,
    Backend,
    Rng,
    NetAdapter,
    ChainService,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    mut blend_messages: impl Stream<Item = EncapsulatedMessageWithVerifiedPublicHeader>
    + Send
    + Unpin
    + 'static,
    mut remaining_clock_stream: impl Stream<Item = SlotTick> + Send + Sync + Unpin + 'static,
    mut remaining_session_stream: impl Stream<
        Item = SessionEvent<CoreSessionInfo<NodeId, CorePoQGenerator>>,
    > + Unpin,
    blend_config: &BlendConfig<Backend::Settings>,
    mut backend: Backend,
    network_adapter: NetAdapter,
    sdp_relay: OutboundRelay<SdpMessage>,
    mut epoch_handler: EpochHandler<ChainService, RuntimeServiceId>,
    mut message_scheduler: OldSessionMessageScheduler<
        Rng,
        ProcessedMessage<NetAdapter::BroadcastSettings>,
    >,
    mut rng: Rng,
    mut blending_token_collector: OldSessionBlendingTokenCollector,
    mut crypto_processor: CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    mut public_info: PublicInfo<NodeId>,
) where
    NodeId: Clone + Eq + Hash + Send + 'static,
    Rng: rand::Rng + Clone + Send + Unpin,
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId> + Sync,
    NetAdapter: NetworkAdapter<
            RuntimeServiceId,
            BroadcastSettings: Serialize
                                   + for<'de> Deserialize<'de>
                                   + Debug
                                   + Eq
                                   + Hash
                                   + Clone
                                   + Send
                                   + Sync
                                   + Unpin,
        > + Sync,
    ChainService: ChainApi<RuntimeServiceId> + Sync,
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
    RuntimeServiceId: Sync,
{
    loop {
        tokio::select! {
            Some(incoming_message) = blend_messages.next() => {
                handle_incoming_blend_message_from_old_session(incoming_message, &mut message_scheduler, &crypto_processor, &mut blending_token_collector);
            }
            Some(processed_messages_to_release) = message_scheduler.next() => {
                handle_release_round_for_old_session(processed_messages_to_release, &mut rng, &backend, &network_adapter).await;
            }
            Some(clock_tick) = remaining_clock_stream.next() => {
                public_info = handle_clock_event(clock_tick, blend_config, &mut epoch_handler, &mut crypto_processor, &mut backend, public_info).await;
            }
            Some(SessionEvent::TransitionPeriodExpired) = remaining_session_stream.next() => {
                handle_session_transition_expired(&mut backend, blending_token_collector, &sdp_relay).await;
                // Now the core service is no longer needed for the current (new) session,
                // and the remaining session transition has been completed,
                // so finishing the retirement process.
                return;
            }
        }
    }
}

/// Handles a [`SessionEvent`].
///
/// It consumes the previous cryptographic processor and creates a new one
/// on a new session with its new membership. It also creates new public inputs
/// for `PoQ` verification in this new session. It ignores the transition period
/// expiration event and returns the previous cryptographic processor as is.
#[expect(clippy::too_many_arguments, reason = "necessary for session handling")]
async fn handle_session_event<
    NodeId,
    ProofsGenerator,
    ProofsVerifier,
    Backend,
    Rng,
    BroadcastSettings,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    event: SessionEvent<CoreSessionInfo<NodeId, CorePoQGenerator>>,
    settings: &BlendConfig<Backend::Settings>,
    current_cryptographic_processor: CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    current_scheduler: SessionMessageScheduler<
        Rng,
        ProcessedMessage<BroadcastSettings>,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    current_public_info: PublicInfo<NodeId>,
    current_recovery_checkpoint: ServiceState<Backend::Settings, BroadcastSettings>,
    backend: &mut Backend,
    sdp_relay: &OutboundRelay<SdpMessage>,
) -> HandleSessionEventOutput<
    NodeId,
    Rng,
    ProofsGenerator,
    ProofsVerifier,
    Backend::Settings,
    BroadcastSettings,
    CorePoQGenerator,
>
where
    NodeId: Eq + Hash + Clone + Send,
    Rng: rand::Rng + Clone + Unpin,
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
    BroadcastSettings: Debug + Clone + Send + Sync + Unpin,
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId>,
{
    match event {
        SessionEvent::NewSession(CoreSessionInfo {
            core_poq_generator,
            public:
                CoreSessionPublicInfo {
                    poq_core_public_inputs: new_core_public_inputs,
                    session: new_session,
                    membership: new_membership,
                },
        }) => {
            let (_, _, _, _, current_session_blending_token_collector, _, state_updater) =
                current_recovery_checkpoint.into_components();

            let new_reward_session_info = reward::SessionInfo::new(
                new_session,
                &current_public_info.epoch.pol_epoch_nonce,
                new_membership.size() as u64,
                new_core_public_inputs.quota,
            )
            .expect("Reward session info must be created successfully. Panicking since the service cannot continue with this session");
            let (new_session_blending_token_collector, old_session_blending_token_collector) =
                current_session_blending_token_collector.rotate_session(&new_reward_session_info);

            let new_session_info = SessionInfo {
                membership: new_membership.clone(),
                session_number: new_session,
                core_public_inputs: new_core_public_inputs,
            };
            backend.rotate_session(new_session_info.clone()).await;

            let new_scheduler_session_info = SchedulerSessionInfo {
                core_quota: settings.session_quota(new_session_info.membership.size()),
                session_number: u128::from(new_session).into(),
            };

            let new_public_info = PublicInfo {
                session: new_session_info.clone(),
                ..current_public_info
            };
            let new_processor = match CoreCryptographicProcessor::try_new_with_core_condition_check(
                new_membership,
                settings.minimum_network_size,
                &settings.crypto,
                new_public_info.clone().into(),
                core_poq_generator,
            ) {
                Ok(new_processor) => new_processor,
                Err(e @ (Error::LocalIsNotCoreNode | Error::NetworkIsTooSmall(_))) => {
                    tracing::info!(target: LOG_TARGET, "New membership does not satisfy the core node condition: {e:?}");
                    return HandleSessionEventOutput::Retiring {
                        old_crypto_processor: current_cryptographic_processor,
                        old_scheduler: current_scheduler
                            .rotate_session(
                                new_scheduler_session_info,
                                settings.scheduler_settings(),
                            )
                            .1,
                        old_token_collector: old_session_blending_token_collector,
                        old_public_info: current_public_info,
                    };
                }
            };

            let (new_scheduler, old_scheduler) = current_scheduler
                .rotate_session(new_scheduler_session_info, settings.scheduler_settings());
            HandleSessionEventOutput::Transitioning {
                new_crypto_processor: new_processor,
                old_crypto_processor: current_cryptographic_processor,
                new_scheduler,
                old_scheduler,
                new_public_info,
                new_recovery_checkpoint: ServiceState::with_session(
                    new_session,
                    new_session_blending_token_collector,
                    Some(old_session_blending_token_collector),
                    state_updater,
                )
                .expect("service state should be created successfully"),
            }
        }
        SessionEvent::TransitionPeriodExpired => {
            let mut state_updater = current_recovery_checkpoint.start_updating();

            if let Some(old_token_collector) = state_updater.clear_old_session_token_collector() {
                handle_session_transition_expired(backend, old_token_collector, sdp_relay).await;
            }

            HandleSessionEventOutput::TransitionCompleted {
                current_crypto_processor: current_cryptographic_processor,
                current_scheduler,
                current_public_info,
                new_recovery_checkpoint: state_updater.commit_changes(),
            }
        }
    }
}

/// Handles [`SessionEvent::TransitionPeriodExpired`].
async fn handle_session_transition_expired<Backend, NodeId, Rng, ProofsVerifier, RuntimeServiceId>(
    backend: &mut Backend,
    blending_token_collector: OldSessionBlendingTokenCollector,
    sdp_relay: &OutboundRelay<SdpMessage>,
) where
    Backend: BlendBackend<NodeId, Rng, ProofsVerifier, RuntimeServiceId>,
    NodeId: Eq + Hash + Clone + Send,
    ProofsVerifier: ProofsVerifierTrait,
{
    compute_and_submit_activity_proof(blending_token_collector, sdp_relay).await;
    backend.complete_session_transition().await;
}

async fn compute_and_submit_activity_proof(
    blending_token_collector: OldSessionBlendingTokenCollector,
    sdp_relay: &OutboundRelay<SdpMessage>,
) {
    if let Some(activity_proof) = blending_token_collector.compute_activity_proof() {
        if let Err(e) = submit_activity_proof(activity_proof, sdp_relay).await {
            error!(target: LOG_TARGET, "Failed to submit activity proof for the old session: {e:?}");
        }
    } else {
        info!(target: LOG_TARGET, "No activity proof generated for the old session");
    }
}

enum HandleSessionEventOutput<
    NodeId,
    Rng,
    ProofsGenerator,
    ProofsVerifier,
    BackendSettings,
    BroadcastSettings,
    CorePoQGenerator,
> {
    Transitioning {
        new_crypto_processor:
            CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
        old_crypto_processor:
            CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
        new_scheduler: SessionMessageScheduler<
            Rng,
            ProcessedMessage<BroadcastSettings>,
            EncapsulatedMessageWithVerifiedPublicHeader,
        >,
        old_scheduler: OldSessionMessageScheduler<Rng, ProcessedMessage<BroadcastSettings>>,
        new_public_info: PublicInfo<NodeId>,
        new_recovery_checkpoint: ServiceState<BackendSettings, BroadcastSettings>,
    },
    TransitionCompleted {
        current_crypto_processor:
            CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
        current_scheduler: SessionMessageScheduler<
            Rng,
            ProcessedMessage<BroadcastSettings>,
            EncapsulatedMessageWithVerifiedPublicHeader,
        >,
        current_public_info: PublicInfo<NodeId>,
        new_recovery_checkpoint: ServiceState<BackendSettings, BroadcastSettings>,
    },
    Retiring {
        old_crypto_processor:
            CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
        old_scheduler: OldSessionMessageScheduler<Rng, ProcessedMessage<BroadcastSettings>>,
        old_token_collector: OldSessionBlendingTokenCollector,
        old_public_info: PublicInfo<NodeId>,
    },
}

/// Blend a new message received from another service.
///
/// When a new local data message is received, an attempt to serialize and
/// encapsulate its payload is performed. If encapsulation is successful, the
/// message is queued with the Blend scheduler and blended during the next
/// round.
async fn handle_local_data_message<
    NodeId,
    Rng,
    BackendSettings,
    BroadcastSettings,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
>(
    local_data_message: ServiceMessage<BroadcastSettings>,
    cryptographic_processor: &mut CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    scheduler: &mut SessionMessageScheduler<
        Rng,
        ProcessedMessage<BroadcastSettings>,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    current_recovery_checkpoint: ServiceState<BackendSettings, BroadcastSettings>,
) -> ServiceState<BackendSettings, BroadcastSettings>
where
    NodeId: Eq + Hash + Send + 'static,
    Rng: RngCore + Clone + Send + Unpin,
    BackendSettings: Clone + Send + Sync,
    BroadcastSettings:
        Serialize + for<'de> Deserialize<'de> + Debug + Hash + Eq + Clone + Send + Sync + Unpin,
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
{
    let ServiceMessage::Blend(message_payload) = local_data_message;

    let serialized_data_message = NetworkMessage::<BroadcastSettings>::to_bytes(&message_payload)
        .expect("NetworkMessage should be able to be serialized")
        .to_vec();

    let Ok(wrapped_message) = cryptographic_processor
        .encapsulate_data_payload(&serialized_data_message)
        .await
        .inspect_err(|e| {
            tracing::error!(target: LOG_TARGET, "Failed to wrap message: {e:?}");
        })
    else {
        return current_recovery_checkpoint;
    };

    let mut state_updater = current_recovery_checkpoint.start_updating();

    // Before blending the data message, we try to peel off any outer layers that
    // are addressed to us. In this case, we collect the blending tokens and we
    // blend only the remaining layers.
    // TODO: Remove this logic once we don't have tests that deploy less than 3
    // Blend nodes, or when we start using a minimum network size of 3.
    let self_decapsulation_output =
        cryptographic_processor.decapsulate_message_recursive(wrapped_message.clone());

    let Ok(multi_layer_decapsulation_output) = self_decapsulation_output else {
        // The outermost layer of the data message is not for us, hence we treat this as
        // a regular data message that should be released at the next round.
        tracing::debug!(target: LOG_TARGET, "Locally generated data message does not have its outermost layer addressed to us. Sending it out as a data message...");
        scheduler.queue_data_message(wrapped_message.clone());
        assert_eq!(
            state_updater.add_unsent_data_message(wrapped_message.clone()),
            Ok(()),
            "There should not be another copy of the same locally-generated encapsulated data message: {wrapped_message:?}."
        );
        return state_updater.commit_changes();
    };

    // It happened that the outermost `N` layers were addressed to this very same
    // node, so we collect blending tokens for those layers and propagate only the
    // remaining part.
    let (blending_tokens, remaining_message_type) =
        multi_layer_decapsulation_output.into_components();
    let processed_message = match remaining_message_type {
        // If all the layers are peeled off locally, then we are left with the initial data message.
        DecapsulatedMessageType::Completed(_) => {
            tracing::debug!(target: LOG_TARGET, "Locally generated data message {message_payload:?} had all the {} layers addressed to this same node. Propagating only the fully decapsulated message.", blending_tokens.len());
            ProcessedMessage::from(message_payload)
        }
        DecapsulatedMessageType::Incompleted(remaining_encapsulated_message) => {
            tracing::debug!(target: LOG_TARGET, "Locally generated data message had the outermost {} layers addressed to this same node. Propagating only the remaining encapsulated layers.", blending_tokens.len());
            ProcessedMessage::from(*remaining_encapsulated_message)
        }
    };
    state_updater.collect_current_session_tokens(blending_tokens.into_iter());

    // We treat a partially or fully decapsulated message as a processed message,
    // and we schedule for its release at the next release round.
    scheduler.schedule_processed_message(processed_message.clone());
    assert_eq!(
        state_updater.add_unsent_processed_message(processed_message.clone()),
        Ok(()),
        "There should not be another copy of the same locally-generated processed message: {processed_message:?}."
    );
    state_updater.commit_changes()
}

/// Processes an already unwrapped and validated Blend message received from
/// a core or edge peer.
fn handle_incoming_blend_message<
    NodeId,
    Rng,
    BroadcastSettings,
    BackendSettings,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
>(
    validated_encapsulated_message: EncapsulatedMessageWithVerifiedPublicHeader,
    scheduler: &mut SessionMessageScheduler<
        Rng,
        ProcessedMessage<BroadcastSettings>,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    old_session_scheduler: Option<
        &mut OldSessionMessageScheduler<Rng, ProcessedMessage<BroadcastSettings>>,
    >,
    cryptographic_processor: &CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    old_session_cryptographic_processor: Option<
        &CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    >,
    current_recovery_checkpoint: ServiceState<BackendSettings, BroadcastSettings>,
) -> ServiceState<BackendSettings, BroadcastSettings>
where
    NodeId: 'static,
    Rng: RngCore + Clone + Send + Unpin,
    BroadcastSettings: Serialize + for<'de> Deserialize<'de> + Debug + Eq + Hash + Clone + Send,
    BackendSettings: Clone,
    ProofsVerifier: ProofsVerifierTrait,
{
    // First, try to decapsulate with the current session crypto processor.
    // If that fails, try with the old session crypto processor, if any.
    match cryptographic_processor
        .decapsulate_message_recursive(validated_encapsulated_message.clone())
    {
        Ok(output) => handle_decapsulated_incoming_message_from_current_session(
            output,
            scheduler,
            current_recovery_checkpoint,
        ),
        Err(e) => {
            tracing::debug!(target: LOG_TARGET, "Failed to decapsulate received message with the current session crypto processor: {e:?}");
            let (Some(old_crypto_processor), Some(old_session_scheduler)) =
                (old_session_cryptographic_processor, old_session_scheduler)
            else {
                return current_recovery_checkpoint;
            };
            match old_crypto_processor.decapsulate_message_recursive(validated_encapsulated_message)
            {
                Ok(output) => handle_decapsulated_incoming_message_from_old_session(
                    output,
                    old_session_scheduler,
                    current_recovery_checkpoint,
                ),
                Err(e) => {
                    tracing::debug!(target: LOG_TARGET, "Failed to decapsulate received message with the old session crypto processor: {e:?}");
                    current_recovery_checkpoint
                }
            }
        }
    }
}

/// Same as [`handle_incoming_blend_message`] but only tries with
/// the old session crypto processor.
fn handle_incoming_blend_message_from_old_session<
    Rng,
    NodeId,
    BroadcastSettings,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
>(
    validated_encapsulated_message: EncapsulatedMessageWithVerifiedPublicHeader,
    scheduler: &mut OldSessionMessageScheduler<Rng, ProcessedMessage<BroadcastSettings>>,
    cryptographic_processor: &CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    blending_token_collector: &mut OldSessionBlendingTokenCollector,
) where
    NodeId: 'static,
    BroadcastSettings: Serialize + for<'de> Deserialize<'de> + Debug + Eq + Hash + Clone + Send,
    ProofsVerifier: ProofsVerifierTrait,
{
    match cryptographic_processor.decapsulate_message_recursive(validated_encapsulated_message) {
        Ok(output) => {
            let (_, blending_tokens) = schedule_decapsulated_incoming_message(output, scheduler);
            for blending_token in blending_tokens {
                blending_token_collector.collect(blending_token);
            }
        }
        Err(e) => {
            tracing::debug!(target: LOG_TARGET, "Failed to decapsulate received message from old session: {e:?}");
        }
    }
}

/// Schedules a decapsulated incoming message from the current session,
/// and collects the blending tokens obtained from the decapsulation.
///
/// It updates the recovery checkpoint by storing the scheduled message
/// and the collected tokens.
fn handle_decapsulated_incoming_message_from_current_session<
    Rng,
    BroadcastSettings,
    BackendSettings,
>(
    multi_layer_decapsulation_output: MultiLayerDecapsulationOutput,
    scheduler: &mut SessionMessageScheduler<
        Rng,
        ProcessedMessage<BroadcastSettings>,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    current_recovery_checkpoint: ServiceState<BackendSettings, BroadcastSettings>,
) -> ServiceState<BackendSettings, BroadcastSettings>
where
    BroadcastSettings: Serialize + for<'de> Deserialize<'de> + Debug + Eq + Hash + Clone + Send,
    BackendSettings: Clone,
{
    let mut state_updater = current_recovery_checkpoint.start_updating();

    let (maybe_processed_message, blending_tokens) =
        schedule_decapsulated_incoming_message(multi_layer_decapsulation_output, scheduler);

    if let Some(processed_message) = maybe_processed_message
        && state_updater.add_unsent_processed_message(processed_message) == Err(())
    {
        tracing::warn!(target: LOG_TARGET, "The same processed message was already added to the recovery state. Ignoring the new one...");
    }

    state_updater.collect_current_session_tokens(blending_tokens);
    state_updater.commit_changes()
}

/// Schedules a decapsulated incoming message from the old session,
/// and collects the blending tokens obtained from the decapsulation.
///
/// It updates the recovery checkpoint by storing the collected tokens.
fn handle_decapsulated_incoming_message_from_old_session<Rng, BroadcastSettings, BackendSettings>(
    multi_layer_decapsulation_output: MultiLayerDecapsulationOutput,
    scheduler: &mut OldSessionMessageScheduler<Rng, ProcessedMessage<BroadcastSettings>>,
    recovery_checkpoint: ServiceState<BackendSettings, BroadcastSettings>,
) -> ServiceState<BackendSettings, BroadcastSettings>
where
    BroadcastSettings: Serialize + for<'de> Deserialize<'de> + Debug + Eq + Hash + Clone + Send,
    BackendSettings: Clone,
{
    let (_, blending_tokens) =
        schedule_decapsulated_incoming_message(multi_layer_decapsulation_output, scheduler);

    let mut state_updater = recovery_checkpoint.start_updating();
    state_updater
        .collect_old_session_tokens(blending_tokens)
        .expect("token collector in the state should be updated successfully");
    state_updater.commit_changes()
}

/// Schedules a decapsulated incoming message using a message scheduler.
///
/// It returns the processed message if it has been scheduled, along with
/// the blending tokens obtained from the decapsulation.
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: Address this at some point."
)]
fn schedule_decapsulated_incoming_message<BroadcastSettings>(
    multi_layer_decapsulation_output: MultiLayerDecapsulationOutput,
    scheduler: &mut impl ProcessedMessageScheduler<ProcessedMessage<BroadcastSettings>>,
) -> (
    Option<ProcessedMessage<BroadcastSettings>>,
    impl Iterator<Item = BlendingToken>,
)
where
    BroadcastSettings: Serialize + for<'de> Deserialize<'de> + Debug + Eq + Hash + Clone + Send,
{
    let (blending_tokens, decapsulated_message_type) =
        multi_layer_decapsulation_output.into_components();
    tracing::trace!(target: LOG_TARGET, "Batch-decapsulated {} layers from the received message.", blending_tokens.len());

    match decapsulated_message_type {
        DecapsulatedMessageType::Completed(fully_decapsulated_message) => {
            match fully_decapsulated_message.into_components() {
                (PayloadType::Cover, _) => {
                    tracing::info!(target: LOG_TARGET, "Discarding received cover message.");
                    (None, blending_tokens.into_iter())
                }
                (PayloadType::Data, serialized_data_message) => {
                    tracing::debug!(target: LOG_TARGET, "Processing a fully decapsulated data message.");
                    match NetworkMessage::from_bytes(&serialized_data_message) {
                        Ok(deserialized_network_message) => {
                            tracing::debug!(target: LOG_TARGET, "Fully decapsulated and deserialized processed data message: {deserialized_network_message:?}");
                            let processed_message =
                                ProcessedMessage::from(deserialized_network_message);
                            scheduler.schedule_processed_message(processed_message.clone());
                            (Some(processed_message), blending_tokens.into_iter())
                        }
                        Err(e) => {
                            tracing::debug!(target: LOG_TARGET, "Unrecognized data message from blend backend. Dropping: {e:?}");
                            (None, blending_tokens.into_iter())
                        }
                    }
                }
            }
        }
        DecapsulatedMessageType::Incompleted(remaining_encapsulated_message) => {
            tracing::debug!(target: LOG_TARGET, "Processed encapsulated data message: {remaining_encapsulated_message:?}");
            let processed_message = ProcessedMessage::from(*remaining_encapsulated_message);
            scheduler.schedule_processed_message(processed_message.clone());
            (Some(processed_message), blending_tokens.into_iter())
        }
    }
}

/// Reacts to a new release tick as returned by the scheduler.
///
/// When that happens, the previously processed messages (both encapsulated and
/// unencapsulated ones) as well as optionally a cover message are handled.
/// For unencapsulated messages, they are broadcasted to the rest of the network
/// using the configured network adapter. For encapsulated messages as well as
/// the optional cover message, they are forwarded to the rest of the connected
/// Blend peers.
async fn handle_release_round<
    NodeId,
    Rng,
    Backend,
    NetAdapter,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    RoundInfo {
        data_messages,
        release_type,
    }: RoundInfo<
        ProcessedMessage<NetAdapter::BroadcastSettings>,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    cryptographic_processor: &mut CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    rng: &mut Rng,
    backend: &Backend,
    network_adapter: &NetAdapter,
    current_recovery_checkpoint: ServiceState<Backend::Settings, NetAdapter::BroadcastSettings>,
) -> ServiceState<Backend::Settings, NetAdapter::BroadcastSettings>
where
    NodeId: Eq + Hash + 'static,
    Rng: RngCore + Send,
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId> + Sync,
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
    NetAdapter: NetworkAdapter<RuntimeServiceId, BroadcastSettings: Eq + Hash> + Sync,
{
    let (processed_messages, should_generate_cover_message) =
        release_type.map_or_else(|| (vec![], false), RoundReleaseType::into_components);
    let (data_count, processed_count, cover_count) = (
        data_messages.len(),
        processed_messages.len(),
        usize::from(should_generate_cover_message),
    );
    let mut state_updater = current_recovery_checkpoint.start_updating();

    let data_messages_relay_futures = data_messages.into_iter()
        // While we iterate and map the messages to the sending futures, we update the recovery state to remove each message.
        .inspect(|data_message_to_blend| {
            if state_updater.remove_sent_data_message(data_message_to_blend).is_err() {
                tracing::warn!(target: LOG_TARGET, "Recovered data message should be present in the recovery state but was not found.");
            }
            // Each data message that is sent is one less cover message that should be generated, hence we consume one core quota per data message here.
            state_updater.consume_core_quota(1);
        }).map(
            |data_message_to_blend| -> BoxFuture<'_, ()> {
                backend.publish(data_message_to_blend.into()).boxed()
            },
        ).collect::<Vec<_>>();

    let processed_messages_relay_futures = build_futures_to_release_processed_messages(
        processed_messages,
        backend,
        network_adapter,
        Some(&mut state_updater),
    );

    let mut message_futures = data_messages_relay_futures
        .into_iter()
        .chain(processed_messages_relay_futures)
        .collect::<Vec<_>>();

    if should_generate_cover_message
        // TODO: Remove this logic once we don't have tests that deploy less than 3 Blend nodes, or when we start using a minimum network size of 3.
        && let Some(encapsulated_cover_message) = generate_and_try_to_decapsulate_cover_message(
            cryptographic_processor,
            &mut state_updater,
        )
        .await
    {
        message_futures.push(backend.publish(encapsulated_cover_message).boxed());
    }

    message_futures.shuffle(rng);

    // Release all messages concurrently, and wait for all of them to be sent.
    join_all(message_futures).await;
    tracing::debug!(target: LOG_TARGET, "Sent out {data_count} data, {processed_count} processed and {cover_count} cover messages at this release window.");

    state_updater.commit_changes()
}

async fn handle_release_round_for_old_session<
    NodeId,
    Rng,
    Backend,
    NetAdapter,
    ProofsVerifier,
    RuntimeServiceId,
>(
    processed_messages_to_release: Vec<ProcessedMessage<NetAdapter::BroadcastSettings>>,
    rng: &mut Rng,
    backend: &Backend,
    network_adapter: &NetAdapter,
) where
    NodeId: Eq + Hash + 'static,
    Rng: RngCore + Send,
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId> + Sync,
    NetAdapter: NetworkAdapter<RuntimeServiceId, BroadcastSettings: Eq + Hash> + Sync,
{
    let mut futures = build_futures_to_release_processed_messages(
        processed_messages_to_release,
        backend,
        network_adapter,
        None,
    );
    futures.shuffle(rng);

    // Release all messages concurrently, and wait for all of them to be sent.
    let num_futures = futures.len();
    join_all(futures).await;
    tracing::debug!(target: LOG_TARGET, "Sent out {num_futures} processed messages at this release window for the old session");
}

fn build_futures_to_release_processed_messages<
    'fut,
    NodeId,
    Backend,
    NetAdapter,
    ProofsVerifier,
    RuntimeServiceId,
>(
    processed_messages_to_release: Vec<ProcessedMessage<NetAdapter::BroadcastSettings>>,
    backend: &'fut Backend,
    network_adapter: &'fut NetAdapter,
    mut state_updater: Option<
        &mut ServiceStateUpdater<Backend::Settings, NetAdapter::BroadcastSettings>,
    >,
) -> Vec<BoxFuture<'fut, ()>>
where
    NodeId: Eq + Hash + 'static,
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId> + Sync,
    NetAdapter: NetworkAdapter<RuntimeServiceId, BroadcastSettings: Eq + Hash> + Sync,
{
    processed_messages_to_release
        .into_iter()
        .inspect(|processed_message_to_release| {
            if let Some(state_updater) = state_updater.as_mut()
                && state_updater.remove_sent_processed_message(processed_message_to_release).is_err() {
                tracing::warn!(target: LOG_TARGET, "Previously processed message should be present in the recovery state but was not found.");
            }
        })
        .map(
            |processed_message_to_release| -> BoxFuture<'fut, ()> {
                match processed_message_to_release {
                    ProcessedMessage::Network(NetworkMessage {
                        broadcast_settings,
                        message,
                    }) => network_adapter.broadcast(message, broadcast_settings).boxed(),
                    ProcessedMessage::Encapsulated(encapsulated_message) => {
                        backend.publish(*encapsulated_message).boxed()
                    }
                }
            },
        ).collect()
}

/// Generate and encapsulate a cover message. Then, try to locally decapsulate
/// the outermost `N` layers that have the local node as the intended recipient.
///
/// If all layers are removed, the blending tokens are collected and `None` is
/// returned. Else, `Some` with all or the remaining encapsulation layers, with
/// the blending tokens collected in the `state_updater`.
async fn generate_and_try_to_decapsulate_cover_message<
    NodeId,
    BackendSettings,
    BroadcastSettings,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
>(
    cryptographic_processor: &mut CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    state_updater: &mut state::StateUpdater<BackendSettings, BroadcastSettings>,
) -> Option<EncapsulatedMessage>
where
    NodeId: Eq + Hash + 'static,
    BackendSettings: Sync,
    BroadcastSettings: Sync,
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
{
    let encapsulated_cover_message = cryptographic_processor
        .encapsulate_cover_payload(&random_sized_bytes::<{ size_of::<u32>() }>())
        .await
        .expect("Should not fail to generate new cover message");
    let self_decapsulation_output =
        cryptographic_processor.decapsulate_message_recursive(encapsulated_cover_message.clone());
    let Ok(multi_layer_decapsulation_output) = self_decapsulation_output else {
        // First layer not addressed to ourselves. Publish as regular cover message,
        // hence we consume a core quota.
        tracing::debug!(target: LOG_TARGET, "Locally generated cover message does not have its outermost layer addressed to us. Sending it out fully encapsulated...");
        state_updater.consume_core_quota(1);
        return Some(encapsulated_cover_message.into());
    };
    let (blending_tokens, message_type) = multi_layer_decapsulation_output.into_components();

    state_updater.collect_current_session_tokens(blending_tokens.into_iter());

    match message_type {
        // This is the initial message that was encapsulated, since we fully
        // decapsulated a cover message, we don't do anything.
        DecapsulatedMessageType::Completed(_) => None,
        DecapsulatedMessageType::Incompleted(remaining_encapsulated_message) => {
            Some(*remaining_encapsulated_message)
        }
    }
}

/// Handle a clock event by calling into the epoch handler and process the
/// resulting epoch event, if any.
///
/// On a new epoch, it will update the cryptographic processor and the current
/// `PoQ` public inputs. At the end of an epoch transition period, it will
/// interact with the Blend components to communicate the end of such transition
/// period.
async fn handle_clock_event<
    NodeId,
    ProofsGenerator,
    ProofsVerifier,
    ChainService,
    Backend,
    Rng,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    slot_tick: SlotTick,
    settings: &BlendConfig<Backend::Settings>,
    epoch_handler: &mut EpochHandler<ChainService, RuntimeServiceId>,
    cryptographic_processor: &mut CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    backend: &mut Backend,
    current_public_info: PublicInfo<NodeId>,
) -> PublicInfo<NodeId>
where
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
    ChainService: ChainApi<RuntimeServiceId> + Sync,
    Backend: BlendBackend<NodeId, Rng, ProofsVerifier, RuntimeServiceId>,
    RuntimeServiceId: Sync,
{
    let Some(epoch_event) = epoch_handler.tick(slot_tick).await else {
        return current_public_info;
    };

    match epoch_event {
        EpochEvent::NewEpoch(LeaderInputsMinusQuota {
            pol_epoch_nonce,
            pol_ledger_aged,
            total_stake,
        }) => {
            let new_leader_inputs = LeaderInputs {
                message_quota: settings.crypto.num_blend_layers.into(),
                pol_epoch_nonce,
                pol_ledger_aged,
                total_stake,
            };
            let new_public_info = PublicInfo {
                epoch: new_leader_inputs,
                ..current_public_info
            };

            cryptographic_processor.rotate_epoch(new_leader_inputs);
            backend.rotate_epoch(new_leader_inputs).await;

            new_public_info
        }
        EpochEvent::OldEpochTransitionPeriodExpired => {
            cryptographic_processor.complete_epoch_transition();
            backend.complete_epoch_transition().await;

            current_public_info
        }
        EpochEvent::NewEpochAndOldEpochTransitionExpired(LeaderInputsMinusQuota {
            pol_epoch_nonce,
            pol_ledger_aged,
            total_stake,
        }) => {
            let new_leader_inputs = LeaderInputs {
                message_quota: settings.crypto.num_blend_layers.into(),
                pol_epoch_nonce,
                pol_ledger_aged,
                total_stake,
            };
            let new_public_inputs = PublicInfo {
                epoch: new_leader_inputs,
                ..current_public_info
            };

            // Complete transition of previous epoch, then set the current epoch as the old
            // one and move to the new one.
            cryptographic_processor.complete_epoch_transition();
            backend.complete_epoch_transition().await;
            cryptographic_processor.rotate_epoch(new_leader_inputs);
            backend.rotate_epoch(new_leader_inputs).await;

            new_public_inputs
        }
    }
}

/// Handle the availability of new secret `PoL` info by passing it to the
/// underlying cryptographic processor.
fn handle_new_secret_epoch_info<NodeId, ProofsGenerator, ProofsVerifier, CorePoQGenerator>(
    new_pol_info: ProofOfLeadershipQuotaInputs,
    cryptographic_processor: &mut CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
) where
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
{
    cryptographic_processor.set_epoch_private(new_pol_info);
}

/// Submits an activity proof to the SDP service.
async fn submit_activity_proof(
    proof: ActivityProof,
    sdp_relay: &OutboundRelay<SdpMessage>,
) -> Result<(), RelayError> {
    info!(target: LOG_TARGET, "Submitting activity proof for the old session: {proof:?}");
    sdp_relay
        .send(SdpMessage::PostActivity {
            metadata: ActivityMetadata::Blend(Box::new((&proof).into())),
        })
        .await
        .map_err(|(e, _)| e)
}
