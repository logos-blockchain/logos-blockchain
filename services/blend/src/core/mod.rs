use core::num::NonZeroU64;
use std::{
    collections::VecDeque,
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use backends::BlendBackend;
use dispatcher::PayloadDispatcher;
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
            ProofsVerifier as ProofsVerifierTrait, encapsulated::EncapsulatedMessage,
            validated::EncapsulatedMessageWithVerifiedPublicHeader,
        },
        reward::{
            self, ActivityProof, BlendingToken, EpochBlendingTokenCollector,
            OldEpochBlendingTokenCollector,
        },
    },
    proofs::quota::inputs::prove::public::{CoreInputs, LeaderInputs, PowInputs},
    scheduling::{
        EpochMessageScheduler,
        epoch::{EpochEvent, UninitializedEpochEventStream},
        message_blend::{
            crypto::{
                EpochCryptographicProcessorSettings,
                core_and_leader::receive::{
                    DecapsulatedMessageType, MultiLayerDecapsulationOutput,
                },
            },
            provers::{core_leader_and_pow::CoreLeaderAndPowProofsGenerator, pow::new_mining_pool},
        },
        message_scheduler::{
            OldEpochMessageScheduler, ProcessedMessageScheduler,
            epoch_info::EpochInfo as SchedulerEpochInfo,
            round_info::{RoundInfo, RoundReleaseType},
        },
    },
};
use lb_chain_service::{Epoch, api::CryptarchiaServiceData};
use lb_core::sdp::ActivityMetadata;
use lb_key_management_system_service::{
    api::KmsServiceApi,
    keys::{KeyOperators, PublicKeyEncoding},
    operators::ed25519::exfiltrate_secret_key::LeakSecretKeyOperator,
};
use lb_log_targets::blend;
use lb_network_service::NetworkService;
use lb_poq::Quota;
use lb_sdp_service::SdpMessage;
use lb_services_utils::{
    overwatch::{RecoveryOperator, recovery::operators::RecoveryBackend as RecoveryBackendTrait},
    wait_until_services_are_ready,
};
use lb_time_service::TimeService;
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
use rand_chacha::ChaCha20Rng;
use tokio::sync::oneshot;
use tracing::{debug, error, info};

use crate::{
    core::{
        backends::BackendEpochInfo,
        epoch_stages::{
            retiring::RetiringEpoch,
            running::{
                Components, CurrentEpoch, CurrentEpochDuringTransition, CurrentEpochEvent,
                DuringTransitionEvent,
            },
            transitioning::TransitioningEpoch,
        },
        kms::{KmsPoQAdapter, PreloadKMSBackendCorePoQGenerator},
        processor::{
            CoreCryptographicProcessor as CurrentEpochCryptographicProcessor, Error,
            ReceiverCryptographicProcessor,
        },
        scheduler::SchedulerWrapper,
        settings::{RunningBlendConfig, StartingBlendConfig},
        state::{RecoveryServiceState, ServiceState, StateUpdater as ServiceStateUpdater},
    },
    epoch::{CoreEpochInfo, CoreEpochPublicInfo, MaybeEmptyCoreEpochInfo},
    epoch_info::{PolEpochInfo, PolInfoProvider as PolInfoProviderTrait},
    kms::PreloadKmsService,
    membership::{self, ZkInfo, chain::BlendEpochState},
    message::{BlendPayload, ProcessedMessage, ServiceMessage},
    pending::{
        EncapsulationResult, LocalEncapsulation, MessageKind, NextLocalMessage, PendingProposals,
        PendingTransactions, next_local_message, resolve_encapsulation,
    },
};

pub mod backends;
pub mod dispatcher;
pub mod kms;
pub mod settings;

pub(super) mod service_components;

mod epoch_stages;
mod processor;
mod scheduler;
mod state;
#[cfg(test)]
mod tests;
pub use state::RecoveryServiceState as CoreServiceState;

const LOG_TARGET: &str = blend::service::CORE;

type OldEpochCryptographicProcessor<ProofsVerifier> =
    ReceiverCryptographicProcessor<ProofsVerifier>;

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
    Dispatcher,
    SdpService,
    ProofsGenerator,
    ProofsVerifier,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    StateStorage,
    RuntimeServiceId,
> where
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId>,
    Dispatcher: PayloadDispatcher<RuntimeServiceId>,
    StateStorage: RecoveryBackendTrait<
            RuntimeServiceId,
            State = RecoveryServiceState<Backend::Settings, Dispatcher::Settings>,
        > + Send
        + Sync,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    last_saved_state: Option<ServiceState<Backend::Settings, Dispatcher::Settings>>,
    _phantom: PhantomData<(
        Backend,
        SdpService,
        ProofsGenerator,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        StateStorage,
    )>,
}

impl<
    Backend,
    NodeId,
    Dispatcher,
    SdpService,
    ProofsGenerator,
    ProofsVerifier,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    StateStorage,
    RuntimeServiceId,
> ServiceData
    for BlendService<
        Backend,
        NodeId,
        Dispatcher,
        SdpService,
        ProofsGenerator,
        ProofsVerifier,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        StateStorage,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId>,
    Dispatcher: PayloadDispatcher<RuntimeServiceId>,
    StateStorage: RecoveryBackendTrait<
            RuntimeServiceId,
            State = RecoveryServiceState<Backend::Settings, Dispatcher::Settings>,
        > + Send
        + Sync,
{
    type Settings = StartingBlendConfig<Backend::Settings, Dispatcher::Settings>;
    type State = RecoveryServiceState<Backend::Settings, Dispatcher::Settings>;
    type StateOperator = RecoveryOperator<StateStorage>;
    type Message = ServiceMessage<NodeId>;
}

#[async_trait]
impl<
    Backend,
    NodeId,
    Dispatcher,
    SdpService,
    ProofsGenerator,
    ProofsVerifier,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    StateStorage,
    RuntimeServiceId,
> ServiceCore<RuntimeServiceId>
    for BlendService<
        Backend,
        NodeId,
        Dispatcher,
        SdpService,
        ProofsGenerator,
        ProofsVerifier,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        StateStorage,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId> + Send + Sync,
    NodeId: membership::node_id::TryFrom + Clone + Debug + Send + Eq + Hash + Sync + 'static,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Send + Sync,
    ProofsGenerator:
        CoreLeaderAndPowProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>> + Send,
    SdpService: ServiceData<Message = SdpMessage> + Send,
    ProofsVerifier: ProofsVerifierTrait + Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend + Send,
    ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
    PolInfoProvider: PolInfoProviderTrait<RuntimeServiceId, Stream: Send + Unpin + 'static> + Send,
    StateStorage: RecoveryBackendTrait<
            RuntimeServiceId,
            State = RecoveryServiceState<Backend::Settings, Dispatcher::Settings>,
        > + Send
        + Sync,
    RuntimeServiceId: AsServiceId<NetworkService<Dispatcher::Backend, RuntimeServiceId>>
        + AsServiceId<Dispatcher::MempoolService>
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

        let payload_dispatcher = async {
            let network_relay = overwatch_handle
                .relay::<NetworkService<_, _>>()
                .await
                .expect("Relay with network service should be available.");
            let mempool_relay = overwatch_handle
                .relay::<Dispatcher::MempoolService>()
                .await
                .expect("Relay with mempool service should be available.");
            Dispatcher::new(network_relay, mempool_relay, blend_config.network.clone())
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
                "blend_core_service",
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
            scheduler: blend_config.scheduler,
            time: blend_config.time,
            zk: blend_config.zk,
            data_replication_factor: blend_config.data_replication_factor,
            activity_threshold_sensitivity: blend_config.activity_threshold_sensitivity,
            pow_mining_pool: new_mining_pool(),
        };
        let (
            mut remaining_epoch_stream,
            current_public_info,
            crypto_processor,
            current_recovery_checkpoint,
            pending_transactions,
            message_scheduler,
            mut backend,
            mut rng,
        ) = initialize::<
            NodeId,
            Backend,
            Dispatcher,
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
            ChaCha20Rng::from_entropy(),
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
        // epoch it is leaving behind is handed over for the retirement phase.
        let retiring_epoch = run_event_loop(
            inbound_relay,
            &mut blend_messages,
            secret_pol_info_stream,
            &mut remaining_epoch_stream,
            &running_blend_config,
            &mut backend,
            &payload_dispatcher,
            &sdp_relay,
            &mut rng,
            CurrentEpoch::new(
                crypto_processor,
                message_scheduler.into(),
                current_public_info,
            ),
            pending_transactions,
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
            payload_dispatcher,
            sdp_relay,
            rng,
            retiring_epoch,
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
#[expect(clippy::too_many_arguments, reason = "categorize args")]
async fn initialize<
    NodeId,
    Backend,
    Dispatcher,
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
    mut last_saved_state: Option<ServiceState<Backend::Settings, Dispatcher::Settings>>,
    state_updater: StateUpdater<
        Option<RecoveryServiceState<Backend::Settings, Dispatcher::Settings>>,
    >,
    release_delay_rng: ChaCha20Rng,
) -> (
    impl Stream<Item = EpochEvent<MaybeEmptyCoreEpochInfo<NodeId, KmsAdapter::CorePoQGenerator>>>
    + Unpin
    + Send
    + 'static,
    CoreEpochPublicInfo<NodeId>,
    CurrentEpochCryptographicProcessor<
        NodeId,
        KmsAdapter::CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    ServiceState<Backend::Settings, Dispatcher::Settings>,
    PendingTransactions,
    SchedulerWrapper<ChaCha20Rng, ProcessedMessage, EncapsulatedMessageWithVerifiedPublicHeader>,
    Backend,
    ChaCha20Rng,
)
where
    NodeId: Clone + Debug + Eq + Hash + Send + 'static,
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId> + Sync,
    Dispatcher: PayloadDispatcher<RuntimeServiceId>,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<KmsAdapter::CorePoQGenerator>,
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
                      pow_difficulty,
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
                        poq_pow_public_inputs: PowInputs {
                            pow_blend_difficulty: pow_difficulty,
                            pow_quota: config.epoch_pow_quota(),
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

    let current_epoch_poq_verification_inputs = PoQVerificationInputsMinusSigningKey {
        core: current_epoch_public_info.poq_core_public_inputs,
        leader: current_epoch_public_info.poq_leadership_public_inputs,
        pow: current_epoch_public_info.poq_pow_public_inputs,
    };

    // Initialize the current epoch state. If the epoch matches the stored one,
    // retrieves the tracked consumed core quota. Else, fallback to `0`.
    let current_recovery_checkpoint = match last_saved_state.take() {
        Some(saved_state) if saved_state.last_seen_epoch() == current_epoch_public_info.epoch => {
            tracing::trace!(
                target: LOG_TARGET,
                "Found recovery state for epoch {:?}: {saved_state:?}",
                current_epoch_public_info.epoch
            );
            saved_state
        }
        maybe_stale_state => {
            tracing::trace!(
                target: LOG_TARGET,
                "No recovery state found for epoch {:?}. Initializing a new one.",
                current_epoch_public_info.epoch
            );

            let current_epoch_reward_info = reward::EpochInfo::new(
                    current_epoch_public_info.epoch,
                    &current_epoch_public_info.poq_leadership_public_inputs.pol_epoch_nonce,
                    current_epoch_public_info.membership.size() as u64,
                    current_epoch_public_info.poq_core_public_inputs.quota,
                    blend_config.activity_threshold_sensitivity,
                ).expect("Reward epoch info must be created successfully. Panicking since the service cannot continue with this epoch");

            // Everything else in a stale state belongs to the epoch it was
            // saved under, but a transaction still waiting for a `PoW` solution
            // has not been encapsulated and so belongs to none: it outlives the
            // state that carried it, the same way it outlives an epoch rotation.
            //
            // The tokens that state collected are the exception. A state saved
            // under the immediately preceding epoch holds a full epoch's worth
            // of them, and they are still worth an activity proof: rotating that
            // collector here is the same move the running service makes at an
            // epoch boundary, and it hands the proof to the submission below.
            // A gap of two or more epochs is past submitting for, so it is
            // dropped.
            let (pending_transactions, recovered_old_epoch_token_collector) = maybe_stale_state
                .map_or_else(
                    || (VecDeque::new(), None),
                    |state| {
                        let is_previous_epoch = state.last_seen_epoch().strict_add(1.into())
                            == current_epoch_public_info.epoch;
                        let (_, _, _, _, pending_transactions, token_collector, ..) =
                            state.into_components();
                        let old_epoch_token_collector = is_previous_epoch.then(|| {
                            tracing::debug!(target: LOG_TARGET, "Recovered a token collector for the immediately preceding epoch. Rotating it so its activity proof is not lost.");
                            token_collector.rotate_epoch(&current_epoch_reward_info).1
                        });
                        (pending_transactions, old_epoch_token_collector)
                    },
                );

            ServiceState::with_epoch(
                current_epoch_public_info.epoch,
                pending_transactions,
                EpochBlendingTokenCollector::new(&current_epoch_reward_info),
                recovered_old_epoch_token_collector,
                state_updater,
            )
            .expect("service state should be created successfully")
        }
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

    let epoch_core_quota =
        blend_config.epoch_core_quota(current_epoch_public_info.membership.size());
    let spent_core_quota = current_recovery_checkpoint.spent_quota();

    let crypto_processor = CurrentEpochCryptographicProcessor::<
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
            pow_mining_pool: Arc::clone(&blend_config.pow_mining_pool),
            spent_core_quota,
        },
        current_epoch_poq_verification_inputs,
        current_epoch_core_poq_generator
            .expect("Core PoQ generator must be present at startup: the proxy service only launches CoreMode when the node is part of the core membership."),
        current_epoch_public_info.epoch,
    )
    .expect("The initial membership should satisfy the core node condition");

    let message_scheduler = SchedulerWrapper::new_with_initial_messages(
        SchedulerEpochInfo {
            core_quota: epoch_core_quota.saturating_sub(spent_core_quota),
            epoch: current_epoch_public_info.epoch,
        },
        release_delay_rng,
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
        BackendEpochInfo {
            membership: current_epoch_public_info.membership.clone(),
            epoch: current_epoch_public_info.epoch,
            // The backend verifies the `PoQ` of every message it receives before
            // relaying it, so it needs its own verifier for the epoch.
            proofs_verifier: ProofsVerifier::new(current_epoch_poq_verification_inputs),
        },
        ChaCha20Rng::from_entropy(),
    );

    // Rng for releasing messages.
    let rng = ChaCha20Rng::from_entropy();

    // The transactions a previous run had queued, back in the shared queue that
    // also holds proposals.
    let mut pending_transactions = PendingTransactions::new();
    for transaction in current_recovery_checkpoint.pending_transactions() {
        pending_transactions.queue(transaction.clone());
    }

    (
        remaining_epoch_stream,
        current_epoch_public_info,
        crypto_processor,
        current_recovery_checkpoint,
        pending_transactions,
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
    Dispatcher,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    mut inbound_relay: impl Stream<Item = ServiceMessage<NodeId>> + Send + Unpin,
    blend_messages: &mut (
             impl Stream<Item = (EncapsulatedMessageWithVerifiedPublicHeader, Epoch)>
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
    payload_dispatcher: &Dispatcher,
    sdp_relay: &OutboundRelay<SdpMessage>,
    rng: &mut Rng,
    current_epoch: CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>,
    mut pending_transactions: PendingTransactions,
    mut recovery_checkpoint: ServiceState<Backend::Settings, Dispatcher::Settings>,
) -> RetiringEpoch<Rng, ProofsVerifier>
where
    NodeId: Clone + Eq + Hash + Send + Sync + 'static,
    Rng: rand::Rng + Clone + Send + Unpin,
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId> + Sync + Send,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator> + Send,
    CorePoQGenerator: Send + Sync,
    ProofsVerifier: ProofsVerifierTrait + Send + Sync,
    RuntimeServiceId: Sync + Send,
{
    let mut latest_secret_pol_info: Option<PolEpochInfo> = None;
    let mut current_epoch_stage = current_epoch.into();
    loop {
        let epoch_outcome = match current_epoch_stage {
            Stage::Current(current) => {
                run_current_epoch(
                    &mut inbound_relay,
                    blend_messages,
                    &mut secret_pol_info_stream,
                    remaining_epoch_stream,
                    blend_config,
                    backend,
                    payload_dispatcher,
                    sdp_relay,
                    rng,
                    *current,
                    &mut pending_transactions,
                    &mut latest_secret_pol_info,
                    recovery_checkpoint,
                )
                .await
            }
            Stage::DuringTransition(during_transition) => {
                run_during_transition(
                    &mut inbound_relay,
                    blend_messages,
                    &mut secret_pol_info_stream,
                    remaining_epoch_stream,
                    blend_config,
                    backend,
                    payload_dispatcher,
                    sdp_relay,
                    rng,
                    *during_transition,
                    &mut pending_transactions,
                    &mut latest_secret_pol_info,
                    recovery_checkpoint,
                )
                .await
            }
        };
        match epoch_outcome {
            StageOutcome::NewEpoch {
                next,
                recovery_checkpoint: checkpoint,
            } => {
                current_epoch_stage = next;
                recovery_checkpoint = *checkpoint;
            }
            StageOutcome::Retiring(retiring_epoch) => {
                tracing::info!(target: LOG_TARGET, "Exiting from the main event loop");
                return *retiring_epoch;
            }
        }
    }
}

/// Which stage of its life the node's blending is in.
enum Stage<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng> {
    Current(Box<CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>>),
    DuringTransition(
        Box<
            CurrentEpochDuringTransition<
                NodeId,
                CorePoQGenerator,
                ProofsGenerator,
                ProofsVerifier,
                Rng,
            >,
        >,
    ),
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
    From<CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>>
    for Stage<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
{
    fn from(
        value: CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>,
    ) -> Self {
        Self::Current(Box::new(value))
    }
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
    From<
        CurrentEpochDuringTransition<
            NodeId,
            CorePoQGenerator,
            ProofsGenerator,
            ProofsVerifier,
            Rng,
        >,
    > for Stage<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
{
    fn from(
        value: CurrentEpochDuringTransition<
            NodeId,
            CorePoQGenerator,
            ProofsGenerator,
            ProofsVerifier,
            Rng,
        >,
    ) -> Self {
        Self::DuringTransition(Box::new(value))
    }
}

/// How a stage ended.
enum StageOutcome<
    NodeId,
    CorePoQGenerator,
    ProofsGenerator,
    ProofsVerifier,
    Rng,
    BackendSettings,
    NetworkSettings,
> {
    /// An epoch event moved the node to another stage.
    NewEpoch {
        next: Stage<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>,
        recovery_checkpoint: Box<ServiceState<BackendSettings, NetworkSettings>>,
    },
    /// The node is no longer a core node, or Blend is disabled.
    Retiring(Box<RetiringEpoch<Rng, ProofsVerifier>>),
}

/// The stage with no previous epoch left to drain.
#[expect(clippy::too_many_arguments, reason = "categorize args")]
async fn run_current_epoch<
    NodeId,
    Backend,
    Rng,
    Dispatcher,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    inbound_relay: &mut (impl Stream<Item = ServiceMessage<NodeId>> + Send + Unpin),
    blend_messages: &mut (
             impl Stream<Item = (EncapsulatedMessageWithVerifiedPublicHeader, Epoch)>
             + Send
             + Unpin
             + 'static
         ),
    secret_pol_info_stream: &mut (impl Stream<Item = PolEpochInfo> + Send + Unpin),
    remaining_epoch_stream: &mut (
             impl Stream<Item = EpochEvent<MaybeEmptyCoreEpochInfo<NodeId, CorePoQGenerator>>>
             + Unpin
             + Send
         ),
    blend_config: &RunningBlendConfig<Backend::Settings>,
    backend: &mut Backend,
    payload_dispatcher: &Dispatcher,
    sdp_relay: &OutboundRelay<SdpMessage>,
    rng: &mut Rng,
    mut current_epoch: CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>,
    pending_transactions: &mut PendingTransactions,
    latest_secret_pol_info: &mut Option<PolEpochInfo>,
    mut recovery_checkpoint: ServiceState<Backend::Settings, Dispatcher::Settings>,
) -> StageOutcome<
    NodeId,
    CorePoQGenerator,
    ProofsGenerator,
    ProofsVerifier,
    Rng,
    Backend::Settings,
    Dispatcher::Settings,
>
where
    NodeId: Clone + Eq + Hash + Send + Sync + 'static,
    Rng: rand::Rng + Clone + Send + Unpin,
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId> + Sync + Send,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator> + Send,
    CorePoQGenerator: Send + Sync,
    ProofsVerifier: ProofsVerifierTrait + Send + Sync,
    RuntimeServiceId: Sync + Send,
{
    loop {
        tokio::select! {
            Some(msg) = inbound_relay.next() => {
                recovery_checkpoint = handle_service_message(msg, current_epoch.proposals_mut(), pending_transactions, blend_config, backend, recovery_checkpoint).await;
            }
            Some(incoming_message) = blend_messages.next() => {
                let (scheduler, crypto) = current_epoch.decapsulation_borrows();
                recovery_checkpoint = handle_incoming_blend_message(incoming_message, scheduler, None, crypto.receiver(), None, recovery_checkpoint);
            }
            event = current_epoch.next_event(pending_transactions) => {
                recovery_checkpoint = handle_current_epoch_event(event, &mut current_epoch, pending_transactions, rng, backend, payload_dispatcher, recovery_checkpoint).await;
            }
            Some(pol_secret_info) = secret_pol_info_stream.next() => {
                apply_or_hold_secret_pol_info(pol_secret_info, &mut current_epoch, latest_secret_pol_info);
            }
            Some(epoch_event) = remaining_epoch_stream.next() => {
                match epoch_event {
                    // Not an epoch change, so this stage keeps everything it
                    // has — queued proposals included. There is nothing to
                    // drain here, but the expiry still has to be acknowledged.
                    EpochEvent::TransitionPeriodExpired => {
                        recovery_checkpoint = complete_transition_period(backend, sdp_relay, recovery_checkpoint).await;
                    }
                    EpochEvent::NewEpoch(new_epoch_info) => {
                        return rotate::<_, _, _, Dispatcher, _, _, _, RuntimeServiceId>(new_epoch_info, current_epoch.into_components(), latest_secret_pol_info, blend_config, backend, recovery_checkpoint).await;
                    }
                }
            }
        }
    }
}

/// The stage in which the epoch before this one is still within its transition
/// period, so both are live.
#[expect(clippy::too_many_arguments, reason = "categorize args")]
async fn run_during_transition<
    NodeId,
    Backend,
    Rng,
    Dispatcher,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    inbound_relay: &mut (impl Stream<Item = ServiceMessage<NodeId>> + Send + Unpin),
    blend_messages: &mut (
             impl Stream<Item = (EncapsulatedMessageWithVerifiedPublicHeader, Epoch)>
             + Send
             + Unpin
             + 'static
         ),
    secret_pol_info_stream: &mut (impl Stream<Item = PolEpochInfo> + Send + Unpin),
    remaining_epoch_stream: &mut (
             impl Stream<Item = EpochEvent<MaybeEmptyCoreEpochInfo<NodeId, CorePoQGenerator>>>
             + Unpin
             + Send
         ),
    blend_config: &RunningBlendConfig<Backend::Settings>,
    backend: &mut Backend,
    payload_dispatcher: &Dispatcher,
    sdp_relay: &OutboundRelay<SdpMessage>,
    rng: &mut Rng,
    mut during_transition: CurrentEpochDuringTransition<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
        Rng,
    >,
    pending_transactions: &mut PendingTransactions,
    latest_secret_pol_info: &mut Option<PolEpochInfo>,
    mut recovery_checkpoint: ServiceState<Backend::Settings, Dispatcher::Settings>,
) -> StageOutcome<
    NodeId,
    CorePoQGenerator,
    ProofsGenerator,
    ProofsVerifier,
    Rng,
    Backend::Settings,
    Dispatcher::Settings,
>
where
    NodeId: Clone + Eq + Hash + Send + Sync + 'static,
    Rng: rand::Rng + Clone + Send + Unpin,
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId> + Sync + Send,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator> + Send,
    CorePoQGenerator: Send + Sync,
    ProofsVerifier: ProofsVerifierTrait + Send + Sync,
    RuntimeServiceId: Sync + Send,
{
    loop {
        tokio::select! {
            Some(msg) = inbound_relay.next() => {
                recovery_checkpoint = handle_service_message(msg, during_transition.current_mut().proposals_mut(), pending_transactions, blend_config, backend, recovery_checkpoint).await;
            }
            Some(incoming_message) = blend_messages.next() => {
                let (scheduler, previous_scheduler, crypto, previous_crypto) = during_transition.decapsulation_borrows();
                recovery_checkpoint = handle_incoming_blend_message(incoming_message, scheduler, Some(previous_scheduler), crypto.receiver(), Some(previous_crypto), recovery_checkpoint);
            }
            event = during_transition.next_event(pending_transactions) => {
                match event {
                    DuringTransitionEvent::Current(event) => {
                        recovery_checkpoint = handle_current_epoch_event(event, during_transition.current_mut(), pending_transactions, rng, backend, payload_dispatcher, recovery_checkpoint).await;
                    }
                    DuringTransitionEvent::PreviousEpochReleaseRound(round_info, previous_epoch) => {
                        handle_release_round_for_old_epoch(round_info, rng, backend, payload_dispatcher, previous_epoch).await;
                    }
                }
            }
            Some(pol_secret_info) = secret_pol_info_stream.next() => {
                apply_or_hold_secret_pol_info(pol_secret_info, during_transition.current_mut(), latest_secret_pol_info);
            }
            Some(epoch_event) = remaining_epoch_stream.next() => {
                match epoch_event {
                    // The epoch being drained is finished with, but this one is
                    // not: `end_transition` keeps it whole, proposals and all.
                    EpochEvent::TransitionPeriodExpired => {
                        return StageOutcome::NewEpoch {
                            next: during_transition.end_transition().into(),
                            recovery_checkpoint: Box::new(complete_transition_period(backend, sdp_relay, recovery_checkpoint).await),
                        };
                    }
                    EpochEvent::NewEpoch(new_epoch_info) => {
                        return rotate::<_, _, _, Dispatcher, _, _, _, RuntimeServiceId>(new_epoch_info, during_transition.into_components(), latest_secret_pol_info, blend_config, backend, recovery_checkpoint).await;
                    }
                }
            }
        }
    }
}

/// Answers a message from another service, which both stages do the same way.
async fn handle_service_message<
    NodeId,
    Backend,
    ProofsVerifier,
    NetworkSettings,
    RuntimeServiceId,
>(
    message: ServiceMessage<NodeId>,
    pending_proposals: &mut PendingProposals,
    pending_transactions: &mut PendingTransactions,
    blend_config: &RunningBlendConfig<Backend::Settings>,
    backend: &Backend,
    recovery_checkpoint: ServiceState<Backend::Settings, NetworkSettings>,
) -> ServiceState<Backend::Settings, NetworkSettings>
where
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId> + Sync,
    NetworkSettings: Clone,
{
    match message {
        ServiceMessage::Blend(BlendPayload::Transaction(transaction)) => {
            queue_transaction_for_encapsulation(
                transaction,
                pending_transactions,
                recovery_checkpoint,
            )
        }
        ServiceMessage::Blend(BlendPayload::BlockProposal(proposal)) => {
            let copies = NonZeroU64::new(blend_config.data_replication_factor.strict_add(1))
                .expect("A block proposal is always sent at least once.");
            pending_proposals.queue(proposal, copies);
            recovery_checkpoint
        }
        ServiceMessage::GetNetworkInfo { reply } => {
            let info = backend.network_info().await;
            drop(reply.send(info));
            recovery_checkpoint
        }
        ServiceMessage::GetPendingTransactions { reply } => {
            drop(reply.send(pending_transactions.iter().cloned().collect()));
            recovery_checkpoint
        }
    }
}

/// Acts on something the current epoch produced, which both stages do the same
/// way.
async fn handle_current_epoch_event<
    NodeId,
    Backend,
    Rng,
    Dispatcher,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    event: CurrentEpochEvent,
    current_epoch: &mut CurrentEpoch<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
        Rng,
    >,
    pending_transactions: &mut PendingTransactions,
    rng: &mut Rng,
    backend: &Backend,
    payload_dispatcher: &Dispatcher,
    recovery_checkpoint: ServiceState<Backend::Settings, Dispatcher::Settings>,
) -> ServiceState<Backend::Settings, Dispatcher::Settings>
where
    NodeId: Eq + Hash + Send + Sync + 'static,
    Rng: rand::Rng + Clone + Send + Unpin,
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId> + Sync,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
{
    match event {
        CurrentEpochEvent::Encapsulated(encapsulation_result) => match encapsulation_result {
            EncapsulationResult::Complete(encapsulation) => {
                let LocalEncapsulation { message, kind } = *encapsulation;
                let (proposals, crypto_processor, scheduler) = current_epoch.scheduling_borrows();
                match kind {
                    MessageKind::Proposal => {
                        let checkpoint = schedule_local_encapsulated_message(
                            &message,
                            crypto_processor,
                            scheduler,
                            recovery_checkpoint,
                        );
                        proposals.mark_copy_as_sent();
                        checkpoint
                    }
                    MessageKind::Transaction => handle_local_transaction(
                        &message,
                        pending_transactions,
                        crypto_processor,
                        scheduler,
                        recovery_checkpoint,
                    ),
                }
            }
            // The head of whichever queue it came from can never be
            // encapsulated, so it goes rather than blocking everything behind
            // it.
            EncapsulationResult::Discard(MessageKind::Proposal) => {
                current_epoch.proposals_mut().discard_head();
                recovery_checkpoint
            }
            EncapsulationResult::Discard(MessageKind::Transaction) => {
                discard_unencapsulatable_transaction(pending_transactions, recovery_checkpoint)
            }
            EncapsulationResult::Retry => unreachable!(
                "`encapsulate_next_local_message` turns the encapsulation result into the `None` that disables its branch, so that the loop waits rather than spinning on a branch with nothing to do."
            ),
        },
        CurrentEpochEvent::ReleaseRound(round_info) => {
            handle_release_round(
                round_info,
                current_epoch.crypto_processor_mut(),
                rng,
                backend,
                payload_dispatcher,
                recovery_checkpoint,
            )
            .await
        }
    }
}

/// Applies this epoch's secret `PoL` info, or holds it for the epoch it names,
/// which both stages do the same way.
fn apply_or_hold_secret_pol_info<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>(
    pol_secret_info: PolEpochInfo,
    current_epoch: &mut CurrentEpoch<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
        Rng,
    >,
    latest_secret_pol_info: &mut Option<PolEpochInfo>,
) where
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
{
    if current_epoch.epoch_info().epoch == pol_secret_info.epoch {
        // Apply now: move the winning-slot stream into the current processor.
        current_epoch.crypto_processor_mut().set_epoch_private(
            pol_secret_info.winning_pol_info_stream,
            pol_secret_info.epoch,
        );
        *latest_secret_pol_info = None;
    } else {
        // Belongs to an upcoming epoch: keep it to seed that epoch's processor
        // when the rotation happens.
        *latest_secret_pol_info = Some(pol_secret_info);
    }
}

/// Turns an epoch event into the stage that follows, which is the only thing
/// either stage ends on.
async fn rotate<
    NodeId,
    Backend,
    Rng,
    Dispatcher,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    new_epoch_info: MaybeEmptyCoreEpochInfo<NodeId, CorePoQGenerator>,
    components: Components<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>,
    latest_secret_pol_info: &mut Option<PolEpochInfo>,
    blend_config: &RunningBlendConfig<Backend::Settings>,
    backend: &mut Backend,
    recovery_checkpoint: ServiceState<Backend::Settings, Dispatcher::Settings>,
) -> StageOutcome<
    NodeId,
    CorePoQGenerator,
    ProofsGenerator,
    ProofsVerifier,
    Rng,
    Backend::Settings,
    Dispatcher::Settings,
>
where
    NodeId: Clone + Eq + Hash + Send,
    Rng: rand::Rng + Clone + Unpin,
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId>,
    Dispatcher: PayloadDispatcher<RuntimeServiceId>,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
{
    // The epoch's own components go in; whatever it also held — its queued
    // proposals — is dropped here, which is the whole reason they live on it.
    let (crypto_processor, message_scheduler, _) = components;
    match handle_epoch_event(
        new_epoch_info,
        blend_config,
        crypto_processor,
        message_scheduler,
        recovery_checkpoint,
        backend,
        latest_secret_pol_info,
    )
    .await
    {
        HandleEpochEventOutput::Transitioning {
            current_epoch,
            new_recovery_checkpoint,
            old_epoch_components,
        } => StageOutcome::NewEpoch {
            next: CurrentEpochDuringTransition::new(*current_epoch, *old_epoch_components).into(),
            recovery_checkpoint: new_recovery_checkpoint,
        },
        HandleEpochEventOutput::Retiring { retiring_epoch } => {
            StageOutcome::Retiring(retiring_epoch)
        }
    }
}

/// Records a transaction as waiting for a `PoW` solution to back its layer
/// proofs.
fn queue_transaction_for_encapsulation<BackendSettings, NetworkSettings>(
    transaction: Vec<u8>,
    pending_transactions: &mut PendingTransactions,
    current_recovery_checkpoint: ServiceState<BackendSettings, NetworkSettings>,
) -> ServiceState<BackendSettings, NetworkSettings>
where
    BackendSettings: Clone,
{
    pending_transactions.queue(transaction.clone());
    let mut state_updater = current_recovery_checkpoint.start_updating();
    state_updater.queue_unencapsulated_transaction(transaction);
    state_updater.commit_changes()
}

/// Encapsulates one locally-originated message, once proofs back it.
///
/// Proposals go first: one is tied to the slot it was built for and goes stale,
/// whereas a transaction keeps.
///
/// Neither queue is popped here due to `tokio-select` cancellation safety. The
/// caller updates the queues once the race is settled, which is also why only
/// one copy of a proposal is wrapped per call.
///
/// Returns `None` when there is nothing to hand back — nothing queued, or the
/// branch that would back the message has no proofs yet. Both mean "do nothing
/// this time round", and the message stays queued for the next. `Err(())` means
/// the head can never be encapsulated and has to go: the head is retried before
/// anything else is looked at, so one that keeps failing would take everything
/// queued behind it down with it.
async fn encapsulate_next_local_message<NodeId, ProofsGenerator, ProofsVerifier, CorePoQGenerator>(
    pending_proposals: &PendingProposals,
    pending_transactions: &PendingTransactions,
    cryptographic_processor: &mut CurrentEpochCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
) -> Option<EncapsulationResult>
where
    NodeId: Eq + Hash + 'static,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
{
    let (kind, encapsulated) = match next_local_message(pending_proposals, pending_transactions)? {
        NextLocalMessage::ProposalCopy(proposal) => (
            MessageKind::Proposal,
            cryptographic_processor
                .encapsulate_block_proposal_payload(proposal)
                .await,
        ),
        NextLocalMessage::Transaction(transaction) => (
            MessageKind::Transaction,
            cryptographic_processor
                .encapsulate_transaction_payload(transaction)
                .await,
        ),
    };

    match resolve_encapsulation(encapsulated, kind) {
        // Not something the caller acts on, and handing it back would be worse
        // than useless: the branch this feeds would then complete immediately
        // every time round with nothing to do, which is a busy loop rather than
        // a wait. `None` fails the branch's pattern and disables it instead.
        EncapsulationResult::Retry => None,
        processed => Some(processed),
    }
}

/// Drops the queued message that cannot be encapsulated, taking it out of the
/// recovery state too so a restart does not bring it back to fail again.
fn discard_unencapsulatable_transaction<BackendSettings, NetworkSettings>(
    pending_transactions: &mut PendingTransactions,
    current_recovery_checkpoint: ServiceState<BackendSettings, NetworkSettings>,
) -> ServiceState<BackendSettings, NetworkSettings>
where
    BackendSettings: Clone,
{
    let Some(transaction) = pending_transactions.discard_head() else {
        return current_recovery_checkpoint;
    };
    let mut state_updater = current_recovery_checkpoint.start_updating();
    state_updater.dequeue_unencapsulated_transaction(&transaction);
    state_updater.commit_changes()
}

/// Processes a transaction whose wait for a `PoW` solution is over.
fn handle_local_transaction<
    NodeId,
    Rng,
    BackendSettings,
    NetworkSettings,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
>(
    encapsulation: &EncapsulatedMessageWithVerifiedPublicHeader,
    pending_transactions: &mut PendingTransactions,
    cryptographic_processor: &CurrentEpochCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    scheduler: &mut EpochMessageScheduler<
        Rng,
        ProcessedMessage,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    current_recovery_checkpoint: ServiceState<BackendSettings, NetworkSettings>,
) -> ServiceState<BackendSettings, NetworkSettings>
where
    NodeId: Eq + Hash + Send + 'static,
    Rng: RngCore + Clone + Send + Unpin,
    BackendSettings: Clone + Send + Sync,
    ProofsVerifier: ProofsVerifierTrait,
{
    let recovery_checkpoint = schedule_local_encapsulated_message(
        encapsulation,
        cryptographic_processor,
        scheduler,
        current_recovery_checkpoint,
    );

    let transaction = pending_transactions.mark_as_sent();
    let mut state_updater = recovery_checkpoint.start_updating();
    state_updater.dequeue_unencapsulated_transaction(&transaction);
    state_updater.commit_changes()
}

/// Processes the old epoch during the epoch transition period
/// before retiring the core service.
async fn retire<
    NodeId,
    Backend,
    Rng,
    Dispatcher,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    mut blend_messages: impl Stream<Item = EncapsulatedMessageWithVerifiedPublicHeader>
    + Unpin
    + Send
    + 'static,
    mut remaining_epoch_stream: impl Stream<
        Item = EpochEvent<MaybeEmptyCoreEpochInfo<NodeId, CorePoQGenerator>>,
    > + Send
    + Unpin,
    mut backend: Backend,
    payload_dispatcher: Dispatcher,
    sdp_relay: OutboundRelay<SdpMessage>,
    mut rng: Rng,
    mut retiring_epoch: RetiringEpoch<Rng, ProofsVerifier>,
) where
    NodeId: Clone + Eq + Hash + Send + Sync + 'static,
    Rng: rand::Rng + Clone + Send + Unpin,
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId> + Send + Sync,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Send + Sync,
    CorePoQGenerator: Send + Sync,
    ProofsVerifier: ProofsVerifierTrait + Send + Sync,
    RuntimeServiceId: Send + Sync,
{
    loop {
        let epoch = retiring_epoch.epoch();
        tokio::select! {
            Some(incoming_message) = blend_messages.next() => {
                let (crypto_processor, message_scheduler, blending_token_collector) = retiring_epoch.split_mut();
                handle_incoming_blend_message_from_old_epoch(incoming_message, message_scheduler, crypto_processor, blending_token_collector);
            }
            Some(round_info) = retiring_epoch.scheduler_mut().next() => {
                handle_release_round_for_old_epoch(round_info, &mut rng, &backend, &payload_dispatcher, epoch).await;
            }
            Some(EpochEvent::TransitionPeriodExpired) = remaining_epoch_stream.next() => {
                handle_epoch_transition_expired(&mut backend, retiring_epoch.into_tokens(), &sdp_relay).await;
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
#[expect(clippy::too_many_lines, reason = "necessary for epoch handling")]
async fn handle_epoch_event<
    NodeId,
    ProofsGenerator,
    ProofsVerifier,
    Backend,
    NetworkSettings,
    Rng,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    new_epoch_info: MaybeEmptyCoreEpochInfo<NodeId, CorePoQGenerator>,
    settings: &RunningBlendConfig<Backend::Settings>,
    current_cryptographic_processor: CurrentEpochCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    current_scheduler: EpochMessageScheduler<
        Rng,
        ProcessedMessage,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    current_recovery_checkpoint: ServiceState<Backend::Settings, NetworkSettings>,
    backend: &mut Backend,
    current_secret_info: &mut Option<PolEpochInfo>,
) -> HandleEpochEventOutput<
    NodeId,
    Rng,
    ProofsGenerator,
    ProofsVerifier,
    Backend::Settings,
    NetworkSettings,
    CorePoQGenerator,
>
where
    NodeId: Eq + Hash + Clone + Send,
    Rng: rand::Rng + Clone + Unpin,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId>,
{
    match new_epoch_info {
        MaybeEmptyCoreEpochInfo::NonEmpty(core_epoch_info) => {
            let CoreEpochInfo {
                core_poq_generator: new_core_poq_generator,
                public: new_epoch_info,
            } = *core_epoch_info;
            // Once a new epoch starts, the old epoch's proving is useless: retiring
            // its processor into a receive-only one for the transition period drops
            // the generators, and with them the `PoW` mining they have in flight.
            let old_cryptographic_processor = current_cryptographic_processor.rotate_epoch();
            // Queued proposals go with it, and for the same reason: the rotation
            // is what makes them unsendable. Anything not yet encapsulated would
            // now draw on the new epoch's leadership quota — one message's worth
            let (
                _,
                _,
                _,
                _,
                pending_transactions,
                current_epoch_blending_token_collector,
                _,
                state_updater,
            ) = current_recovery_checkpoint.into_components();

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

            let new_poq_verification_inputs = PoQVerificationInputsMinusSigningKey {
                core: new_epoch_info.poq_core_public_inputs,
                leader: new_epoch_info.poq_leadership_public_inputs,
                pow: new_epoch_info.poq_pow_public_inputs,
            };
            backend
                .rotate_epoch(BackendEpochInfo {
                    membership: new_epoch_info.membership.clone(),
                    epoch: new_epoch_info.epoch,
                    proofs_verifier: ProofsVerifier::new(new_poq_verification_inputs),
                })
                .await;

            let new_scheduler_epoch_info = SchedulerEpochInfo {
                core_quota: settings.epoch_core_quota(new_epoch_info.membership.size()),
                epoch: new_epoch_info.epoch,
            };

            let Some(core_poq_generator) = new_core_poq_generator else {
                tracing::info!(target: LOG_TARGET, "Local node is not part of new membership. Retiring from core.");
                return HandleEpochEventOutput::Retiring {
                    retiring_epoch: Box::new(RetiringEpoch::new(
                        TransitioningEpoch::new(
                            old_cryptographic_processor,
                            current_scheduler
                                .rotate_epoch(
                                    new_scheduler_epoch_info,
                                    settings.scheduler_settings(),
                                )
                                .1,
                        ),
                        old_epoch_blending_token_collector,
                    )),
                };
            };

            let new_processor: CurrentEpochCryptographicProcessor<_, _, _, ProofsVerifier> =
                match CurrentEpochCryptographicProcessor::try_new_with_core_condition_check(
                    new_epoch_info.membership.clone(),
                    settings.minimum_network_size,
                    EpochCryptographicProcessorSettings {
                        non_ephemeral_encryption_key: settings
                            .non_ephemeral_signing_key
                            .derive_x25519(),
                        num_blend_layers: settings.num_blend_layers,
                        pow_mining_pool: Arc::clone(&settings.pow_mining_pool),
                        spent_core_quota: Quota::ZERO,
                    },
                    new_poq_verification_inputs,
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
                            retiring_epoch: Box::new(RetiringEpoch::new(
                                TransitioningEpoch::new(
                                    old_cryptographic_processor,
                                    current_scheduler
                                        .rotate_epoch(
                                            new_scheduler_epoch_info,
                                            settings.scheduler_settings(),
                                        )
                                        .1,
                                ),
                                old_epoch_blending_token_collector,
                            )),
                        };
                    }
                };

            let (new_scheduler, old_scheduler) = current_scheduler
                .rotate_epoch(new_scheduler_epoch_info, settings.scheduler_settings());
            let new_recovery_checkpoint = ServiceState::with_epoch(
                new_epoch_info.epoch,
                pending_transactions,
                new_epoch_blending_token_collector,
                Some(old_epoch_blending_token_collector),
                state_updater,
            )
            .expect("service state should be created successfully");
            HandleEpochEventOutput::Transitioning {
                current_epoch: Box::new(CurrentEpoch::new(
                    new_processor,
                    new_scheduler,
                    new_epoch_info,
                )),
                old_epoch_components: Box::new(TransitioningEpoch::new(
                    old_cryptographic_processor,
                    old_scheduler,
                )),
                new_recovery_checkpoint: Box::new(new_recovery_checkpoint),
            }
        }
        MaybeEmptyCoreEpochInfo::Empty { epoch, epoch_nonce } => {
            tracing::info!(target: LOG_TARGET, "New epoch event received, but no epoch info is available due to empty membership set.");
            let old_cryptographic_processor = current_cryptographic_processor.rotate_epoch();
            let (_, _, _, _, _, current_epoch_blending_token_collector, _, _) =
                current_recovery_checkpoint.into_components();
            let new_reward_epoch_info = reward::EpochInfo::new(
                epoch,
                &epoch_nonce,
                0,
                Quota::ZERO,
                settings.activity_threshold_sensitivity,
            )
            .expect("Reward epoch info must be created successfully. Panicking since the service cannot continue with this epoch");
            let (_, old_epoch_blending_token_collector) =
                current_epoch_blending_token_collector.rotate_epoch(&new_reward_epoch_info);
            HandleEpochEventOutput::Retiring {
                retiring_epoch: Box::new(RetiringEpoch::new(
                    TransitioningEpoch::new(
                        old_cryptographic_processor,
                        current_scheduler.consume(),
                    ),
                    old_epoch_blending_token_collector,
                )),
            }
        }
    }
}

/// Handles [`EpochEvent::TransitionPeriodExpired`]: the epoch that was being
/// drained is finished with, and what it earned is submitted.
///
/// Takes nothing belonging to the current epoch, because the transition period
/// ending is not an epoch change — which is what lets the caller keep that
/// epoch whole rather than taking it apart and putting it back together.
async fn complete_transition_period<
    Backend,
    NodeId,
    Rng,
    ProofsVerifier,
    NetworkSettings,
    RuntimeServiceId,
>(
    backend: &mut Backend,
    sdp_relay: &OutboundRelay<SdpMessage>,
    current_recovery_checkpoint: ServiceState<Backend::Settings, NetworkSettings>,
) -> ServiceState<Backend::Settings, NetworkSettings>
where
    Backend: BlendBackend<NodeId, Rng, ProofsVerifier, RuntimeServiceId>,
    NodeId: Clone + Eq + Hash + Send,
    NetworkSettings: Clone,
{
    let mut state_updater = current_recovery_checkpoint.start_updating();
    if let Some(old_token_collector) = state_updater.clear_old_epoch_token_collector() {
        handle_epoch_transition_expired(backend, old_token_collector, sdp_relay).await;
    }
    state_updater.commit_changes()
}

/// Handles [`EpochEvent::TransitionPeriodExpired`].
async fn handle_epoch_transition_expired<Backend, NodeId, Rng, ProofsVerifier, RuntimeServiceId>(
    backend: &mut Backend,
    blending_token_collector: OldEpochBlendingTokenCollector,
    sdp_relay: &OutboundRelay<SdpMessage>,
) where
    Backend: BlendBackend<NodeId, Rng, ProofsVerifier, RuntimeServiceId>,
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
    NetworkSettings,
    CorePoQGenerator,
> {
    Transitioning {
        current_epoch:
            Box<CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>>,
        new_recovery_checkpoint: Box<ServiceState<BackendSettings, NetworkSettings>>,
        old_epoch_components: Box<TransitioningEpoch<Rng, ProofsVerifier>>,
    },
    Retiring {
        retiring_epoch: Box<RetiringEpoch<Rng, ProofsVerifier>>,
    },
}

/// Schedules a locally-generated, already-encapsulated data message for
/// release.
///
/// Before scheduling, the outermost layers addressed to this node are
/// self-decapsulated so that blending tokens are collected immediately and only
/// the remaining layers (or the fully unwrapped message) are scheduled for the
/// next release round.
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
fn schedule_local_encapsulated_message<
    NodeId,
    Rng,
    BackendSettings,
    NetworkSettings,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
>(
    wrapped_message: &EncapsulatedMessageWithVerifiedPublicHeader,
    cryptographic_processor: &CurrentEpochCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    scheduler: &mut EpochMessageScheduler<
        Rng,
        ProcessedMessage,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    current_recovery_checkpoint: ServiceState<BackendSettings, NetworkSettings>,
) -> ServiceState<BackendSettings, NetworkSettings>
where
    NodeId: Eq + Hash + Send + 'static,
    Rng: RngCore + Clone + Send + Unpin,
    BackendSettings: Clone + Send + Sync,
    ProofsVerifier: ProofsVerifierTrait,
{
    let mut state_updater = current_recovery_checkpoint.start_updating();

    // Before blending the data message, we try to peel off any outer layers that
    // are addressed to us. In this case, we collect the blending tokens and we
    // blend only the remaining layers.
    let self_decapsulation_output = cryptographic_processor
        .receiver()
        .decapsulate_message_recursive(wrapped_message.clone());

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
            let data_message = match fully_decapsulated_message.into_components() {
                (PayloadType::BlockProposal, encoded_block_proposal) => {
                    BlendPayload::BlockProposal(encoded_block_proposal)
                }
                (PayloadType::Transaction, encoded_transaction) => {
                    BlendPayload::Transaction(encoded_transaction)
                }
                (PayloadType::Cover, _) => {
                    panic!(
                        "Locally-generated and fully-decapsulated message should be a data message."
                    );
                }
            };
            tracing::trace!(target: LOG_TARGET, "Locally generated data message of {} bytes had all the {} layers addressed to this same node. Propagating only the fully decapsulated message.", data_message.len(), blending_tokens.len());
            ProcessedMessage::from(data_message)
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

/// Processes an incoming Blend message received from a core or edge peer.
///
/// The backend has already verified the message's whole public header — `PoQ`
/// included, which is what gated it from being relayed to the rest of the
/// network — so all that is left here is to decapsulate it with the current or
/// old epoch's cryptographic processor, depending on the epoch it comes from.
fn handle_incoming_blend_message<Rng, BackendSettings, NetworkSettings, ProofsVerifier>(
    (verified_message, epoch): (EncapsulatedMessageWithVerifiedPublicHeader, Epoch),
    scheduler: &mut EpochMessageScheduler<
        Rng,
        ProcessedMessage,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    old_epoch_scheduler: Option<
        &mut OldEpochMessageScheduler<
            Rng,
            ProcessedMessage,
            EncapsulatedMessageWithVerifiedPublicHeader,
        >,
    >,
    cryptographic_processor: &ReceiverCryptographicProcessor<ProofsVerifier>,
    old_epoch_cryptographic_processor: Option<&OldEpochCryptographicProcessor<ProofsVerifier>>,
    current_recovery_checkpoint: ServiceState<BackendSettings, NetworkSettings>,
) -> ServiceState<BackendSettings, NetworkSettings>
where
    Rng: RngCore + Clone + Send + Unpin,
    BackendSettings: Clone,
    ProofsVerifier: ProofsVerifierTrait,
{
    if epoch == cryptographic_processor.epoch() {
        let Some(output) = try_decapsulate(verified_message, cryptographic_processor, epoch) else {
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
        let Some(output) = try_decapsulate(verified_message, old_cryptographic_processor, epoch)
        else {
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

/// Attempts recursive decapsulation of a message whose `PoQ` has already been
/// verified. Returns `None` if decapsulation fails (already logged).
fn try_decapsulate<ProofsVerifier>(
    message: EncapsulatedMessageWithVerifiedPublicHeader,
    processor: &ReceiverCryptographicProcessor<ProofsVerifier>,
    epoch: Epoch,
) -> Option<MultiLayerDecapsulationOutput>
where
    ProofsVerifier: ProofsVerifierTrait,
{
    match processor.decapsulate_message_recursive(message) {
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
fn handle_incoming_blend_message_from_old_epoch<Rng, ProofsVerifier>(
    verified_message: EncapsulatedMessageWithVerifiedPublicHeader,
    scheduler: &mut OldEpochMessageScheduler<
        Rng,
        ProcessedMessage,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    cryptographic_processor: &OldEpochCryptographicProcessor<ProofsVerifier>,
    blending_token_collector: &mut OldEpochBlendingTokenCollector,
) where
    ProofsVerifier: ProofsVerifierTrait,
{
    let Some(output) = try_decapsulate(
        verified_message,
        cryptographic_processor,
        cryptographic_processor.epoch(),
    ) else {
        return;
    };
    let (_, blending_tokens) =
        schedule_decapsulated_incoming_message(output, scheduler, cryptographic_processor);
    for blending_token in blending_tokens {
        blending_token_collector.collect(blending_token);
    }
}

/// Schedules a decapsulated incoming message from the current epoch,
/// and collects the blending tokens obtained from the decapsulation.
///
/// It updates the recovery checkpoint by storing the scheduled message
/// and the collected tokens.
fn handle_decapsulated_incoming_message_from_current_epoch<
    Rng,
    BackendSettings,
    NetworkSettings,
    ProofsVerifier,
>(
    multi_layer_decapsulation_output: MultiLayerDecapsulationOutput,
    scheduler: &mut EpochMessageScheduler<
        Rng,
        ProcessedMessage,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    current_recovery_checkpoint: ServiceState<BackendSettings, NetworkSettings>,
    cryptographic_processor: &ReceiverCryptographicProcessor<ProofsVerifier>,
) -> ServiceState<BackendSettings, NetworkSettings>
where
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
    BackendSettings,
    NetworkSettings,
    ProofsVerifier,
>(
    multi_layer_decapsulation_output: MultiLayerDecapsulationOutput,
    scheduler: &mut OldEpochMessageScheduler<
        Rng,
        ProcessedMessage,
        EncapsulatedMessageWithVerifiedPublicHeader,
    >,
    recovery_checkpoint: ServiceState<BackendSettings, NetworkSettings>,
    old_cryptographic_processor: &OldEpochCryptographicProcessor<ProofsVerifier>,
) -> ServiceState<BackendSettings, NetworkSettings>
where
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
fn schedule_decapsulated_incoming_message<ProofsVerifier>(
    multi_layer_decapsulation_output: MultiLayerDecapsulationOutput,
    scheduler: &mut impl ProcessedMessageScheduler<ProcessedMessage>,
    cryptographic_processor: &ReceiverCryptographicProcessor<ProofsVerifier>,
) -> (
    Option<ProcessedMessage>,
    impl Iterator<Item = BlendingToken>,
)
where
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
            let data_message = match fully_decapsulated_message.into_components() {
                (PayloadType::BlockProposal, encoded_block_proposal) => {
                    BlendPayload::BlockProposal(encoded_block_proposal)
                }
                (PayloadType::Transaction, encoded_transaction) => {
                    BlendPayload::Transaction(encoded_transaction)
                }
                (PayloadType::Cover, _) => {
                    tracing::trace!(target: LOG_TARGET, "Discarding received cover message.");
                    return (None, blending_tokens.into_iter());
                }
            };
            tracing::trace!(
                target: LOG_TARGET,
                "Processing a fully decapsulated {:?} message of {} bytes.",
                data_message.payload_type(),
                data_message.len()
            );
            let processed_message = ProcessedMessage::from(data_message);
            scheduler.schedule_processed_message(processed_message.clone());
            (Some(processed_message), blending_tokens.into_iter())
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
    Dispatcher,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
    RuntimeServiceId,
>(
    RoundInfo {
        data_messages,
        release_type,
    }: RoundInfo<ProcessedMessage, EncapsulatedMessageWithVerifiedPublicHeader>,
    cryptographic_processor: &mut CurrentEpochCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    rng: &mut Rng,
    backend: &Backend,
    payload_dispatcher: &Dispatcher,
    current_recovery_checkpoint: ServiceState<Backend::Settings, Dispatcher::Settings>,
) -> ServiceState<Backend::Settings, Dispatcher::Settings>
where
    NodeId: Eq + Hash + 'static,
    Rng: RngCore + Send,
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId> + Sync,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
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
        }).map(
            |data_message_to_blend| -> BoxFuture<'_, ()> {
                backend.publish(data_message_to_blend, current_epoch).boxed()
            },
        ).collect::<Vec<_>>();

    let processed_messages_relay_futures = build_futures_to_release_processed_messages(
        processed_messages,
        backend,
        payload_dispatcher,
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

async fn handle_release_round_for_old_epoch<
    NodeId,
    Rng,
    Backend,
    Dispatcher,
    ProofsVerifier,
    RuntimeServiceId,
>(
    RoundInfo {
        data_messages,
        release_type,
    }: RoundInfo<ProcessedMessage, EncapsulatedMessageWithVerifiedPublicHeader>,
    rng: &mut Rng,
    backend: &Backend,
    payload_dispatcher: &Dispatcher,
    epoch: Epoch,
) where
    NodeId: Eq + Hash + 'static,
    Rng: RngCore + Send,
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId> + Sync,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
{
    // The old epoch never generates cover traffic, so the cover flag is always
    // `false` here.
    let (processed_messages, _) =
        release_type.map_or_else(|| (vec![], false), RoundReleaseType::into_components);
    let (data_count, processed_count) = (data_messages.len(), processed_messages.len());

    // Data messages the epoch left unreleased carry its `PoQ`, which only verifies
    // against that epoch's public inputs, so they are published under the old
    // epoch's number and therefore to the peers still negotiated for it. They are
    // not tracked in the new epoch's recovery state, which was reset on rotation,
    // and they do not consume the new epoch's core quota, since they neither spend
    // it nor reach current-epoch peers.
    let data_messages_relay_futures =
        data_messages
            .into_iter()
            .map(|data_message_to_blend| -> BoxFuture<'_, ()> {
                backend.publish(data_message_to_blend, epoch).boxed()
            });

    let mut futures = data_messages_relay_futures
        .chain(build_futures_to_release_processed_messages(
            processed_messages,
            backend,
            payload_dispatcher,
            None,
            epoch,
        ))
        .collect::<Vec<_>>();
    futures.shuffle(rng);

    // Release all messages concurrently, and wait for all of them to be sent.
    join_all(futures).await;
    log_old_epoch_release_summary(data_count, processed_count);
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

fn log_old_epoch_release_summary(data_count: usize, processed_count: usize) {
    if data_count > 0 || processed_count > 0 {
        tracing::debug!(
            target: LOG_TARGET,
            "Sent out {data_count} data and {processed_count} processed messages at this release window for the old epoch"
        );
    } else {
        tracing::trace!(
            target: LOG_TARGET,
            "Sent out {data_count} data and {processed_count} processed messages at this release window for the old epoch"
        );
    }
}

fn build_futures_to_release_processed_messages<
    'fut,
    NodeId,
    Backend,
    Dispatcher,
    ProofsVerifier,
    RuntimeServiceId,
>(
    processed_messages_to_release: Vec<ProcessedMessage>,
    backend: &'fut Backend,
    payload_dispatcher: &'fut Dispatcher,
    mut state_updater: Option<&mut ServiceStateUpdater<Backend::Settings, Dispatcher::Settings>>,
    epoch: Epoch,
) -> Vec<BoxFuture<'fut, ()>>
where
    NodeId: Eq + Hash + 'static,
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId> + Sync,
    Dispatcher: PayloadDispatcher<RuntimeServiceId> + Sync,
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
                    ProcessedMessage::Decapsulated(payload) => {
                        payload_dispatcher.dispatch(payload).boxed()
                    }
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
    NetworkSettings,
    ProofsGenerator,
    ProofsVerifier,
    CorePoQGenerator,
>(
    cryptographic_processor: &mut CurrentEpochCryptographicProcessor<
        NodeId,
        CorePoQGenerator,
        ProofsGenerator,
        ProofsVerifier,
    >,
    state_updater: &mut state::StateUpdater<BackendSettings, NetworkSettings>,
) -> Option<EncapsulatedMessage>
where
    NodeId: Eq + Hash + 'static,
    BackendSettings: Sync,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
    ProofsVerifier: ProofsVerifierTrait,
{
    let encapsulated_cover_message = cryptographic_processor
        .encapsulate_cover_payload(&random_sized_bytes::<{ size_of::<u32>() }>())
        .await
        .expect("Should not fail to generate new cover message");
    // Each message consumes `num_blend_layers` indices.
    state_updater.consume_core_quota(
        Quota::try_new(cryptographic_processor.num_blend_layers().get())
            .expect("Number of blend layers must fit within the `PoQ` quota width."),
    );
    let self_decapsulation_output = cryptographic_processor
        .receiver()
        .decapsulate_message_recursive(encapsulated_cover_message.clone());
    let Ok(multi_layer_decapsulation_output) = self_decapsulation_output else {
        // First layer not addressed to ourselves, so it goes out fully encapsulated.
        // The quota it spent was already recorded above.
        tracing::trace!(target: LOG_TARGET, "Locally generated cover message does not have its outermost layer addressed to us. Sending it out fully encapsulated...");
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
    let proof_epoch = proof.epoch();
    debug!(
        target: LOG_TARGET,
        diagnostic = "blend_tsi_outage",
        event = "sdp_activity_proof_submission_requested",
        proof_epoch = u32::from(proof_epoch),
        signing_key = ?proof.token().signing_key(),
        "Requested activity proof submission to SDP"
    );
    let result = sdp_relay
        .send(SdpMessage::PostActivity {
            metadata: ActivityMetadata::Blend(Box::new((&proof).into())),
        })
        .await
        .map_err(|(e, _)| e);
    match &result {
        Ok(()) => debug!(
            target: LOG_TARGET,
            diagnostic = "blend_tsi_outage",
            event = "sdp_activity_proof_submitted",
            proof_epoch = u32::from(proof_epoch),
            signing_key = ?proof.token().signing_key(),
            "Submitted activity proof to SDP"
        ),
        Err(error) => error!(
            target: LOG_TARGET,
            diagnostic = "blend_tsi_outage",
            event = "sdp_activity_proof_submission_failed",
            proof_epoch = u32::from(proof_epoch),
            signing_key = ?proof.token().signing_key(),
            error = ?error,
            "Failed to submit activity proof to SDP"
        ),
    }
    result
}
