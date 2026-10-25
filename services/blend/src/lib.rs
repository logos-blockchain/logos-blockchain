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
    epoch::{EpochEvent, EpochEventStream},
    stream::UninitializedFirstReadyStream,
};
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::{
    mantle::NoteId,
    sdp::{DeclarationMessage, Locator, ProviderId, ServiceType},
};
use lb_key_management_system_service::{
    api::KmsServiceApi,
    keys::{Ed25519PublicKey, PublicKeyEncoding, ZkPublicKey},
};
use lb_log_targets::blend;
use lb_network_service::NetworkService;
use lb_sdp_service::{SdpMessage, SdpServiceApi, SdpSubmission};
use lb_services_utils::wait_until_services_are_ready;
use lb_time_service::TimeService;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        resources::ServiceResourcesHandle,
        state::{NoOperator, NoState},
    },
};
use tracing::{debug, error, info};

use crate::{
    broadcast::service_components::ServiceComponents as BroadcastServiceComponents,
    core::{
        dispatcher::PayloadDispatcher as PayloadDispatcherTrait,
        service_components::{
            ChainNetworkOfService, MempoolOfService, NetworkBackendOfService,
            ServiceComponents as CoreServiceComponents,
        },
    },
    edge::service_components::ServiceComponents as EdgeServiceComponents,
    kms::PreloadKmsService,
    membership::node_id,
    message::{ProxyServiceMessage, ServiceMessage},
    mode::ModeMembership,
    orchestrator::Instance,
    settings::OrchestratorSettings,
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

pub struct BlendService<CoreService, EdgeService, BroadcastService, SdpService, RuntimeServiceId>
where
    CoreService: ServiceData + CoreServiceComponents<RuntimeServiceId>,
    EdgeService: EdgeServiceComponents,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    _phantom: PhantomData<(CoreService, EdgeService, BroadcastService, SdpService)>,
}

impl<CoreService, EdgeService, BroadcastService, SdpService, RuntimeServiceId> ServiceData
    for BlendService<CoreService, EdgeService, BroadcastService, SdpService, RuntimeServiceId>
where
    CoreService: ServiceData + CoreServiceComponents<RuntimeServiceId>,
    EdgeService: EdgeServiceComponents,
{
    type Settings = OrchestratorSettings;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ProxyServiceMessage<CoreService::Message>;
}

#[expect(
    clippy::too_many_lines,
    reason = "One linear bootstrap, then the mode loop."
)]
#[async_trait]
impl<CoreService, EdgeService, BroadcastService, SdpService, RuntimeServiceId>
    ServiceCore<RuntimeServiceId>
    for BlendService<CoreService, EdgeService, BroadcastService, SdpService, RuntimeServiceId>
where
    CoreService: ServiceData<Message = ServiceMessage<CoreService::NodeId>>
        + CoreServiceComponents<
            RuntimeServiceId,
            PayloadDispatcher: PayloadDispatcherTrait<RuntimeServiceId> + Send + Sync + 'static,
            NodeId: Clone + Debug + Hash + Eq + Send + Sync + node_id::TryFrom + 'static,
        > + Send
        + 'static,
    // The core and edge services are tied to the same message type, so a node
    // cannot be wired up with two modes that answer different requests.
    EdgeService: ServiceData<Message = CoreService::Message>
        + EdgeServiceComponents<
            ChainService: CryptarchiaServiceData<Tx: Send>,
            TimeBackend: lb_time_service::backends::TimeBackend + Send,
        > + Send
        + 'static,
    BroadcastService: ServiceData<Message = CoreService::Message>
        + BroadcastServiceComponents<
            ChainService = <EdgeService as EdgeServiceComponents>::ChainService,
            TimeBackend = <EdgeService as EdgeServiceComponents>::TimeBackend,
        > + Send
        + 'static,
    SdpService: ServiceData<Message = SdpMessage> + Send,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<CoreService>
        + AsServiceId<EdgeService>
        + AsServiceId<BroadcastService>
        + AsServiceId<<EdgeService as EdgeServiceComponents>::ChainService>
        + AsServiceId<
            TimeService<<EdgeService as EdgeServiceComponents>::TimeBackend, RuntimeServiceId>,
        > + AsServiceId<PreloadKmsService<RuntimeServiceId>>
        + AsServiceId<
            NetworkService<
                NetworkBackendOfService<CoreService, RuntimeServiceId>,
                <EdgeService as EdgeServiceComponents>::TimeBackend,
                RuntimeServiceId,
            >,
        > + AsServiceId<MempoolOfService<CoreService, RuntimeServiceId>>
        + AsServiceId<ChainNetworkOfService<CoreService, RuntimeServiceId>>
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
                ServiceResourcesHandle {
                    ref mut inbound_relay,
                    ref overwatch_handle,
                    ref settings_handle,
                    ref status_updater,
                    ..
                },
            ..
        } = self;

        let OrchestratorSettings {
            non_ephemeral_signing_key_id,
            zk_secret_key_kms_id,
            era_schedule,
        } = settings_handle.notifier().get_updated_settings();

        // Wait for services (except Chain and SDP) to become ready, with timeout
        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_mins(1)),
            PreloadKmsService<_>
        )
        .await?;
        // Wait for Chain, which becomes ready after recovering blocks, and SDP,
        // which becomes ready after Chain, without timeout.
        wait_until_services_are_ready!(
            &overwatch_handle,
            None,
            <EdgeService as EdgeServiceComponents>::ChainService,
            SdpService
        )
        .await?;

        let sdp_service_api =
            SdpServiceApi::<SdpService>::from_overwatch_handle(overwatch_handle).await;

        let kms = KmsServiceApi::<PreloadKmsService<_>, RuntimeServiceId>::new(
            overwatch_handle.relay::<PreloadKmsService<_>>().await?,
        );

        let PublicKeyEncoding::Zk(zk_public_key) = kms
            .public_key(zk_secret_key_kms_id)
            .await
            .expect("ZK public key for provided ID should be stored in KMS.")
        else {
            panic!("Key with specified ID is not a ZK key.");
        };

        let PublicKeyEncoding::Ed25519(non_ephemeral_signing_key_public) = kms
            .public_key(non_ephemeral_signing_key_id)
            .await
            .expect("KMS does not have key with the specified ID.")
        else {
            panic!("Non-ephemeral signing key must be an Ed25519 key");
        };

        // Wait until the chain becomes Online mode before subscribing to memberships.
        // Chain service provides the correct epoch state only after the chain becomes
        // Online.
        let chain_api =
            CryptarchiaServiceApi::<<EdgeService as EdgeServiceComponents>::ChainService>::from_overwatch_handle(
                overwatch_handle,
            )
            .await;
        info!(target: LOG_TARGET, "Waiting for chain to become Online mode");
        chain_api
            .wait_until_chain_becomes_online()
            .await
            .expect("Waiting for chain to be online should succeed");
        info!(target: LOG_TARGET, "Chain is now Online.");

        let membership_with_epoch_number_stream = membership::chain::subscribe::<
            <EdgeService as EdgeServiceComponents>::ChainService,
            CoreService::NodeId,
            <EdgeService as EdgeServiceComponents>::TimeBackend,
            RuntimeServiceId,
        >(
            overwatch_handle,
            non_ephemeral_signing_key_public,
            // We don't need to generate secret zk info in the orchestrator service, so we
            // ignore the secret key at this level.
            None,
            "blend_orchestrator_service",
        )
        .await
        // We take only the epoch and its membership from the epoch stream since the
        // proxy service does not need anything else.
        .map(|(epoch_state, membership_info)| (epoch_state.epoch, membership_info));
        let ((epoch, membership_info), remaining_membership_stream) =
            UninitializedFirstReadyStream::new(membership_with_epoch_number_stream)
                .first()
                .await
                .expect("The current epoch state must be ready");
        let mut remaining_membership_stream = EpochEventStream::new(
            remaining_membership_stream,
            era_schedule
                .at_epoch(epoch)
                .parameters
                .epoch_transition_period,
        );

        info!(
            target: LOG_TARGET,
            members = membership_info.membership.size(),
            "current membership is ready",
        );

        // The mode of an epoch follows from its membership and the minimum
        // network size of its era.
        let mut era_minimum_network_size =
            era_schedule.at_epoch(epoch).parameters.minimum_network_size;
        let mut instance =
            Instance::<CoreService, EdgeService, BroadcastService, RuntimeServiceId>::new(
                ModeMembership::resolve(membership_info, era_minimum_network_size).mode(),
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
                    let epoch_event = match epoch_event {
                        EpochEvent::NewEpoch((epoch, membership_info)) => {
                            let era_settings = era_schedule.at_epoch(epoch).parameters;
                            era_minimum_network_size = era_settings.minimum_network_size;
                            // The transition out of this epoch lasts its era's period. For same-era epoch events, this is a no-op.
                            remaining_membership_stream.set_transition_period(era_settings.epoch_transition_period);
                            EpochEvent::NewEpoch(membership_info)
                        }
                        EpochEvent::TransitionPeriodExpired => EpochEvent::TransitionPeriodExpired,
                    };
                    instance = instance
                        .handle_epoch_event(epoch_event, overwatch_handle, era_minimum_network_size)
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
                        ProxyServiceMessage::GetZkId { reply } => {
                            reply.send(zk_public_key).unwrap_or_else(|e| {
                                debug!(target: LOG_TARGET, "Failed to send GetZkId reply: {e:?}");
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
) -> Result<SdpSubmission, lb_sdp_service::api::Error>
where
    SdpService: ServiceData<Message = SdpMessage>,
{
    tracing::info!(
        target: LOG_TARGET,
        "Submitting Blend service declaration to SDP with locator {locator:?} and service note id {service_note_id:?}",
    );
    let sdp_declaration = DeclarationMessage {
        locators: locator.into(),
        service_note_id,
        provider_id: ProviderId(non_ephemeral_signing_key_public),
        service_type: ServiceType::BlendNetwork,
        zk_id,
    };
    sdp_service_api.post_declaration(sdp_declaration).await
}
