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
        crypto::proofs::PoQVerificationInputsMinusSigningKey,
        encap::{
            ProofsVerifier as ProofsVerifierTrait,
            encapsulated::EncapsulatedMessage,
            validated::{
                EncapsulatedMessageWithVerifiedPublicHeader,
                EncapsulatedMessageWithVerifiedSignature,
            },
        },
        reward::{
            self, ActivityProof, BlendingToken, EpochBlendingTokenCollector,
            OldEpochBlendingTokenCollector,
        },
    },
    proofs::quota::inputs::prove::public::{CoreInputs, LeaderInputs},
    scheduling::{
        EpochMessageScheduler,
        epoch::{EpochEvent, UninitializedEpochEventStream},
        message_blend::{
            crypto::EpochCryptographicProcessorSettings,
            provers::core_and_leader::CoreAndLeaderProofsGenerator,
        },
        message_scheduler::{
            OldEpochMessageScheduler, ProcessedMessageScheduler,
            epoch_info::EpochInfo as SchedulerEpochInfo,
            round_info::{RoundInfo, RoundReleaseType},
        },
    },
};
use lb_chain_service::{Epoch, api::CryptarchiaServiceData};
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
use lb_time_service::TimeService;
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
use tracing::{debug, error, info};

use crate::{
    core::{
        kms::{KmsPoQAdapter, PreloadKMSBackendCorePoQGenerator},
        processor::{
            CoreCryptographicProcessor, DecapsulatedMessageType, Error,
            MultiLayerDecapsulationOutput,
        },
        scheduler::SchedulerWrapper,
        settings::{RunningBlendConfig, StartingBlendConfig},
        state::{RecoveryServiceState, ServiceState, StateUpdater as ServiceStateUpdater},
    },
    epoch::{CoreEpochInfo, CoreEpochPublicInfo, MaybeEmptyCoreEpochInfo},
    epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait},
    kms::PreloadKmsService,
    membership::{self, ZkInfo, chain::BlendEpochState},
    message::{NetworkMessage, ProcessedMessage, ServiceMessage},
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
    NodeId: membership::node_id::TryFrom + Clone + Debug + Send + Eq + Hash + Sync + 'static,
    Network: NetworkAdapter<RuntimeServiceId, BroadcastSettings: Eq + Hash + Unpin> + Send + Sync,
    ProofsGenerator:
        CoreAndLeaderProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>> + Send,
    SdpService: ServiceData<Message = SdpMessage> + Send,
    ProofsVerifier: ProofsVerifierTrait + Clone + Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend + Send,
    ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Send + Unpin + 'static> + Send,
    RuntimeServiceId: AsServiceId<NetworkService<Network::Backend, RuntimeServiceId>>
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
            // service. If the persisted state is inconsistent (e.g. an epoch
            // mismatch from version skew or a partial write), discard it rather
            // than panicking: `run` already falls back to a fresh state when
            // none was recovered, which avoids a crash loop on every start.
            last_saved_state: recovery_initial_state.service_state.and_then(|s| {
                match s.try_into_state_with_state_updater(state_updater) {
                    Ok(state) => Some(state),
                    Err(error) => {
                        tracing::error!(
                            target: LOG_TARGET,
                            "Discarding inconsistent recovery state and starting fresh: {error:?}"
                        );
                        None
                    }
                }
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

        let public_epoch_stream =
            membership::chain::subscribe::<ChainService, NodeId, TimeBackend, RuntimeServiceId>(
                overwatch_handle,
                non_ephemeral_signing_key.public_key(),
                Some(zk_public_key),
            )
            .await;

        let sdp_relay = overwatch_handle
            .relay::<SdpService>()
            .await
            .expect("Relay with SDP service should be available.");

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
            mut remaining_epoch_stream,
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
            ProofsGenerator,
            ProofsVerifier,
            KmsServiceApi<PreloadKmsService<RuntimeServiceId>, RuntimeServiceId>,
            RuntimeServiceId,
        >(
            running_blend_config.clone(),
            public_epoch_stream,
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
        // epochs. When the node becomes a non-core node in a new epoch, the
        // old epoch's components (crypto processor, scheduler, blending token
        // collector, public info, and epoch) are returned for the retirement phase.
        let (
            old_epoch_crypto_processor,
            old_epoch_message_scheduler,
            old_epoch_blending_token_collector,
        ) = run_event_loop(
            inbound_relay,
            &mut blend_messages,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &running_blend_config,
            &mut backend,
            &network_adapter,
            &sdp_relay,
            message_scheduler.into(),
            &mut rng,
            crypto_processor,
            current_public_info,
            current_recovery_checkpoint,
        )
        .await;

        // The main event loop has ended because the node is no longer a core node
        // in the new epoch.
        // Before terminating the service, complete the old epoch during a single
        // epoch transition period.
        retire(
            // We don't need epoch numbers anymore since we know we are dealing with a single,
            // past epoch.
            blend_messages.map(|(message, _)| message),
            remaining_epoch_stream,
            backend,
            network_adapter,
            sdp_relay,
            old_epoch_message_scheduler,
            rng,
            old_epoch_blending_token_collector,
            old_epoch_crypto_processor,
        )
        .await;

        Ok(())
    }
}

/// Initialize the components for the [`BlendService`].
#[expect(clippy::too_many_lines, reason = "Need to initialize many components")]
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
async fn initialize<
    NodeId,
    Backend,
    NetAdapter,
    ProofsGenerator,
    ProofsVerifier,
    KmsAdapter,
    RuntimeServiceId,
>(
    blend_config: RunningBlendConfig<Backend::Settings>,
    public_epoch_stream: impl Stream<Item = BlendEpochState<NodeId>> + Send + Unpin + 'static,
    overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    kms_adapter: KmsAdapter,
    sdp_relay: &OutboundRelay<SdpMessage>,
    mut last_saved_state: Option<ServiceState<Backend::Settings, NetAdapter::BroadcastSettings>>,
    state_updater: StateUpdater<
        Option<RecoveryServiceState<Backend::Settings, NetAdapter::BroadcastSettings>>,
    >,
) -> (
    impl Stream<Item = EpochEvent<MaybeEmptyCoreEpochInfo<NodeId, KmsAdapter::CorePoQGenerator>>>
    + Unpin
    + Send
    + 'static,
    CoreEpochPublicInfo<NodeId>,
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
    ProofsGenerator: CoreAndLeaderProofsGenerator<KmsAdapter::CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
    // To avoid bubbling up generics everywhere in the configs (current Overwatch limitation), we
    // know the final key ID type is a `String`, so we constraint the trait impl here instead.
    KmsAdapter: KmsPoQAdapter<RuntimeServiceId, KeyId = String, CorePoQGenerator: Clone + Send + Sync>
        + Send
        + 'static,
    RuntimeServiceId: Clone + Send + Sync + 'static,
{
    // Initialize epoch stream for all public PoQ inputs.
    let epoch_stream = async {
        let config = blend_config.clone();
        let zk_sk_id = config.zk.secret_key_kms_id.clone();
        public_epoch_stream.map(
            move |BlendEpochState {
                      aged,
                      epoch,
                      lottery_0,
                      lottery_1,
                      membership_info,
                      nonce,
                  }| {
                // This can be empty in case of an empty membership set.
                let Some(ZkInfo {
                    root,
                    core_and_path_selectors,
                }) = membership_info.zk
                else {
                    return MaybeEmptyCoreEpochInfo::Empty {
                        epoch,
                        epoch_nonce: nonce,
                    };
                };
                // `None` when the local node is not part of the epoch membership. This can
                // happen when the node transitions from core to edge mode.
                let core_poq_generator = core_and_path_selectors.map(|selectors| {
                    kms_adapter.core_poq_generator(zk_sk_id.clone(), Box::new(selectors))
                });
                CoreEpochInfo {
                    public: CoreEpochPublicInfo {
                        poq_core_public_inputs: CoreInputs {
                            quota: config.epoch_core_quota(membership_info.membership.size()),
                            zk_root: root,
                        },
                        membership: membership_info.membership,
                        epoch,
                        poq_leadership_public_inputs: LeaderInputs {
                            pol_ledger_aged: aged,
                            pol_epoch_nonce: nonce,
                            message_quota: config.epoch_leadership_quota(),
                            lottery_0,
                            lottery_1,
                        },
                    },
                    core_poq_generator,
                }
                .into()
            },
        )
    }
    .await;
    let (current_epoch_info, remaining_epoch_stream) = Box::pin(
        UninitializedEpochEventStream::new(epoch_stream, blend_config.time.epoch_transition_period)
            .await_first_ready(),
    )
    .await
    .map(|(epoch_info, remaining_epoch_stream)| {
        let MaybeEmptyCoreEpochInfo::NonEmpty(core_epoch_info) = epoch_info else {
            panic!("First retrieved epoch for Blend core startup must be available.");
        };
        (core_epoch_info, remaining_epoch_stream.fork())
    })
    .expect("The current epoch info must be available.");

    let CoreEpochInfo {
        public: current_epoch_public_info,
        core_poq_generator: current_epoch_core_poq_generator,
    } = *current_epoch_info;

    info!(
        target: LOG_TARGET,
        "The current membership is ready: {:?}",
        current_epoch_public_info
    );

    let crypto_processor = CoreCryptographicProcessor::<
        _,
        KmsAdapter::CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >::try_new_with_core_condition_check(
        current_epoch_public_info.membership.clone(),
        blend_config.minimum_network_size,
        EpochCryptographicProcessorSettings {
            non_ephemeral_encryption_key: blend_config.non_ephemeral_signing_key.derive_x25519(),
            num_blend_layers: blend_config.num_blend_layers,
        },
        PoQVerificationInputsMinusSigningKey {
            core: current_epoch_public_info.poq_core_public_inputs,
            leader: current_epoch_public_info.poq_leadership_public_inputs,
        },
        current_epoch_core_poq_generator
            .expect("Core PoQ generator must be present at startup: the proxy service only launches CoreMode when the node is part of the core membership."),
        current_epoch_public_info.epoch,
    )
    .expect("The initial membership should satisfy the core node condition");

    // Initialize the current epoch state. If the epoch matches the stored one,
    // retrieves the tracked consumed core quota. Else, fallback to `0`.
    let current_recovery_checkpoint = if let Some(saved_state) = last_saved_state.take()
        && saved_state.last_seen_epoch() == current_epoch_public_info.epoch
    {
        tracing::trace!(
            target: LOG_TARGET,
            "Found recovery state for epoch {:?}: {saved_state:?}",
            current_epoch_public_info.epoch
        );
        saved_state
    } else {
        tracing::trace!(
            target: LOG_TARGET,
            "No recovery state found for epoch {:?}. Initializing a new one.",
            current_epoch_public_info.epoch
        );

        ServiceState::with_epoch(
            current_epoch_public_info.epoch,
            EpochBlendingTokenCollector::new(
                &reward::EpochInfo::new(
                    current_epoch_public_info.epoch,
                    &current_epoch_public_info.poq_leadership_public_inputs.pol_epoch_nonce,
                    current_epoch_public_info.membership.size() as u64,
                    current_epoch_public_info.poq_core_public_inputs.quota,
                    blend_config.activity_threshold_sensitivity,
                ).expect("Reward epoch info must be created successfully. Panicking since the service cannot continue with this epoch")
            ),
            None,
            state_updater,
        ).expect("service state should be created successfully")
    };

    // If there is the old epoch token collector loaded from `last_saved_state`,
    // compute/submit its activity proof because we won't collect more tokens for
    // the old epoch after this initialization step because we are not
    // establishing connections for the old epoch.
    let mut state_updater = current_recovery_checkpoint.start_updating();
    if let Some(old_epoch_token_collector) = state_updater.clear_old_epoch_token_collector() {
        tracing::debug!(target: LOG_TARGET, "Old epoch token collector loaded. Computing activity proof");
        compute_and_submit_activity_proof(old_epoch_token_collector, sdp_relay).await;
    }
    let current_recovery_checkpoint = state_updater.commit_changes();

    let message_scheduler = SchedulerWrapper::new_with_initial_messages(
        SchedulerEpochInfo {
            core_quota: blend_config
                .epoch_core_quota(current_epoch_public_info.membership.size())
                .saturating_sub(current_recovery_checkpoint.spent_quota()),
            epoch: current_epoch_public_info.epoch,
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
        (
            current_epoch_public_info.membership.clone(),
            current_epoch_public_info.epoch,
        ),
        BlakeRng::from_entropy(),
    );

    // Rng for releasing messages.
    let rng = BlakeRng::from_entropy();

    (
        remaining_epoch_stream,
        current_epoch_public_info,
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
// This can span across multiple epochs.
//
// Epoch rotations are driven by the public epoch stream (membership and public
// `PoQ` inputs) through `handle_epoch_event`. The secret `PoL` info stream is
// independent: it only enables leadership-proof generation for the current
// epoch once its info arrives, without driving rotations on its own.
//
// Returns the old epoch components when the node is no longer a core node.
#[expect(clippy::too_many_arguments, reason = "categorize args")]
async fn run_event_loop<
    NodeId,
    Backend,
    Rng,
    NetAdapter,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    mut inbound_relay: impl Stream<Item = ServiceMessage<NetAdapter::BroadcastSettings, NodeId>>
    + Send
    + Unpin,
    blend_messages: &mut (
             impl Stream<Item = (EncapsulatedMessageWithVerifiedSignature, Epoch)>
             + Send
             + Unpin
             + 'static
         ),
    mut secret_pol_info_stream: impl Stream<Item = PolEpochInfo> + Send + Unpin,
    remaining_epoch_stream: &mut (
             impl Stream<Item = EpochEvent<MaybeEmptyCoreEpochInfo<NodeId, CorePoQGenerator>>>
             + Unpin
             + Send
         ),
    blend_config: &RunningBlendConfig<Backend::Settings>,
    backend: &mut Backend,
    network_adapter: &NetAdapter,
    sdp_relay: &OutboundRelay<SdpMessage>,
    mut message_scheduler: EpochMessageScheduler<
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
    mut current_epoch_info: CoreEpochPublicInfo<NodeId>,
    mut recovery_checkpoint: ServiceState<Backend::Settings, NetAdapter::BroadcastSettings>,
) -> (
    CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    OldEpochMessageScheduler<Rng, ProcessedMessage<NetAdapter::BroadcastSettings>>,
    OldEpochBlendingTokenCollector,
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
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator> + Send,
    CorePoQGenerator: Send + Sync,
    ProofsVerifier: ProofsVerifierTrait + Send + Sync,
    RuntimeServiceId: Sync + Send,
{
    // An optional crypto processor to handle the old epoch during transition
    // period.
    let mut old_epoch_crypto_processor: Option<
        CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    > = None;
    let mut old_epoch_message_scheduler: Option<
        OldEpochMessageScheduler<Rng, ProcessedMessage<NetAdapter::BroadcastSettings>>,
    > = None;
    let mut latest_secret_pol_info: Option<PolEpochInfo> = None;

    loop {
        // `old_epoch` captured here so we can drop the `Sync` requirement.
        let old_epoch = old_epoch_crypto_processor
            .as_ref()
            .map(CoreCryptographicProcessor::epoch);
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
                recovery_checkpoint = handle_incoming_blend_message(incoming_message, &mut message_scheduler, old_epoch_message_scheduler.as_mut(), &crypto_processor, old_epoch_crypto_processor.as_ref(),  recovery_checkpoint);
            }
            Some(round_info) = message_scheduler.next() => {
                recovery_checkpoint = handle_release_round(round_info, &mut crypto_processor, rng, backend, network_adapter, recovery_checkpoint).await;
            }
            Some((Some(processed_messages_to_release), previous_epoch)) = async {
                match (&mut old_epoch_message_scheduler, old_epoch) {
                    (Some(old_scheduler), Some(old_epoch)) => {
                        Some((old_scheduler.next().await, old_epoch))
                    },
                    _ => None
                }
            } => {
                handle_release_round_for_old_epoch(processed_messages_to_release, rng, backend, network_adapter, previous_epoch).await;
            }
            Some(pol_secret_info) = secret_pol_info_stream.next() => {
                if current_epoch_info.epoch == pol_secret_info.epoch {
                    // Apply now: move the winning-slot stream into the current processor.
                    crypto_processor.set_epoch_private(pol_secret_info.winning_pol_info_stream, pol_secret_info.epoch);
                    latest_secret_pol_info = None;
                } else {
                    // Belongs to an upcoming epoch: keep it to seed that epoch's
                    // processor when the rotation happens.
                    latest_secret_pol_info = Some(pol_secret_info);
                }
            }
            Some(epoch_event) = remaining_epoch_stream.next() => {
                match handle_epoch_event(epoch_event, blend_config, crypto_processor, message_scheduler, current_epoch_info, recovery_checkpoint, backend, sdp_relay, &mut latest_secret_pol_info).await {
                    // Current epoch info updated to new one
                    HandleEpochEventOutput::Transitioning { new_crypto_processor, old_crypto_processor, new_scheduler, old_scheduler, new_epoch_info, new_recovery_checkpoint } => {
                        crypto_processor = new_crypto_processor;
                        old_epoch_crypto_processor = Some(old_crypto_processor);
                        message_scheduler = new_scheduler;
                        old_epoch_message_scheduler = Some(old_scheduler);
                        current_epoch_info = new_epoch_info;
                        recovery_checkpoint = new_recovery_checkpoint;
                    },
                    // Current epoch info unchanged
                    HandleEpochEventOutput::TransitionCompleted { current_crypto_processor, current_scheduler, new_recovery_checkpoint, current_epoch_info: same_epoch_info } => {
                        crypto_processor = current_crypto_processor;
                        old_epoch_crypto_processor = None;
                        message_scheduler = current_scheduler;
                        old_epoch_message_scheduler = None;
                        current_epoch_info = same_epoch_info;
                        recovery_checkpoint = new_recovery_checkpoint;
                    },
                    // Current epoch info consumed, not usable anymore
                    HandleEpochEventOutput::Retiring { old_crypto_processor, old_scheduler, old_token_collector } => {
                        tracing::info!(target: LOG_TARGET, "Exiting from the main event loop");
                        return (
                            old_crypto_processor,
                            old_scheduler,
                            old_token_collector,
                        );
                    },
                }
            }
        }
    }
}

/// Processes the old epoch during the epoch transition period
/// before retiring the core service.
#[expect(clippy::too_many_arguments, reason = "categorize args")]
async fn retire<
    NodeId,
    Backend,
    Rng,
    NetAdapter,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    mut blend_messages: impl Stream<Item = EncapsulatedMessageWithVerifiedSignature>
    + Unpin
    + Send
    + 'static,
    mut remaining_epoch_stream: impl Stream<
        Item = EpochEvent<MaybeEmptyCoreEpochInfo<NodeId, CorePoQGenerator>>,
    > + Send
    + Unpin,
    mut backend: Backend,
    network_adapter: NetAdapter,
    sdp_relay: OutboundRelay<SdpMessage>,
    mut message_scheduler: OldEpochMessageScheduler<
        Rng,
        ProcessedMessage<NetAdapter::BroadcastSettings>,
    >,
    mut rng: Rng,
    mut blending_token_collector: OldEpochBlendingTokenCollector,
    crypto_processor: CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
) where
    NodeId: Clone + Eq + Hash + Send + Sync + 'static,
    Rng: rand::Rng + Clone + Send + Unpin,
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId> + Send + Sync,
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
        > + Send
        + Sync,
    ProofsGenerator: CoreAndLeaderProofsGenerator<CorePoQGenerator> + Send,
    CorePoQGenerator: Send + Sync,
    ProofsVerifier: ProofsVerifierTrait + Send + Sync,
    RuntimeServiceId: Send + Sync,
{
    loop {
        tokio::select! {
            Some(incoming_message) = blend_messages.next() => {
                handle_incoming_blend_message_from_old_epoch(incoming_message, &mut message_scheduler, &crypto_processor, &mut blending_token_collector);
            }
            Some(processed_messages_to_release) = message_scheduler.next() => {
                handle_release_round_for_old_epoch(processed_messages_to_release, &mut rng, &backend, &network_adapter, crypto_processor.epoch()).await;
            }
            Some(EpochEvent::TransitionPeriodExpired) = remaining_epoch_stream.next() => {
                handle_epoch_transition_expired(&mut backend, blending_token_collector, &sdp_relay).await;
                // Now the core service is no longer needed for the current (new) epoch,
                // and the remaining epoch transition has been completed,
                // so finishing the retirement process.
                return;
            }
        }
    }
}

/// Handles an [`EpochEvent`].
///
/// On a new epoch it consumes the previous cryptographic processor and creates
/// a new one for the new epoch with its new membership and public `PoQ`
/// verification inputs. If secret `PoL` info for the new epoch is already
/// available, leadership-proof generation is enabled on the new processor right
/// away. It ignores the transition period expiration event and returns the
/// previous cryptographic processor as is.
#[expect(clippy::too_many_arguments, reason = "necessary for epoch handling")]
#[expect(clippy::too_many_lines, reason = "necessary for epoch handling")]
#[expect(clippy::cognitive_complexity, reason = "necessary for epoch handling")]
async fn handle_epoch_event<
    NodeId,
    ProofsGenerator,
    ProofsVerifier,
    Backend,
    Rng,
    BroadcastSettings,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    event: EpochEvent<MaybeEmptyCoreEpochInfo<NodeId, CorePoQGenerator>>,
    settings: &RunningBlendConfig<Backend::Settings>,
    current_cryptographic_processor: CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    current_scheduler: EpochMessageScheduler<
        Rng,
        ProcessedMessage<BroadcastSettings>,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    current_epoch_info: CoreEpochPublicInfo<NodeId>,
    current_recovery_checkpoint: ServiceState<Backend::Settings, BroadcastSettings>,
    backend: &mut Backend,
    sdp_relay: &OutboundRelay<SdpMessage>,
    current_secret_info: &mut Option<PolEpochInfo>,
) -> HandleEpochEventOutput<
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
        EpochEvent::NewEpoch(MaybeEmptyCoreEpochInfo::NonEmpty(core_epoch_info)) => {
            let CoreEpochInfo {
                core_poq_generator: new_core_poq_generator,
                public: new_epoch_info,
            } = *core_epoch_info;
            let (_, _, _, _, current_epoch_blending_token_collector, _, state_updater) =
                current_recovery_checkpoint.into_components();

            let new_reward_epoch_info = reward::EpochInfo::new(
                new_epoch_info.epoch,
                &new_epoch_info.poq_leadership_public_inputs.pol_epoch_nonce,
                new_epoch_info.membership.size() as u64,
                new_epoch_info.poq_core_public_inputs.quota,
                settings.activity_threshold_sensitivity,
            )
            .expect("Reward epoch info must be created successfully. Panicking since the service cannot continue with this epoch");
            let (new_epoch_blending_token_collector, old_epoch_blending_token_collector) =
                current_epoch_blending_token_collector.rotate_epoch(&new_reward_epoch_info);

            backend
                .rotate_epoch((new_epoch_info.membership.clone(), new_epoch_info.epoch))
                .await;

            let new_scheduler_epoch_info = SchedulerEpochInfo {
                core_quota: settings.epoch_core_quota(new_epoch_info.membership.size()),
                epoch: new_epoch_info.epoch,
            };

            let Some(core_poq_generator) = new_core_poq_generator else {
                tracing::info!(target: LOG_TARGET, "Local node is not part of new membership. Retiring from core.");
                return HandleEpochEventOutput::Retiring {
                    old_crypto_processor: current_cryptographic_processor,
                    old_scheduler: current_scheduler
                        .rotate_epoch(new_scheduler_epoch_info, settings.scheduler_settings())
                        .1,
                    old_token_collector: old_epoch_blending_token_collector,
                };
            };

            let new_processor = match CoreCryptographicProcessor::try_new_with_core_condition_check(
                new_epoch_info.membership.clone(),
                settings.minimum_network_size,
                EpochCryptographicProcessorSettings {
                    non_ephemeral_encryption_key: settings
                        .non_ephemeral_signing_key
                        .derive_x25519(),
                    num_blend_layers: settings.num_blend_layers,
                },
                PoQVerificationInputsMinusSigningKey {
                    core: new_epoch_info.poq_core_public_inputs,
                    leader: new_epoch_info.poq_leadership_public_inputs,
                },
                core_poq_generator,
                new_epoch_info.epoch,
            ) {
                Ok(mut new_processor) => {
                    if current_secret_info
                        .as_ref()
                        .is_some_and(|secret| secret.epoch == new_epoch_info.epoch)
                    {
                        // We consume the stream by `take()`ing only if the epochs match.
                        let current_secret_info = current_secret_info
                            .take()
                            .expect("Secret PoL info presence checked above.");
                        new_processor.set_epoch_private(
                            current_secret_info.winning_pol_info_stream,
                            new_epoch_info.epoch,
                        );
                    }
                    new_processor
                }
                Err(e @ (Error::LocalIsNotCoreNode | Error::NetworkIsTooSmall(_))) => {
                    tracing::info!(target: LOG_TARGET, "New membership does not satisfy the core node condition: {e:?}");
                    return HandleEpochEventOutput::Retiring {
                        old_crypto_processor: current_cryptographic_processor,
                        old_scheduler: current_scheduler
                            .rotate_epoch(new_scheduler_epoch_info, settings.scheduler_settings())
                            .1,
                        old_token_collector: old_epoch_blending_token_collector,
                    };
                }
            };

            let (new_scheduler, old_scheduler) = current_scheduler
                .rotate_epoch(new_scheduler_epoch_info, settings.scheduler_settings());
            HandleEpochEventOutput::Transitioning {
                new_crypto_processor: new_processor,
                old_crypto_processor: current_cryptographic_processor,
                new_scheduler,
                old_scheduler,
                new_recovery_checkpoint: ServiceState::with_epoch(
                    new_epoch_info.epoch,
                    new_epoch_blending_token_collector,
                    Some(old_epoch_blending_token_collector),
                    state_updater,
                )
                .expect("service state should be created successfully"),
                new_epoch_info,
            }
        }
        EpochEvent::NewEpoch(MaybeEmptyCoreEpochInfo::Empty { epoch, epoch_nonce }) => {
            tracing::info!(target: LOG_TARGET, "New epoch event received, but no epoch info is available due to empty membership set.");
            let (_, _, _, _, current_epoch_blending_token_collector, _, _) =
                current_recovery_checkpoint.into_components();
            let new_reward_epoch_info = reward::EpochInfo::new(
                epoch,
                &epoch_nonce,
                0,
                0,
                settings.activity_threshold_sensitivity,
            )
            .expect("Reward epoch info must be created successfully. Panicking since the service cannot continue with this epoch");
            let (_, old_epoch_blending_token_collector) =
                current_epoch_blending_token_collector.rotate_epoch(&new_reward_epoch_info);
            HandleEpochEventOutput::Retiring {
                old_crypto_processor: current_cryptographic_processor,
                old_scheduler: current_scheduler.consume(),
                old_token_collector: old_epoch_blending_token_collector,
            }
        }
        EpochEvent::TransitionPeriodExpired => {
            let mut state_updater = current_recovery_checkpoint.start_updating();

            if let Some(old_token_collector) = state_updater.clear_old_epoch_token_collector() {
                handle_epoch_transition_expired(backend, old_token_collector, sdp_relay).await;
            }

            HandleEpochEventOutput::TransitionCompleted {
                current_crypto_processor: current_cryptographic_processor,
                current_scheduler,
                current_epoch_info,
                new_recovery_checkpoint: state_updater.commit_changes(),
            }
        }
    }
}

/// Handles [`EpochEvent::TransitionPeriodExpired`].
async fn handle_epoch_transition_expired<Backend, NodeId, Rng, RuntimeServiceId>(
    backend: &mut Backend,
    blending_token_collector: OldEpochBlendingTokenCollector,
    sdp_relay: &OutboundRelay<SdpMessage>,
) where
    Backend: BlendBackend<NodeId, Rng, RuntimeServiceId>,
    NodeId: Eq + Hash + Clone + Send,
{
    compute_and_submit_activity_proof(blending_token_collector, sdp_relay).await;
    backend.complete_epoch_transition().await;
}

async fn compute_and_submit_activity_proof(
    blending_token_collector: OldEpochBlendingTokenCollector,
    sdp_relay: &OutboundRelay<SdpMessage>,
) {
    if let Some(activity_proof) = blending_token_collector.compute_activity_proof() {
        if let Err(e) = submit_activity_proof(activity_proof, sdp_relay).await {
            error!(target: LOG_TARGET, "Failed to submit activity proof for the old epoch: {e:?}");
        }
    } else {
        debug!(target: LOG_TARGET, "No activity proof generated for the old epoch");
    }
}

enum HandleEpochEventOutput<
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
        new_scheduler: EpochMessageScheduler<
            Rng,
            ProcessedMessage<BroadcastSettings>,
            EncapsulatedMessageWithVerifiedPublicHeader,
        >,
        old_scheduler: OldEpochMessageScheduler<Rng, ProcessedMessage<BroadcastSettings>>,
        new_epoch_info: CoreEpochPublicInfo<NodeId>,
        new_recovery_checkpoint: ServiceState<BackendSettings, BroadcastSettings>,
    },
    TransitionCompleted {
        current_crypto_processor:
            CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
        current_scheduler: EpochMessageScheduler<
            Rng,
            ProcessedMessage<BroadcastSettings>,
            EncapsulatedMessageWithVerifiedPublicHeader,
        >,
        current_epoch_info: CoreEpochPublicInfo<NodeId>,
        new_recovery_checkpoint: ServiceState<BackendSettings, BroadcastSettings>,
    },
    Retiring {
        old_crypto_processor:
            CoreCryptographicProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
        old_scheduler: OldEpochMessageScheduler<Rng, ProcessedMessage<BroadcastSettings>>,
        old_token_collector: OldEpochBlendingTokenCollector,
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
    scheduler: &mut EpochMessageScheduler<
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
    state_updater.collect_current_epoch_tokens(blending_tokens.into_iter());

    scheduler.schedule_processed_message(processed_message.clone());
    // We treat a partially or fully decapsulated message as a processed message,
    // and we schedule for its release at the next release round.
    if state_updater
        .add_unsent_processed_message(processed_message.clone())
        .is_err()
    {
        // With a data replication factor greater than `0`, it's expected to have
        // multiple identical copies of the same data message, so in that case it's not
        // a warning and should not be logged.
        // Hence, we only log a warning in the unexpected case of an encapsulated
        // message seen twice, which should never happen.
        if matches!(processed_message, ProcessedMessage::Encapsulated(_)) {
            tracing::warn!(
                target: LOG_TARGET,
                "There should not be another copy of the same locally-generated processed message: {processed_message:?}."
            );
        }
    }
    state_updater.commit_changes()
}

/// Processes an incoming Blend message (with verified signature) received
/// from a core or edge peer.
///
/// Decapsulation is attempted with the current or old epoch's cryptographic
/// processor depending on the epoch the message is coming from.
fn handle_incoming_blend_message<
    NodeId,
    Rng,
    BroadcastSettings,
    BackendSettings,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
>(
    (validated_encapsulated_message, epoch): (EncapsulatedMessageWithVerifiedSignature, Epoch),
    scheduler: &mut EpochMessageScheduler<
        Rng,
        ProcessedMessage<BroadcastSettings>,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    old_epoch_scheduler: Option<
        &mut OldEpochMessageScheduler<Rng, ProcessedMessage<BroadcastSettings>>,
    >,
    cryptographic_processor: &CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    old_epoch_cryptographic_processor: Option<
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
    if epoch == cryptographic_processor.epoch() {
        let Some(output) = try_validate_and_decapsulate(
            validated_encapsulated_message,
            cryptographic_processor,
            epoch,
        ) else {
            return current_recovery_checkpoint;
        };
        handle_decapsulated_incoming_message_from_current_epoch(
            output,
            scheduler,
            current_recovery_checkpoint,
            cryptographic_processor,
        )
    } else if let Some(old_cryptographic_processor) = old_epoch_cryptographic_processor
        && epoch == old_cryptographic_processor.epoch()
    {
        let Some(output) = try_validate_and_decapsulate(
            validated_encapsulated_message,
            old_cryptographic_processor,
            epoch,
        ) else {
            return current_recovery_checkpoint;
        };
        handle_decapsulated_incoming_message_from_old_epoch(
            output,
            old_epoch_scheduler
                .expect("Old epoch scheduler should be available when old epoch crypto processor is available"),
            current_recovery_checkpoint,
            old_cryptographic_processor,
        )
    } else {
        tracing::debug!(target: LOG_TARGET, "Received message for epoch {epoch} that is not currently handled. Ignoring...");
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
    epoch: Epoch,
) -> Option<MultiLayerDecapsulationOutput>
where
    ProofsVerifier: ProofsVerifierTrait,
{
    let Ok(validated_message) = processor.validate_message_poq(message) else {
        tracing::debug!(target: LOG_TARGET, "Received message for epoch {epoch} failed PoQ validation. Ignoring...");
        return None;
    };
    match processor.decapsulate_message_recursive(validated_message) {
        Ok(output) => Some(output),
        Err(e) => {
            if matches!(e, MessageError::PrivateHeaderDeserializationFailed) {
                tracing::trace!(target: LOG_TARGET, "Failed to decapsulate received message for epoch {epoch} due to deserialization error. This can happen when the message was intended for another node or when the message is malformed. Ignoring...");
            } else {
                tracing::debug!(target: LOG_TARGET, "Failed to decapsulate received message for epoch {epoch}: {e:?}.");
            }
            None
        }
    }
}

/// Same as [`handle_incoming_blend_message`] but only tries with
/// the old epoch crypto processor.
fn handle_incoming_blend_message_from_old_epoch<
    Rng,
    NodeId,
    BroadcastSettings,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
>(
    validated_encapsulated_message: EncapsulatedMessageWithVerifiedSignature,
    scheduler: &mut OldEpochMessageScheduler<Rng, ProcessedMessage<BroadcastSettings>>,
    cryptographic_processor: &CoreCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    blending_token_collector: &mut OldEpochBlendingTokenCollector,
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
                tracing::trace!(target: LOG_TARGET, "Failed to decapsulate received message from old epoch due to deserialization error. This can happen when the message was intended for another node or when the message is malformed. Ignoring...");
            } else {
                tracing::debug!(target: LOG_TARGET, "Failed to decapsulate received message from old epoch: {e:?}");
            }
        }
    }
}

/// Schedules a decapsulated incoming message from the current epoch,
/// and collects the blending tokens obtained from the decapsulation.
///
/// It updates the recovery checkpoint by storing the scheduled message
/// and the collected tokens.
fn handle_decapsulated_incoming_message_from_current_epoch<
    Rng,
    BroadcastSettings,
    BackendSettings,
    NodeId,
    CorePoQGenerator,
    ProofsGenerator,
    ProofsVerifier,
>(
    multi_layer_decapsulation_output: MultiLayerDecapsulationOutput,
    scheduler: &mut EpochMessageScheduler<
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

    if let Some(processed_message) = maybe_processed_message
        && state_updater
            .add_unsent_processed_message(processed_message)
            .is_err()
    {
        tracing::trace!(
            target: LOG_TARGET,
            "Dropping a duplicate decapsulated replica already pending release."
        );
    }

    state_updater.collect_current_epoch_tokens(blending_tokens);
    state_updater.commit_changes()
}

/// Schedules a decapsulated incoming message from the old epoch,
/// and collects the blending tokens obtained from the decapsulation.
///
/// It updates the recovery checkpoint by storing the collected tokens.
fn handle_decapsulated_incoming_message_from_old_epoch<
    Rng,
    BroadcastSettings,
    BackendSettings,
    NodeId,
    CorePoQGenerator,
    ProofsGenerator,
    ProofsVerifier,
>(
    multi_layer_decapsulation_output: MultiLayerDecapsulationOutput,
    scheduler: &mut OldEpochMessageScheduler<Rng, ProcessedMessage<BroadcastSettings>>,
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
        .collect_old_epoch_tokens(blending_tokens)
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
    let current_epoch = cryptographic_processor.epoch();

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
                backend.publish(data_message_to_blend, current_epoch).boxed()
            },
        ).collect::<Vec<_>>();

    let processed_messages_relay_futures = build_futures_to_release_processed_messages(
        processed_messages,
        backend,
        network_adapter,
        Some(&mut state_updater),
        current_epoch,
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
                    current_epoch,
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

async fn handle_release_round_for_old_epoch<NodeId, Rng, Backend, NetAdapter, RuntimeServiceId>(
    processed_messages_to_release: Vec<ProcessedMessage<NetAdapter::BroadcastSettings>>,
    rng: &mut Rng,
    backend: &Backend,
    network_adapter: &NetAdapter,
    epoch: Epoch,
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
        epoch,
    );
    futures.shuffle(rng);

    // Release all messages concurrently, and wait for all of them to be sent.
    let num_futures = futures.len();
    join_all(futures).await;
    log_old_epoch_release_summary(num_futures);
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

fn log_old_epoch_release_summary(num_futures: usize) {
    if num_futures > 0 {
        tracing::debug!(
            target: LOG_TARGET,
            "Sent out {num_futures} processed messages at this release window for the old epoch"
        );
    } else {
        tracing::trace!(
            target: LOG_TARGET,
            "Sent out {num_futures} processed messages at this release window for the old epoch"
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
    epoch: Epoch,
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
                && state_updater.remove_sent_processed_message(processed_message_to_release).is_err() && matches!(processed_message_to_release, ProcessedMessage::Encapsulated(_)) {
                    // With a data replication factor greater than `0`, it's expected to have
                    // multiple identical copies of the same data message, so in that case it's not
                    // a warning and should not be logged.
                    // Hence, we only log a warning in the unexpected case of an encapsulated
                    // message seen twice, which should never happen.
                    tracing::warn!(
                            target: LOG_TARGET,
                            "Previously processed message should be present in the recovery state but was not found."
                        );
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
                        backend.publish(*encapsulated_message, epoch).boxed()
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

    state_updater.collect_current_epoch_tokens(blending_tokens.into_iter());

    match message_type {
        // This is the initial message that was encapsulated, since we fully
        // decapsulated a cover message, we don't do anything.
        DecapsulatedMessageType::Completed(_) => None,
        DecapsulatedMessageType::Incompleted(remaining_encapsulated_message) => {
            Some(*remaining_encapsulated_message)
        }
    }
}

/// Submits an activity proof to the SDP service.
async fn submit_activity_proof(
    proof: ActivityProof,
    sdp_relay: &OutboundRelay<SdpMessage>,
) -> Result<(), RelayError> {
    debug!(target: LOG_TARGET, "Submitting activity proof for the old epoch");
    sdp_relay
        .send(SdpMessage::PostActivity {
            metadata: ActivityMetadata::Blend(Box::new((&proof).into())),
        })
        .await
        .map_err(|(e, _)| e)
}
