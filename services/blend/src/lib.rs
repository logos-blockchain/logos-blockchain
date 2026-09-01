use std::{
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    time::Duration,
};

use async_trait::async_trait;
use futures::StreamExt as _;
pub use lb_blend::message::{crypto::proofs::RealProofsVerifier, encap::ProofsVerifier};
use lb_blend::scheduling::{
    epoch::UninitializedEpochEventStream,
    message_blend::provers::{
        core_leader_and_pow::CoreLeaderAndPowProofsGenerator,
        leader_and_pow::LeaderAndPowProofsGenerator,
    },
};
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::{
    mantle::NoteId,
    sdp::{DeclarationId, DeclarationMessage, Locator, ProviderId, ServiceType},
};
use lb_key_management_system_service::{
    api::KmsServiceApi,
    keys::{Ed25519PublicKey, PublicKeyEncoding, ZkPublicKey},
};
use lb_log_targets::blend;
use lb_network_service::NetworkService;
use lb_sdp_service::{SdpMessage, SdpServiceApi};
use lb_services_utils::{
    overwatch::recovery::RecoveryBackend as RecoveryBackendTrait, wait_until_services_are_ready,
};
use lb_time_service::TimeService;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use rand_chacha::ChaCha20Rng;
use tracing::{debug, error, info};

use crate::{
    broadcast::{BlendService as BroadcastBlendService, Components as BroadcastComponents},
    core::{
        BlendService as CoreBlendService,
        backends::BlendBackend as CoreBlendBackend,
        dispatcher::PayloadDispatcher as PayloadDispatcherTrait,
        kms::PreloadKMSBackendCorePoQGenerator,
        service_components::{
            BackendSettingsOf, ChainNetworkOfComponents, Components as CoreComponents,
            MempoolOfComponents, NetworkBackendOfComponents, NetworkSettingsOf,
            RecoveryStateOf as CoreRecoveryStateOf,
        },
    },
    edge::{
        BlendService as EdgeBlendService,
        backends::BlendBackend as EdgeBlendBackend,
        service_components::{Components as EdgeComponents, EdgeBackendSettingsOf},
    },
    epoch_info::PolInfoProvider as PolInfoProviderTrait,
    kms::PreloadKmsService,
    membership::{MembershipInfo, chain::BlendEpochState, node_id},
    message::{ProxyServiceMessage, ServiceMessage},
    orchestrator::Instance,
    settings::Settings,
};

pub mod api;
pub mod broadcast;
pub mod core;
pub mod delivery;
pub mod edge;
pub mod epoch;
pub mod epoch_info;
pub mod membership;
pub mod message;
pub(crate) mod metrics;
pub mod settings;

mod kms;
mod mode;
mod orchestrator;
mod pending;
mod service_components;
pub use self::{mode::Mode, service_components::ServiceComponents};

#[cfg(test)]
mod test_utils;

const LOG_TARGET: &str = blend::service::ROOT;

/// The Blend orchestrator.
///
/// It owns no blending of its own: it watches the membership, works out which
/// mode the node should be in with [`Mode::choose`], and starts or stops the
/// service for that mode.
///
/// Five type parameters, and three of them are the modes' own bundles rather
/// than the services themselves — the orchestrator derives the service types
/// from them, so a node names its collaborators once per mode and nowhere else.
pub struct BlendService<Core, Edge, Broadcast, SdpService, RuntimeServiceId>
where
    Core: CoreComponents<RuntimeServiceId>,
    Core::Backend:
        CoreBlendBackend<Core::NodeId, ChaCha20Rng, Core::ProofsVerifier, RuntimeServiceId>,
    Core::Dispatcher: PayloadDispatcherTrait<RuntimeServiceId>,
    Edge: EdgeComponents<
            RuntimeServiceId,
            NodeId: Clone,
            Dispatcher: PayloadDispatcherTrait<RuntimeServiceId>,
        >,
    Edge::Backend: EdgeBlendBackend<Edge::NodeId, RuntimeServiceId>,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    #[expect(clippy::type_complexity, reason = "Marker field.")]
    _phantom: PhantomData<fn() -> (Core, Edge, Broadcast, SdpService)>,
}

impl<Core, Edge, Broadcast, SdpService, RuntimeServiceId> ServiceData
    for BlendService<Core, Edge, Broadcast, SdpService, RuntimeServiceId>
where
    Core: CoreComponents<RuntimeServiceId>,
    Core::Backend:
        CoreBlendBackend<Core::NodeId, ChaCha20Rng, Core::ProofsVerifier, RuntimeServiceId>,
    Core::Dispatcher: PayloadDispatcherTrait<RuntimeServiceId>,
    Edge: EdgeComponents<
            RuntimeServiceId,
            NodeId: Clone,
            Dispatcher: PayloadDispatcherTrait<RuntimeServiceId>,
        >,
    Edge::Backend: EdgeBlendBackend<Edge::NodeId, RuntimeServiceId>,
{
    type Settings = Settings<
        BackendSettingsOf<Core, RuntimeServiceId>,
        EdgeBackendSettingsOf<Edge, RuntimeServiceId>,
        NetworkSettingsOf<Core, RuntimeServiceId>,
    >;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ProxyServiceMessage<ServiceMessage<Core::NodeId>>;
}

#[expect(
    clippy::too_many_lines,
    reason = "One linear bootstrap, then the mode loop."
)]
#[async_trait]
impl<Core, Edge, Broadcast, SdpService, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for BlendService<Core, Edge, Broadcast, SdpService, RuntimeServiceId>
where
    Core: CoreComponents<
            RuntimeServiceId,
            NodeId: Clone + Debug + Eq + Hash + Send + Sync + node_id::TryFrom + 'static,
            Backend: CoreBlendBackend<
                Core::NodeId,
                ChaCha20Rng,
                Core::ProofsVerifier,
                RuntimeServiceId,
                Settings: Clone + Send + Sync,
            > + Send
                         + Sync,
            Dispatcher: PayloadDispatcherTrait<RuntimeServiceId, Settings: Clone + Send + Sync>
                            + Send
                            + Sync
                            + 'static,
            ProofsGenerator: CoreLeaderAndPowProofsGenerator<
                PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>,
            > + Send,
            ProofsVerifier: ProofsVerifier + Send + Sync,
            TimeBackend: lb_time_service::backends::TimeBackend + Send,
            ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
            PolInfoProvider: PolInfoProviderTrait<
                RuntimeServiceId,
                Stream: Send + Unpin + 'static,
            > + Send,
            SdpService: ServiceData<Message = SdpMessage> + Send,
            StateStorage: RecoveryBackendTrait<
                RuntimeServiceId,
                State = CoreRecoveryStateOf<Core, RuntimeServiceId>,
            > + Send
                              + Sync,
        > + Send
        + 'static,
    Edge: EdgeComponents<
            RuntimeServiceId,
            NodeId = Core::NodeId,
            Dispatcher = Core::Dispatcher,
            ChainService = Core::ChainService,
            TimeBackend = Core::TimeBackend,
            Backend: EdgeBlendBackend<
                Core::NodeId,
                RuntimeServiceId,
                Settings: Clone + Send + Sync,
            > + Send
                         + Sync,
            ProofsGenerator: LeaderAndPowProofsGenerator + Send,
            PolInfoProvider: PolInfoProviderTrait<
                RuntimeServiceId,
                Stream: Send + Unpin + 'static,
            > + Send,
        > + Send
        + 'static,
    Broadcast: BroadcastComponents<
            RuntimeServiceId,
            NodeId = Core::NodeId,
            Dispatcher = Core::Dispatcher,
            ChainService = Core::ChainService,
            TimeBackend = Core::TimeBackend,
        > + Send
        + 'static,
    Core::Backend:
        CoreBlendBackend<Core::NodeId, ChaCha20Rng, Core::ProofsVerifier, RuntimeServiceId>,
    Core::Dispatcher: PayloadDispatcherTrait<RuntimeServiceId>,
    Edge::Backend: EdgeBlendBackend<Core::NodeId, RuntimeServiceId>,
    SdpService: ServiceData<Message = SdpMessage> + Send,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<CoreBlendService<Core, RuntimeServiceId>>
        + AsServiceId<EdgeBlendService<Edge, RuntimeServiceId>>
        + AsServiceId<BroadcastBlendService<Broadcast, RuntimeServiceId>>
        + AsServiceId<Core::ChainService>
        + AsServiceId<TimeService<Core::TimeBackend, RuntimeServiceId>>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>
        + AsServiceId<
            NetworkService<NetworkBackendOfComponents<Core, RuntimeServiceId>, RuntimeServiceId>,
        > + AsServiceId<MempoolOfComponents<Core, RuntimeServiceId>>
        + AsServiceId<ChainNetworkOfComponents<Core, RuntimeServiceId>>
        + AsServiceId<SdpService>
        + Debug
        + Display
        + Clone
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
        let minimal_network_size = settings.common.minimum_network_size;

        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_mins(1)),
            PreloadKmsService<_>,
            SdpService,
            Core::ChainService
        )
        .await?;

        let sdp_service_api =
            SdpServiceApi::<SdpService>::from_overwatch_handle(overwatch_handle).await;

        let kms = KmsServiceApi::<PreloadKmsService<_>, RuntimeServiceId>::new(
            overwatch_handle.relay::<PreloadKmsService<_>>().await?,
        );

        let PublicKeyEncoding::Zk(zk_public_key) = kms
            .public_key(settings.core.zk.secret_key_kms_id.clone())
            .await
            .expect("ZK public key for provided ID should be stored in KMS.")
        else {
            panic!("Key with specified ID is not a ZK key.");
        };

        let PublicKeyEncoding::Ed25519(non_ephemeral_signing_key_public) = kms
            .public_key(settings.common.non_ephemeral_signing_key_id)
            .await
            .expect("KMS does not have key with the specified ID.")
        else {
            panic!("Non-ephemeral signing key must be an Ed25519 key");
        };

        // Wait until the chain becomes Online mode before subscribing to memberships.
        // Chain service provides the correct epoch state only after the chain becomes
        // Online.
        let chain_api = CryptarchiaServiceApi::<Core::ChainService, RuntimeServiceId>::new(
            overwatch_handle.relay::<Core::ChainService>().await?,
        );
        info!(target: LOG_TARGET, "Waiting for chain to become Online mode");
        chain_api
            .wait_until_chain_becomes_online()
            .await
            .expect("Waiting for chain to be online should succeed");
        info!(target: LOG_TARGET, "Chain is now Online.");

        let membership_stream = membership::chain::subscribe::<
            Core::ChainService,
            Core::NodeId,
            Core::TimeBackend,
            RuntimeServiceId,
        >(
            overwatch_handle,
            non_ephemeral_signing_key_public,
            // We don't need to generate secret zk info in the proxy service, so we ignore the
            // secret key at this level.
            None,
            "blend_orchestrator_service",
        )
        .await
        // We take only the membership info from the epoch stream since the proxy service does not
        // need anything else.
        .map(
            |BlendEpochState {
                 membership_info, ..
             }| membership_info,
        );

        let (MembershipInfo { membership, .. }, mut remaining_membership_stream) =
            UninitializedEpochEventStream::new(
                membership_stream,
                settings.common.time.epoch_transition_period,
            )
            .await_first_ready()
            .await
            .expect("The current epoch state must be ready");

        info!(
            target: LOG_TARGET,
            members = membership.size(),
            "current membership is ready",
        );

        // The orchestrator no longer builds a mode: it starts a service. What
        // broadcast mode needs — a dispatcher, a node id, network settings —
        // is now that service's business.
        let mut instance = Instance::<
            CoreBlendService<Core, RuntimeServiceId>,
            EdgeBlendService<Edge, RuntimeServiceId>,
            BroadcastBlendService<Broadcast, RuntimeServiceId>,
            RuntimeServiceId,
        >::new(
            Mode::choose(&membership, minimal_network_size),
            overwatch_handle,
        )
        .await?;

        status_updater.notify_ready();
        info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        loop {
            tokio::select! {
                Some(epoch_event) = remaining_membership_stream.next() => {
                    debug!(target: LOG_TARGET, ?epoch_event, "received epoch event");
                    instance = instance
                        .handle_epoch_event(
                            epoch_event,
                            overwatch_handle,
                            minimal_network_size,
                        )
                        .await?;
                },
                Some(message) = inbound_relay.next() => {
                    match message {
                        ProxyServiceMessage::JoinAsCore { locator, service_note_id, reply } => {
                            reply.send(
                                submit_blend_sdp_declaration(
                                    &sdp_service_api,
                                    locator,
                                    service_note_id,
                                    non_ephemeral_signing_key_public,
                                    zk_public_key,
                                )
                                .await
                            ).unwrap_or_else(|e| {
                                debug!(target: LOG_TARGET, "Failed to send JoinAsCore reply: {e:?}");
                            });
                        },
                        ProxyServiceMessage::Inner(inner_message) => {
                            if let Err(e) = instance.handle_inbound_message(inner_message).await {
                                error!(target: LOG_TARGET, "Failed to handle inbound message: {e:?}");
                            }
                        },
                    }
                },
            }
        }
    }
}

async fn submit_blend_sdp_declaration<SdpService>(
    sdp_service_api: &SdpServiceApi<SdpService>,
    locator: Locator,
    service_note_id: NoteId,
    non_ephemeral_signing_key_public: Ed25519PublicKey,
    zk_id: ZkPublicKey,
) -> Result<DeclarationId, lb_sdp_service::api::Error>
where
    SdpService: ServiceData<Message = SdpMessage>,
{
    tracing::info!(
        target: LOG_TARGET,
        "Submitting Blend service declaration to SDP with locator {locator:?} and service note id {service_note_id:?}",
    );
    let sdp_declaration = DeclarationMessage {
        locators: [locator].into(),
        service_note_id,
        provider_id: ProviderId(non_ephemeral_signing_key_public),
        service_type: ServiceType::BlendNetwork,
        zk_id,
    };
    sdp_service_api.post_declaration(sdp_declaration).await
}
