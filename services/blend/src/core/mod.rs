use std::{
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    time::Duration,
};

use async_trait::async_trait;
use backends::BlendBackend;
use fork_stream::StreamExt as _;
use futures::{
    FutureExt as _, Stream, StreamExt as _,
    future::{BoxFuture, join_all},
};
use lb_blend::{
    crypto::random_sized_bytes,
    message::{
        Error as MessageError, PayloadType,
        encap::{
            ProofsVerifier as ProofsVerifierTrait,
            encapsulated::EncapsulatedMessage,
            validated::{
                EncapsulatedMessageWithVerifiedPublicHeader,
                EncapsulatedMessageWithVerifiedSignature,
            },
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
        message_blend::{
            crypto::SessionCryptographicProcessorSettings,
            provers::core_and_leader::CoreAndLeaderProofsGenerator,
        },
        message_scheduler::{
            OldSessionMessageScheduler, ProcessedMessageScheduler,
            round_info::{RoundInfo, RoundReleaseType},
            session_info::SessionInfo as SchedulerSessionInfo,
        },
        session::{SessionEvent, UninitializedSessionEventStream},
        stream::UninitializedFirstReadyStream,
    },
};
use lb_chain_service::{
    Epoch,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_core::{
    codec::{DeserializeOp as _, SerializeOp as _},
    sdp::ActivityMetadata,
};
use lb_key_management_system_service::{
    api::KmsServiceApi,
    keys::{KeyOperators, PublicKeyEncoding},
    operators::ed25519::exfiltrate_secret_key::LeakSecretKeyOperator,
};
use lb_log_targets::blend;
use lb_network_service::NetworkService;
use lb_sdp_service::SdpMessage;
use lb_services_utils::{
    overwatch::{JsonFileBackend, RecoveryOperator},
    wait_until_services_are_ready,
};
use lb_time_service::{SlotTick, TimeService, TimeServiceMessage};
use lb_utils::blake_rng::BlakeRng;
use network::NetworkAdapter;
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
use tokio::sync::oneshot;
use tracing::{debug, error, info, trace};

use crate::{
    core::{
        backends::{PublicInfo, SessionInfo},
        kms::{KmsPoQAdapter, PreloadKMSBackendCorePoQGenerator},
        processor::{
            CoreCryptographicProcessor, DecapsulatedMessageType, Error,
            MultiLayerDecapsulationOutput,
        },
        scheduler::SchedulerWrapper,
        settings::{RunningBlendConfig, StartingBlendConfig},
        state::{RecoveryServiceState, ServiceState, StateUpdater as ServiceStateUpdater},
    },
    epoch_info::{
        ChainApi, EpochEvent, EpochHandler, LeaderInputsMinusQuota, PolEpochInfo,
        PolInfoProvider as PolInfoProviderTrait,
    },
    kms::PreloadKmsService,
    membership::{self, MembershipInfo, ZkInfo},
    message::{NetworkMessage, ProcessedMessage, ServiceMessage},
    session::{CoreSessionInfo, CoreSessionPublicInfo, MaybeEmptyCoreSessionInfo},
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

const LOG_TARGET: &str = blend::service::CORE;

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
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId>,
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
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId>,
    Network: NetworkAdapter<RuntimeServiceId>,
{
    type Settings = StartingBlendConfig<Backend::Settings>;
    type State = RecoveryServiceState<Backend::Settings, Network::BroadcastSettings>;
    type StateOperator = RecoveryOperator<
        JsonFileBackend<
            RecoveryServiceState<Backend::Settings, Network::BroadcastSettings>,
            StartingBlendConfig<Backend::Settings>,
        >,
    >;
    type Message = ServiceMessage<Network::BroadcastSettings, NodeId>;
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
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId> + Send + Sync,
    NodeId: Clone + Debug + Send + Eq + Hash + Sync + 'static,
    Network: NetworkAdapter<RuntimeServiceId, BroadcastSettings: Eq + Hash + Unpin> + Send + Sync,
    MembershipAdapter: membership::Adapter<NodeId = NodeId, Error: Send + Sync + 'static> + Send,
    membership::ServiceMessage<MembershipAdapter>: Send + Sync + 'static,
    ProofsGenerator: CoreAndLeaderProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>>
        + Send
        + Sync,
    SdpService: ServiceData<Message = SdpMessage> + Send,
    ProofsVerifier: ProofsVerifierTrait + Clone + Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend + Send,
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
            Some(Duration::from_mins(1)),
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

        // TODO: This will go once we do not need to pass the secret key anymore, i.e.,
        // when we have libp2p integration with KMS.
        let non_ephemeral_signing_key = {
            let (sender, receiver) = oneshot::channel();
            kms_api
                .execute(
                    blend_config.non_ephemeral_signing_key_id.clone(),
                    KeyOperators::Ed25519(Box::new(LeakSecretKeyOperator::new(sender))),
                )
                .await
                .expect("Failed to interact with KMS to fetch non-ephemeral signing key.");
            receiver
                .await
                .expect("Failed to retrieve non-ephemeral signing key from KMS.")
        };

        let membership_stream = MembershipAdapter::new(
            overwatch_handle
                .relay::<<MembershipAdapter as membership::Adapter>::Service>()
                .await
                .expect("Failed to get relay channel with membership service."),
            non_ephemeral_signing_key.public_key(),
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
        let running_blend_config = RunningBlendConfig {
            backend: blend_config.backend,
            non_ephemeral_signing_key,
            num_blend_layers: blend_config.num_blend_layers,
            minimum_network_size: blend_config.minimum_network_size,
            recovery_path: blend_config.recovery_path.clone(),
            scheduler: blend_config.scheduler,
            time: blend_config.time,
            zk: blend_config.zk,
            data_replication_factor: blend_config.data_replication_factor,
            activity_threshold_sensitivity: blend_config.activity_threshold_sensitivity,
        };
        let (
            mut remaining_session_stream,
            mut remaining_clock_stream,
            current_public_info,
            current_epoch,
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
            running_blend_config.clone(),
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
        // old session's components (crypto processor, scheduler, blending token
        // collector, public info, and epoch) are returned for the retirement phase.
        let (
            old_session_crypto_processor,
            old_session_message_scheduler,
            old_session_blending_token_collector,
            old_session_public_info,
            old_epoch,
        ) = run_event_loop(
            inbound_relay,
            &mut blend_messages,
            &mut remaining_clock_stream,
            secret_pol_info_stream,
            &mut remaining_session_stream,
            &running_blend_config,
            &mut backend,
            &network_adapter,
            &sdp_relay,
            &mut epoch_handler,
            message_scheduler.into(),
            &mut rng,
            crypto_processor,
            current_public_info,
            current_epoch,
            current_recovery_checkpoint,
        )
        .await;

        // The main event loop has ended because the node is no longer a core node
        // in the new session.
        // Before terminating the service, complete the old session during a single
        // session transition period.
        retire(
            // We don't need session numbers anymore since we know we are dealing with a single,
            // past session.
            blend_messages.map(|(message, _)| message),
            remaining_clock_stream,
            remaining_session_stream,
            &running_blend_config,
            backend,
            network_adapter,
            sdp_relay,
            epoch_handler,
            old_session_message_scheduler,
            rng,
            old_session_blending_token_collector,
            old_session_crypto_processor,
            old_session_public_info,
            old_epoch,
        )
        .await;

        Ok(())
    }
}

/// Initialize the components for the [`BlendService`].
#[expect(clippy::too_many_lines, reason = "Need to initialize many components")]
#[expect(
    clippy::too_many_arguments,
    reason = "Need to initialize many components."
)]
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
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
    blend_config: RunningBlendConfig<Backend::Settings>,
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
    impl Stream<Item = SessionEvent<MaybeEmptyCoreSessionInfo<NodeId, KmsAdapter::CorePoQGenerator>>>
    + Unpin
    + Send
    + 'static,
    impl Stream<Item = SlotTick> + Unpin + Send + Sync + 'static,
    PublicInfo<NodeId>,
    Epoch,
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
    NodeId: Clone + Debug + Eq + Hash + Send + 'static,
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId> + Sync,
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
                      zk,
                  }| {
                // This can be empty in case of an empty membership set.
                let Some(ZkInfo {
                    root,
                    core_and_path_selectors,
                }) = zk
                else {
                    return MaybeEmptyCoreSessionInfo::Empty {
                        session: session_number,
                    };
                };
                // `None` when the local node is not part of the session membership. This can
                // happen when the node transitions from core to edge mode.
                let core_poq_generator = core_and_path_selectors.map(|selectors| {
                    kms_adapter.core_poq_generator(zk_sk_id.clone(), Box::new(selectors))
                });
                CoreSessionInfo {
                    public: CoreSessionPublicInfo {
                        poq_core_public_inputs: CoreInputs {
                            quota: config.session_core_quota(membership.size()),
                            zk_root: root,
                        },
                        membership,
                        session: session_number,
                    },
                    core_poq_generator,
                }
                .into()
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
        let MaybeEmptyCoreSessionInfo::NonEmpty(core_session_info) = membership_info else {
            panic!("First retrieved session for Blend core startup must be available.");
        };
        (core_session_info, remaining_session_stream.fork())
    })
    .expect("The current session info must be available.");

    let (
        (
            LeaderInputsMinusQuota {
                pol_epoch_nonce,
                pol_ledger_aged,
                lottery_0,
                lottery_1,
            },
            current_epoch,
        ),
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
        "The current membership is ready: {:?}",
        current_membership_info.public
    );

    let current_public_info = PublicInfo {
        epoch: LeaderInputs {
            pol_ledger_aged,
            pol_epoch_nonce,
            message_quota: blend_config.session_leadership_quota(),
            lottery_0,
            lottery_1,
        },
        session: SessionInfo {
            membership: current_membership_info.public.membership.clone(),
            session_number: current_membership_info.public.session,
            core_public_inputs: current_membership_info.public.poq_core_public_inputs,
        },
    };

    trace!(target: LOG_TARGET, "Current public info: {:?}", current_public_info);

    let crypto_processor = CoreCryptographicProcessor::<
        _,
        KmsAdapter::CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >::try_new_with_core_condition_check(
        current_membership_info.public.membership.clone(),
        blend_config.minimum_network_size,
        SessionCryptographicProcessorSettings {
            non_ephemeral_encryption_key: blend_config.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: blend_config.num_blend_layers,
        },
        current_public_info.clone().into(),
        current_membership_info
            .core_poq_generator
            .expect("Core PoQ generator must be present at startup: the proxy service only launches CoreMode when the node is part of the core membership."),
        current_epoch,
    )
    .expect("The initial membership should satisfy the core node condition");

    // Initialize the current session state. If the session matches the stored one,
    // retrieves the tracked consumed core quota. Else, fallback to `0`.
    let current_recovery_checkpoint = if let Some(saved_state) = last_saved_state.take()
        && saved_state.last_seen_session() == current_membership_info.public.session
    {
        tracing::trace!(
            target: LOG_TARGET,
            "Found recovery state for session {:?}: {saved_state:?}",
            current_membership_info.public.session
        );
        saved_state
    } else {
        tracing::trace!(
            target: LOG_TARGET,
            "No recovery state found for session {:?}. Initializing a new one.",
            current_membership_info.public.session
        );

        ServiceState::with_session(
            current_membership_info.public.session,
            SessionBlendingTokenCollector::new(
                &reward::SessionInfo::new(
                    current_membership_info.public.session,
                    &pol_epoch_nonce,
                    current_membership_info.public.membership.size() as u64,
                    current_membership_info.public.poq_core_public_inputs.quota,
                    blend_config.activity_threshold_sensitivity,
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
        tracing::debug!(target: LOG_TARGET, "Old session token collector loaded. Computing activity proof");
        compute_and_submit_activity_proof(old_session_token_collector, sdp_relay).await;
    }
    let current_recovery_checkpoint = state_updater.commit_changes();

    let message_scheduler = SchedulerWrapper::new_with_initial_messages(
        SchedulerSessionInfo {
            core_quota: blend_config
                .session_core_quota(current_membership_info.public.membership.size())
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
        current_epoch,
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
//
// The tracked `epoch` is updated by both clock events and secret PoL info
// events (whichever arrives first), and guards against duplicate epoch
// rotations in the cryptographic processor.
//
// Returns the old session components when the node is no longer a core node.
#[expect(clippy::too_many_arguments, reason = "categorize args")]
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this at some point"
)]
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
    mut inbound_relay: impl Stream<Item = ServiceMessage<NetAdapter::BroadcastSettings, NodeId>>
    + Send
    + Unpin,
    blend_messages: &mut (
             impl Stream<Item = (EncapsulatedMessageWithVerifiedSignature, u64)> + Send + Unpin + 'static
         ),
    remaining_clock_stream: &mut (impl Stream<Item = SlotTick> + Send + Sync + Unpin + 'static),
    mut secret_pol_info_stream: impl Stream<Item = PolEpochInfo> + Send + Unpin,
    remaining_session_stream: &mut (
             impl Stream<Item = SessionEvent<MaybeEmptyCoreSessionInfo<NodeId, CorePoQGenerator>>>
             + Unpin
             + Send
         ),

    blend_config: &RunningBlendConfig<Backend::Settings>,
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
    mut epoch: Epoch,
    mut recovery_checkpoint: ServiceState<Backend::Settings, NetAdapter::BroadcastSettings>,
) -> (
    CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    OldSessionMessageScheduler<Rng, ProcessedMessage<NetAdapter::BroadcastSettings>>,
    OldSessionBlendingTokenCollector,
    PublicInfo<NodeId>,
    Epoch,
)
where
    NodeId: Clone + Eq + Hash + Send + Sync + 'static,
    Rng: rand::Rng + Clone + Send + Unpin,
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId> + Sync + Send,
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
    ChainService: ChainApi<RuntimeServiceId> + Sync + Send,
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator> + Send + Sync,
    CorePoQGenerator: Send + Sync,
    ProofsVerifier: ProofsVerifierTrait + Send + Sync,
    RuntimeServiceId: Sync + Send,
{
    // An optional crypto processor to handle the old session during transition
    // period.
    let mut old_session_crypto_processor: Option<
        CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    > = None;
    let mut old_session_message_scheduler: Option<
        OldSessionMessageScheduler<Rng, ProcessedMessage<NetAdapter::BroadcastSettings>>,
    > = None;
    let mut current_secret_pol_info: Option<ProofOfLeadershipQuotaInputs> = None;

    loop {
        tokio::select! {
            Some(msg) = inbound_relay.next() => {
                match msg {
                    ServiceMessage::Blend(message_payload) => {
                        // We serialize here, outside of the handler function, so that we can serialize only once for all replicas.
                        let serialized_data_message = NetworkMessage::<NetAdapter::BroadcastSettings>::to_bytes(&message_payload).expect("NetworkMessage should be able to be serialized");

                        let message_copies = blend_config.data_replication_factor.checked_add(1).unwrap();
                        for _ in 0..message_copies {
                            recovery_checkpoint = handle_serialized_local_data_message(&serialized_data_message, &mut crypto_processor, &mut message_scheduler, recovery_checkpoint).await;
                        }
                    }
                    ServiceMessage::GetNetworkInfo { reply } => {
                        let info = backend.network_info().await;
                        drop(reply.send(info));
                    }
                }
            }
            Some(incoming_message) = blend_messages.next() => {
                recovery_checkpoint = handle_incoming_blend_message(incoming_message, &mut message_scheduler, old_session_message_scheduler.as_mut(), &crypto_processor, old_session_crypto_processor.as_ref(),  recovery_checkpoint);
            }
            Some(round_info) = message_scheduler.next() => {
                recovery_checkpoint = handle_release_round(round_info, &mut crypto_processor, rng, backend, network_adapter, recovery_checkpoint).await;
            }
            Some((Some(processed_messages_to_release), previous_session_number)) = async {
                match (&mut old_session_message_scheduler, &old_session_crypto_processor) {
                    (Some(old_scheduler), Some(old_crypto_processor)) => {
                        Some((old_scheduler.next().await, old_crypto_processor.session()))
                    },
                    _ => None
                }
            } => {
                handle_release_round_for_old_session(processed_messages_to_release, rng, backend, network_adapter, previous_session_number).await;
            }
            Some(clock_tick) = remaining_clock_stream.next() => {
                (public_info, epoch) = handle_clock_event(clock_tick, blend_config, epoch_handler, &mut crypto_processor, public_info, epoch).await;
            }
            Some(pol_info) = secret_pol_info_stream.next() => {
                if let Some(new_leader_inputs) = handle_new_secret_epoch_info(blend_config, &pol_info, &mut crypto_processor, epoch) {
                    epoch = pol_info.epoch;
                    public_info.epoch = new_leader_inputs;
                }
                current_secret_pol_info = Some(pol_info.poq_private_inputs);
            }
            Some(session_event) = remaining_session_stream.next() => {
                match handle_session_event(session_event, blend_config, crypto_processor, message_scheduler, public_info, recovery_checkpoint, backend, sdp_relay, epoch, current_secret_pol_info.as_ref()).await {
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
                            epoch,
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
    mut blend_messages: impl Stream<Item = EncapsulatedMessageWithVerifiedSignature>
    + Send
    + Unpin
    + 'static,
    mut remaining_clock_stream: impl Stream<Item = SlotTick> + Send + Sync + Unpin + 'static,
    mut remaining_session_stream: impl Stream<
        Item = SessionEvent<MaybeEmptyCoreSessionInfo<NodeId, CorePoQGenerator>>,
    > + Unpin,
    blend_config: &RunningBlendConfig<Backend::Settings>,
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
    mut epoch: Epoch,
) where
    NodeId: Clone + Eq + Hash + Send + 'static,
    Rng: rand::Rng + Clone + Send + Unpin,
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId> + Sync,
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
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator> + Sync,
    ProofsVerifier: ProofsVerifierTrait,
    RuntimeServiceId: Sync,
{
    loop {
        tokio::select! {
            Some(incoming_message) = blend_messages.next() => {
                handle_incoming_blend_message_from_old_session(incoming_message, &mut message_scheduler, &crypto_processor, &mut blending_token_collector);
            }
            Some(processed_messages_to_release) = message_scheduler.next() => {
                handle_release_round_for_old_session(processed_messages_to_release, &mut rng, &backend, &network_adapter, crypto_processor.session()).await;
            }
            Some(clock_tick) = remaining_clock_stream.next() => {
                (public_info, epoch) = handle_clock_event(clock_tick, blend_config, &mut epoch_handler, &mut crypto_processor, public_info, epoch).await;
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
#[expect(clippy::too_many_lines, reason = "necessary for session handling")]
#[expect(
    clippy::cognitive_complexity,
    reason = "necessary for session handling"
)]
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
    event: SessionEvent<MaybeEmptyCoreSessionInfo<NodeId, CorePoQGenerator>>,
    settings: &RunningBlendConfig<Backend::Settings>,
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
    current_epoch: Epoch,
    current_secret_info: Option<&ProofOfLeadershipQuotaInputs>,
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
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId>,
{
    match event {
        SessionEvent::NewSession(MaybeEmptyCoreSessionInfo::NonEmpty(CoreSessionInfo {
            core_poq_generator,
            public:
                CoreSessionPublicInfo {
                    poq_core_public_inputs: new_core_public_inputs,
                    session: new_session,
                    membership: new_membership,
                },
        })) => {
            let (_, _, _, _, current_session_blending_token_collector, _, state_updater) =
                current_recovery_checkpoint.into_components();

            let new_reward_session_info = reward::SessionInfo::new(
                new_session,
                &current_public_info.epoch.pol_epoch_nonce,
                new_membership.size() as u64,
                new_core_public_inputs.quota,
                settings.activity_threshold_sensitivity,
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
                core_quota: settings.session_core_quota(new_session_info.membership.size()),
                session_number: u128::from(new_session).into(),
            };

            let new_public_info = PublicInfo {
                session: new_session_info.clone(),
                ..current_public_info
            };
            let Some(core_poq_generator) = core_poq_generator else {
                tracing::info!(target: LOG_TARGET, "Local node is not part of new membership. Retiring from core.");
                return HandleSessionEventOutput::Retiring {
                    old_crypto_processor: current_cryptographic_processor,
                    old_scheduler: current_scheduler
                        .rotate_session(new_scheduler_session_info, settings.scheduler_settings())
                        .1,
                    old_token_collector: old_session_blending_token_collector,
                    old_public_info: current_public_info,
                };
            };

            let new_processor = match CoreCryptographicProcessor::try_new_with_core_condition_check(
                new_membership,
                settings.minimum_network_size,
                SessionCryptographicProcessorSettings {
                    non_ephemeral_encryption_key: settings
                        .non_ephemeral_signing_key
                        .derive_x25519(),
                    num_blend_layers: settings.num_blend_layers,
                },
                new_public_info.clone().into(),
                core_poq_generator,
                current_epoch,
            ) {
                Ok(mut new_processor) => {
                    if let Some(current_secret_info) = current_secret_info {
                        new_processor.set_epoch_private(
                            current_secret_info.clone(),
                            current_public_info.epoch,
                            current_epoch,
                        );
                    }
                    new_processor
                }
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
        SessionEvent::NewSession(MaybeEmptyCoreSessionInfo::Empty { session }) => {
            tracing::info!(target: LOG_TARGET, "New session event received, but no session info is available due to empty membership set.");
            let (_, _, _, _, current_session_blending_token_collector, _, _) =
                current_recovery_checkpoint.into_components();
            let new_reward_session_info = reward::SessionInfo::new(
                session,
                &current_public_info.epoch.pol_epoch_nonce,
                0,
                0,
                settings.activity_threshold_sensitivity,
            )
            .expect("Reward session info must be created successfully. Panicking since the service cannot continue with this session");
            let (_, old_session_blending_token_collector) =
                current_session_blending_token_collector.rotate_session(&new_reward_session_info);
            HandleSessionEventOutput::Retiring {
                old_crypto_processor: current_cryptographic_processor,
                old_scheduler: current_scheduler.consume(),
                old_token_collector: old_session_blending_token_collector,
                old_public_info: current_public_info,
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
async fn handle_session_transition_expired<Backend, NodeId, Rng, RuntimeServiceId>(
    backend: &mut Backend,
    blending_token_collector: OldSessionBlendingTokenCollector,
    sdp_relay: &OutboundRelay<SdpMessage>,
) where
    Backend: BlendBackend<NodeId, Rng, RuntimeServiceId>,
    NodeId: Eq + Hash + Clone + Send,
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
        debug!(target: LOG_TARGET, "No activity proof generated for the old session");
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

/// Processes an already-serialized local data message from another service.
///
/// The serialized payload is encapsulated with blend layers. Before scheduling,
/// the outermost layers addressed to this node are self-decapsulated so that
/// blending tokens are collected immediately and only the remaining layers (or
/// the fully unwrapped message) are scheduled for the next release round.
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
async fn handle_serialized_local_data_message<
    NodeId,
    Rng,
    BackendSettings,
    BroadcastSettings,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
>(
    serialized_local_data_message: &[u8],
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
    let Ok(wrapped_message) = cryptographic_processor
        .encapsulate_data_payload(serialized_local_data_message)
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
        DecapsulatedMessageType::Completed(fully_decapsulated_message) => {
            assert!(
                fully_decapsulated_message.payload_type() == PayloadType::Data,
                "Locally-generated and fully-decapsulated message should be a data message."
            );
            let deserialized_data_message =
                NetworkMessage::from_bytes(fully_decapsulated_message.payload_body())
                    .expect("Locally-generated and serialized message should be deserializable.");
            tracing::trace!(target: LOG_TARGET, "Locally generated data message {deserialized_data_message:?} had all the {} layers addressed to this same node. Propagating only the fully decapsulated message.", blending_tokens.len());
            ProcessedMessage::from(deserialized_data_message)
        }
        DecapsulatedMessageType::Incompleted(remaining_encapsulated_message) => {
            tracing::trace!(target: LOG_TARGET, "Locally generated data message had the outermost {} layers addressed to this same node. Propagating only the remaining encapsulated layers.", blending_tokens.len());
            // Locally-generated message, so we know it's valid.
            ProcessedMessage::from(
                EncapsulatedMessageWithVerifiedPublicHeader::from_message_unchecked(
                    *remaining_encapsulated_message,
                ),
            )
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

/// Processes an incoming Blend message (with verified signature) received
/// from a core or edge peer.
///
/// Decapsulation is attempted with the current or old session's cryptographic
/// processor depending on the session the message is coming from.
fn handle_incoming_blend_message<
    NodeId,
    Rng,
    BroadcastSettings,
    BackendSettings,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
>(
    (validated_encapsulated_message, session): (EncapsulatedMessageWithVerifiedSignature, u64),
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
    if session == cryptographic_processor.session() {
        let Some(output) = try_validate_and_decapsulate(
            validated_encapsulated_message,
            cryptographic_processor,
            session,
        ) else {
            return current_recovery_checkpoint;
        };
        handle_decapsulated_incoming_message_from_current_session(
            output,
            scheduler,
            current_recovery_checkpoint,
            cryptographic_processor,
        )
    } else if let Some(old_cryptographic_processor) = old_session_cryptographic_processor
        && session == old_cryptographic_processor.session()
    {
        let Some(output) = try_validate_and_decapsulate(
            validated_encapsulated_message,
            old_cryptographic_processor,
            session,
        ) else {
            return current_recovery_checkpoint;
        };
        handle_decapsulated_incoming_message_from_old_session(
            output,
            old_session_scheduler
                .expect("Old session scheduler should be available when old session crypto processor is available"),
            current_recovery_checkpoint,
            old_cryptographic_processor,
        )
    } else {
        tracing::debug!(target: LOG_TARGET, "Received message for session {session} that is not currently handled. Ignoring...");
        current_recovery_checkpoint
    }
}

/// Validates the `PoQ` of a received message and attempts recursive
/// decapsulation. Returns `None` if validation or decapsulation fails (already
/// logged).
fn try_validate_and_decapsulate<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>(
    message: EncapsulatedMessageWithVerifiedSignature,
    processor: &CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    session: u64,
) -> Option<MultiLayerDecapsulationOutput>
where
    ProofsVerifier: ProofsVerifierTrait,
{
    let Ok(validated_message) = processor.validate_message_poq(message) else {
        tracing::debug!(target: LOG_TARGET, "Received message for session {session} failed PoQ validation. Ignoring...");
        return None;
    };
    match processor.decapsulate_message_recursive(validated_message) {
        Ok(output) => Some(output),
        Err(e) => {
            if matches!(e, MessageError::PrivateHeaderDeserializationFailed) {
                tracing::trace!(target: LOG_TARGET, "Failed to decapsulate received message for session {session} due to deserialization error. This can happen when the message was intended for another node or when the message is malformed. Ignoring...");
            } else {
                tracing::debug!(target: LOG_TARGET, "Failed to decapsulate received message for session {session}: {e:?}.");
            }
            None
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
    validated_encapsulated_message: EncapsulatedMessageWithVerifiedSignature,
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
    match cryptographic_processor
        .validate_message_poq(validated_encapsulated_message)
        .and_then(|message_with_verified_header| {
            cryptographic_processor.decapsulate_message_recursive(message_with_verified_header)
        }) {
        Ok(output) => {
            let (_, blending_tokens) =
                schedule_decapsulated_incoming_message(output, scheduler, cryptographic_processor);
            for blending_token in blending_tokens {
                blending_token_collector.collect(blending_token);
            }
        }
        Err(e) => {
            if matches!(e, MessageError::PrivateHeaderDeserializationFailed) {
                tracing::trace!(target: LOG_TARGET, "Failed to decapsulate received message from old session due to deserialization error. This can happen when the message was intended for another node or when the message is malformed. Ignoring...");
            } else {
                tracing::debug!(target: LOG_TARGET, "Failed to decapsulate received message from old session: {e:?}");
            }
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
    NodeId,
    CorePoQGenerator,
    ProofsGenerator,
    ProofsVerifier,
>(
    multi_layer_decapsulation_output: MultiLayerDecapsulationOutput,
    scheduler: &mut SessionMessageScheduler<
        Rng,
        ProcessedMessage<BroadcastSettings>,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    current_recovery_checkpoint: ServiceState<BackendSettings, BroadcastSettings>,
    cryptographic_processor: &CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
) -> ServiceState<BackendSettings, BroadcastSettings>
where
    BroadcastSettings: Serialize + for<'de> Deserialize<'de> + Debug + Eq + Hash + Clone + Send,
    BackendSettings: Clone,
    ProofsVerifier: ProofsVerifierTrait,
{
    let mut state_updater = current_recovery_checkpoint.start_updating();

    let (maybe_processed_message, blending_tokens) = schedule_decapsulated_incoming_message(
        multi_layer_decapsulation_output,
        scheduler,
        cryptographic_processor,
    );

    if let Some(processed_message) = maybe_processed_message {
        state_updater
            .add_unsent_processed_message(processed_message)
            .expect("Swarm should bubble up unique messages only.");
    }

    state_updater.collect_current_session_tokens(blending_tokens);
    state_updater.commit_changes()
}

/// Schedules a decapsulated incoming message from the old session,
/// and collects the blending tokens obtained from the decapsulation.
///
/// It updates the recovery checkpoint by storing the collected tokens.
fn handle_decapsulated_incoming_message_from_old_session<
    Rng,
    BroadcastSettings,
    BackendSettings,
    NodeId,
    CorePoQGenerator,
    ProofsGenerator,
    ProofsVerifier,
>(
    multi_layer_decapsulation_output: MultiLayerDecapsulationOutput,
    scheduler: &mut OldSessionMessageScheduler<Rng, ProcessedMessage<BroadcastSettings>>,
    recovery_checkpoint: ServiceState<BackendSettings, BroadcastSettings>,
    old_cryptographic_processor: &CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
) -> ServiceState<BackendSettings, BroadcastSettings>
where
    BroadcastSettings: Serialize + for<'de> Deserialize<'de> + Debug + Eq + Hash + Clone + Send,
    BackendSettings: Clone,
    ProofsVerifier: ProofsVerifierTrait,
{
    let (_, blending_tokens) = schedule_decapsulated_incoming_message(
        multi_layer_decapsulation_output,
        scheduler,
        old_cryptographic_processor,
    );

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
    reason = "TODO: address this in a dedicated refactor"
)]
fn schedule_decapsulated_incoming_message<
    BroadcastSettings,
    NodeId,
    CorePoQGenerator,
    ProofsGenerator,
    ProofsVerifier,
>(
    multi_layer_decapsulation_output: MultiLayerDecapsulationOutput,
    scheduler: &mut impl ProcessedMessageScheduler<ProcessedMessage<BroadcastSettings>>,
    cryptographic_processor: &CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
) -> (
    Option<ProcessedMessage<BroadcastSettings>>,
    impl Iterator<Item = BlendingToken>,
)
where
    BroadcastSettings: Serialize + for<'de> Deserialize<'de> + Debug + Eq + Hash + Clone + Send,
    ProofsVerifier: ProofsVerifierTrait,
{
    let (blending_tokens, decapsulated_message_type) =
        multi_layer_decapsulation_output.into_components();
    tracing::trace!(
        target: LOG_TARGET,
        "Batch-decapsulated {} layers from the received message.",
        blending_tokens.len()
    );

    match decapsulated_message_type {
        DecapsulatedMessageType::Completed(fully_decapsulated_message) => {
            match fully_decapsulated_message.into_components() {
                (PayloadType::Cover, _) => {
                    tracing::trace!(target: LOG_TARGET, "Discarding received cover message.");
                    (None, blending_tokens.into_iter())
                }
                (PayloadType::Data, serialized_data_message) => {
                    tracing::trace!(target: LOG_TARGET, "Processing a fully decapsulated data message.");
                    match NetworkMessage::from_bytes(&serialized_data_message) {
                        Ok(deserialized_network_message) => {
                            tracing::trace!(
                                target: LOG_TARGET,
                                "Fully decapsulated and deserialized processed data message: {deserialized_network_message:?}"
                            );
                            let processed_message =
                                ProcessedMessage::from(deserialized_network_message);
                            scheduler.schedule_processed_message(processed_message.clone());
                            (Some(processed_message), blending_tokens.into_iter())
                        }
                        Err(e) => {
                            tracing::warn!(target: LOG_TARGET, "Unrecognized data message from blend backend. Dropping: {e:?}");
                            (None, blending_tokens.into_iter())
                        }
                    }
                }
            }
        }
        DecapsulatedMessageType::Incompleted(remaining_encapsulated_message) => {
            tracing::trace!(
                target: LOG_TARGET,
                "Processed encapsulated message: {remaining_encapsulated_message:?}"
            );
            let Ok(validated_message) =
                cryptographic_processor.validate_message_header(*remaining_encapsulated_message)
            else {
                tracing::debug!(target: LOG_TARGET, "Failed to validate the header of the remaining encapsulated message after decapsulation. Dropping...");
                return (None, blending_tokens.into_iter());
            };
            let processed_message = ProcessedMessage::from(validated_message);

            crate::metrics::mix_packets_processed_total();

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
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId> + Sync,
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
    let current_session = cryptographic_processor.session();

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
                backend.publish(data_message_to_blend, current_session).boxed()
            },
        ).collect::<Vec<_>>();

    let processed_messages_relay_futures = build_futures_to_release_processed_messages(
        processed_messages,
        backend,
        network_adapter,
        Some(&mut state_updater),
        current_session,
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
        message_futures.push(
            backend
                .publish(
                    // Locally-generated, so we know it's a valid one.
                    EncapsulatedMessageWithVerifiedPublicHeader::from_message_unchecked(
                        encapsulated_cover_message,
                    ),
                    current_session,
                )
                .boxed(),
        );
    }

    message_futures.shuffle(rng);

    // Release all messages concurrently, and wait for all of them to be sent.
    join_all(message_futures).await;
    log_release_window_summary(data_count, processed_count, cover_count);

    state_updater.commit_changes()
}

async fn handle_release_round_for_old_session<NodeId, Rng, Backend, NetAdapter, RuntimeServiceId>(
    processed_messages_to_release: Vec<ProcessedMessage<NetAdapter::BroadcastSettings>>,
    rng: &mut Rng,
    backend: &Backend,
    network_adapter: &NetAdapter,
    session_number: u64,
) where
    NodeId: Eq + Hash + 'static,
    Rng: RngCore + Send,
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId> + Sync,
    NetAdapter: NetworkAdapter<RuntimeServiceId, BroadcastSettings: Eq + Hash> + Sync,
{
    let mut futures = build_futures_to_release_processed_messages(
        processed_messages_to_release,
        backend,
        network_adapter,
        None,
        session_number,
    );
    futures.shuffle(rng);

    // Release all messages concurrently, and wait for all of them to be sent.
    let num_futures = futures.len();
    join_all(futures).await;
    log_old_session_release_summary(num_futures);
}

fn log_release_window_summary(data_count: usize, processed_count: usize, cover_count: usize) {
    if data_count > 0 || processed_count > 0 {
        tracing::debug!(
            target: LOG_TARGET,
            "Sent out {data_count} data, {processed_count} processed and {cover_count} cover messages at this release window."
        );
    } else {
        tracing::trace!(
            target: LOG_TARGET,
            "Sent out {data_count} data, {processed_count} processed and {cover_count} cover messages at this release window."
        );
    }
}

fn log_old_session_release_summary(num_futures: usize) {
    if num_futures > 0 {
        tracing::debug!(
            target: LOG_TARGET,
            "Sent out {num_futures} processed messages at this release window for the old session"
        );
    } else {
        tracing::trace!(
            target: LOG_TARGET,
            "Sent out {num_futures} processed messages at this release window for the old session"
        );
    }
}

fn build_futures_to_release_processed_messages<
    'fut,
    NodeId,
    Backend,
    NetAdapter,
    RuntimeServiceId,
>(
    processed_messages_to_release: Vec<ProcessedMessage<NetAdapter::BroadcastSettings>>,
    backend: &'fut Backend,
    network_adapter: &'fut NetAdapter,
    mut state_updater: Option<
        &mut ServiceStateUpdater<Backend::Settings, NetAdapter::BroadcastSettings>,
    >,
    session_number: u64,
) -> Vec<BoxFuture<'fut, ()>>
where
    NodeId: Eq + Hash + 'static,
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId> + Sync,
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
                        backend.publish(*encapsulated_message, session_number).boxed()
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
        tracing::trace!(target: LOG_TARGET, "Locally generated cover message does not have its outermost layer addressed to us. Sending it out fully encapsulated...");
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
/// On a new epoch, it updates the public info and conditionally rotates both
/// the cryptographic processor and the backend verifier. Both rotations are
/// guarded by `new_epoch > current_epoch` to avoid duplicates when the `PoL`
/// info handler in the event loop has already advanced to this epoch (and
/// already called `backend.rotate_epoch`). At the end of an epoch transition
/// period, it notifies the Blend components that the old epoch transition is
/// complete.
///
/// Returns the updated public info and the new tracked epoch.
async fn handle_clock_event<
    NodeId,
    ProofsGenerator,
    ProofsVerifier,
    ChainService,
    BackendSettings,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    slot_tick: SlotTick,
    settings: &RunningBlendConfig<BackendSettings>,
    epoch_handler: &mut EpochHandler<ChainService, RuntimeServiceId>,
    cryptographic_processor: &mut CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    current_public_info: PublicInfo<NodeId>,
    current_epoch: Epoch,
) -> (PublicInfo<NodeId>, Epoch)
where
    BackendSettings: Sync,
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
    ChainService: ChainApi<RuntimeServiceId> + Sync,
    RuntimeServiceId: Sync,
{
    let Some(epoch_event) = epoch_handler.tick(slot_tick).await else {
        return (current_public_info, current_epoch);
    };

    match epoch_event {
        EpochEvent::NewEpoch((
            LeaderInputsMinusQuota {
                pol_epoch_nonce,
                pol_ledger_aged,
                lottery_0,
                lottery_1,
            },
            new_epoch,
        )) => {
            tracing::debug!(target: LOG_TARGET, "New epoch {new_epoch:?} with nonce {pol_epoch_nonce:?} started");
            if new_epoch <= current_epoch {
                return (current_public_info, current_epoch);
            }

            // Only rotate if the PoL info handler hasn't already advanced
            // the crypto processor and backend verifier to this epoch.
            let new_leader_inputs = LeaderInputs {
                message_quota: settings.session_leadership_quota(),
                pol_epoch_nonce,
                pol_ledger_aged,
                lottery_0,
                lottery_1,
            };
            let new_public_info = PublicInfo {
                epoch: new_leader_inputs,
                ..current_public_info
            };

            // Only rotate if the PoL info handler hasn't already advanced
            // the crypto processor and backend verifier to this epoch.
            cryptographic_processor.rotate_epoch(new_leader_inputs, new_epoch);

            (new_public_info, new_epoch)
        }
        EpochEvent::OldEpochTransitionPeriodExpired => {
            tracing::debug!(target: LOG_TARGET, "Old epoch transition period expired.");
            cryptographic_processor.complete_epoch_transition();

            (current_public_info, current_epoch)
        }
        EpochEvent::NewEpochAndOldEpochTransitionExpired((
            LeaderInputsMinusQuota {
                pol_epoch_nonce,
                pol_ledger_aged,
                lottery_0,
                lottery_1,
            },
            new_epoch,
        )) => {
            tracing::debug!(target: LOG_TARGET, "New epoch {new_epoch:?} with nonce {pol_epoch_nonce:?} started and old epoch transition period expired.");
            if new_epoch <= current_epoch {
                return (current_public_info, current_epoch);
            }

            let new_leader_inputs = LeaderInputs {
                message_quota: settings.session_leadership_quota(),
                pol_epoch_nonce,
                pol_ledger_aged,
                lottery_0,
                lottery_1,
            };
            let new_public_inputs = PublicInfo {
                epoch: new_leader_inputs,
                ..current_public_info
            };

            // Complete the previous epoch's transition first, then rotate to
            // the new epoch (only if the PoL info handler hasn't already
            // advanced the crypto processor and backend verifier to this epoch).
            cryptographic_processor.complete_epoch_transition();
            cryptographic_processor.rotate_epoch(new_leader_inputs, new_epoch);

            (new_public_inputs, new_epoch)
        }
    }
}

/// Handle the availability of new secret `PoL` info by updating the
/// cryptographic processor.
///
/// If the secret info is for a new epoch that the clock handler hasn't
/// processed yet, the core proof generator and verifier are updated first
/// via [`CoreCryptographicProcessor::rotate_epoch`]. Then the leadership
/// proof generator is set with the received private inputs.
fn handle_new_secret_epoch_info<
    NodeId,
    ProofsGenerator,
    BackendSettings,
    ProofsVerifier,
    CorePoQGenerator,
>(
    settings: &RunningBlendConfig<BackendSettings>,
    new_pol_info: &PolEpochInfo,
    cryptographic_processor: &mut CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    current_epoch: Epoch,
) -> Option<LeaderInputs>
where
    BackendSettings: Sync,
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
{
    tracing::debug!(
        target: LOG_TARGET,
        current_epoch = ?current_epoch,
        slot = ?new_pol_info.poq_public_inputs.slot,
        "Received new secret PoL info; updating cryptographic processor"
    );
    let new_leader_inputs = LeaderInputs {
        pol_ledger_aged: new_pol_info.poq_public_inputs.aged_root,
        pol_epoch_nonce: new_pol_info.poq_public_inputs.epoch_nonce,
        message_quota: settings.session_leadership_quota(),
        lottery_0: new_pol_info.poq_public_inputs.lottery_0,
        lottery_1: new_pol_info.poq_public_inputs.lottery_1,
    };

    cryptographic_processor.set_epoch_private(
        new_pol_info.poq_private_inputs.clone(),
        new_leader_inputs,
        new_pol_info.epoch,
    );

    // If we've already processed the public epoch inputs, do not return anything.
    if new_pol_info.epoch <= current_epoch {
        return None;
    }

    // If the secret info is for a new epoch not yet seen via the clock
    // handler, update the core proof generator and proof verifier first.
    cryptographic_processor.rotate_epoch(new_leader_inputs, new_pol_info.epoch);

    Some(new_leader_inputs)
}

/// Submits an activity proof to the SDP service.
async fn submit_activity_proof(
    proof: ActivityProof,
    sdp_relay: &OutboundRelay<SdpMessage>,
) -> Result<(), RelayError> {
    debug!(target: LOG_TARGET, "Submitting activity proof for the old session");
    sdp_relay
        .send(SdpMessage::PostActivity {
            metadata: ActivityMetadata::Blend(Box::new((&proof).into())),
        })
        .await
        .map_err(|(e, _)| e)
}
