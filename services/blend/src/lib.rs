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
    overwatch::{RecoveryOperator, recovery::operators::RecoveryBackend as RecoveryBackendTrait},
    wait_until_services_are_ready,
};
use lb_time_service::TimeService;
use lb_utils::blake_rng::BlakeRng;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData},
};
use tracing::{debug, error, info};

use crate::{
    core::{
        CoreServiceState,
        backends::BlendBackend as CoreBlendBackend,
        dispatcher::PayloadDispatcher as PayloadDispatcherTrait,
        kms::PreloadKMSBackendCorePoQGenerator,
        service_components::{BackendSettingsOf, Components},
        state::ServiceState as CoreWorkingState,
    },
    edge::{
        backends::BlendBackend as EdgeBlendBackend,
        service_components::{Components, EdgeBackendSettingsOf},
    },
    epoch_info::PolInfoProvider as PolInfoProviderTrait,
    kms::PreloadKmsService,
    membership::{
        MembershipInfo,
        chain::BlendEpochState,
        node_id::{self, TryFrom as _},
    },
    message::{ProxyServiceMessage, ServiceMessage},
    modes::{BroadcastMode, Mode, run_broadcast_mode},
    service_components::NetworkSettingsOf,
    settings::Settings,
};

pub mod api;
pub mod core;
pub mod edge;
pub mod epoch;
pub mod epoch_info;
pub mod membership;
pub mod message;
pub(crate) mod metrics;
pub mod settings;

mod kms;
mod modes;
mod pending;
mod service_components;
pub use self::service_components::ServiceComponents;

#[cfg(test)]
mod test_utils;

const LOG_TARGET: &str = blend::service::ROOT;

/// The Blend service.
pub struct BlendService<C, RuntimeServiceId>
where
    C: Components<
            RuntimeServiceId,
            Backend: EdgeBlendBackend<
                <C as Components<RuntimeServiceId>>::NodeId,
                RuntimeServiceId,
            >,
        > + Components<RuntimeServiceId, StateStorage: RecoveryBackendTrait<RuntimeServiceId>>
        + Components<
            RuntimeServiceId,
            NodeId: Clone + Eq + Hash,
            Dispatcher: PayloadDispatcherTrait<RuntimeServiceId>,
            Backend: CoreBlendBackend<
                <C as Components<RuntimeServiceId>>::NodeId,
                BlakeRng,
                <C as Components<RuntimeServiceId>>::ProofsVerifier,
                RuntimeServiceId,
            >,
        >,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    last_saved_state: Option<CoreWorkingState<<C as Components<RuntimeServiceId>>::Settings>>,
    _phantom: PhantomData<fn() -> C>,
}

impl<C, RuntimeServiceId> ServiceData for BlendService<C, RuntimeServiceId>
where
    C: Components<RuntimeServiceId, StateStorage: RecoveryBackendTrait<RuntimeServiceId>>
        + Components<
            RuntimeServiceId,
            NodeId: Clone + Eq + Hash,
            Dispatcher: PayloadDispatcherTrait<RuntimeServiceId>,
            Backend: CoreBlendBackend<
                <C as Components<RuntimeServiceId>>::NodeId,
                BlakeRng,
                <C as Components<RuntimeServiceId>>::ProofsVerifier,
                RuntimeServiceId,
            >,
        > + Components<
            RuntimeServiceId,
            Backend: EdgeBlendBackend<
                <C as Components<RuntimeServiceId>>::NodeId,
                RuntimeServiceId,
            >,
        >,
{
    type Settings = <C as Components<RuntimeServiceId>>::Settings;
    // One recovery key for the whole service, which is what gives an edge node
    // the persistence it never had: the same transaction submitted to a core
    // node survived a restart, while an edge node's was lost.
    type State = CoreServiceState<<C as Components<RuntimeServiceId>>::Settings>;
    type StateOperator = RecoveryOperator<C::StateStorage>;
    type Message = ProxyServiceMessage<ServiceMessage<C::NodeId>>;
}

#[expect(
    clippy::too_many_lines,
    reason = "One linear bootstrap followed by the mode loop; splitting hides the order."
)]
#[async_trait]
impl<C, RuntimeServiceId> ServiceCore<RuntimeServiceId> for BlendService<C, RuntimeServiceId>
where
    C: Components<RuntimeServiceId, StateStorage: RecoveryBackendTrait<RuntimeServiceId>>
        + Components<
            RuntimeServiceId,
            NodeId: Clone + Debug + Eq + Hash + Send + Sync + node_id::TryFrom + 'static,
            Dispatcher: PayloadDispatcherTrait<RuntimeServiceId> + Clone + Send + Sync + 'static,
            TimeBackend: lb_time_service::backends::TimeBackend + Send,
            ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
            PolInfoProvider: PolInfoProviderTrait<
                RuntimeServiceId,
                Stream: Send + Unpin + 'static,
            > + Send,
            // Pinned to the concrete shape so the bootstrap below can read
            // its fields; `Components` keeps it abstract so the recovery state
            // can be tied to it without this module's layout leaking there.
            Settings = Settings<
                BackendSettingsOf<C, RuntimeServiceId>,
                EdgeBackendSettingsOf<C, RuntimeServiceId>,
                NetworkSettingsOf<C, RuntimeServiceId>,
            >,
            SdpService: ServiceData<Message = SdpMessage> + Send,
            StateStorage: RecoveryBackendTrait<
                RuntimeServiceId,
                State = CoreServiceState<<C as Components<RuntimeServiceId>>::Settings>,
            > + Send
                              + Sync,
            // Pinned, not free: core mode seeds its release scheduler from
            // entropy and draws core `PoQ` proofs from the KMS adapter its
            // bootstrap builds, so no other choice would fit.
            Rng = BlakeRng,
            CorePoQGenerator = PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>,
            ProofsVerifier: ProofsVerifier + Send + Sync + 'static,
        > + Components<RuntimeServiceId>
        + Send
        + 'static,
    <C as Components<RuntimeServiceId>>::Backend: CoreBlendBackend<
            C::NodeId,
            BlakeRng,
            <C as Components<RuntimeServiceId>>::ProofsVerifier,
            RuntimeServiceId,
            Settings: Clone + Send + Sync,
        > + Send
        + Sync
        + 'static,
    <C as Components<RuntimeServiceId>>::ProofsGenerator: CoreLeaderAndPowProofsGenerator<PreloadKMSBackendCorePoQGenerator<RuntimeServiceId>>
        + Send
        + 'static,
    <C as Components<RuntimeServiceId>>::Backend:
        EdgeBlendBackend<C::NodeId, RuntimeServiceId, Settings: Clone + Send + Sync> + Send + Sync,
    <C as Components<RuntimeServiceId>>::ProofsGenerator: LeaderAndPowProofsGenerator + Send,
    NetworkSettingsOf<C, RuntimeServiceId>: Clone + Send + Sync,
    EdgeBackendSettingsOf<C, RuntimeServiceId>: Clone + Send + Sync,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<C::ChainService>
        + AsServiceId<C::SdpService>
        + AsServiceId<TimeService<C::TimeBackend, RuntimeServiceId>>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>
        + AsServiceId<
            NetworkService<
                <C::Dispatcher as PayloadDispatcherTrait<RuntimeServiceId>>::Backend,
                RuntimeServiceId,
            >,
        > + AsServiceId<<C::Dispatcher as PayloadDispatcherTrait<RuntimeServiceId>>::MempoolService>
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
        recovery_initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let state_updater = service_resources_handle.state_updater.clone();
        Ok(Self {
            service_resources_handle,
            // An inconsistent persisted state is discarded rather than fatal:
            // `run` falls back to a fresh one, which avoids a crash loop.
            last_saved_state: recovery_initial_state.service_state.and_then(|s| {
                match s.try_into_state_with_state_updater(state_updater) {
                    Ok(state) => Some(state),
                    Err(error) => {
                        error!(
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

    async fn run(mut self) -> Result<(), DynError> {
        let Self {
            service_resources_handle:
                OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                    ref mut inbound_relay,
                    ref overwatch_handle,
                    ref settings_handle,
                    ref status_updater,
                    state_updater,
                },
            mut last_saved_state,
            ..
        } = self;

        let settings = settings_handle.notifier().get_updated_settings();
        let minimal_network_size = settings.common.minimum_network_size.get() as usize;

        // One readiness wait for the whole service. There used to be four —
        // the proxy's, the core service's, the edge service's and one inside
        // `BroadcastMode::new` — and the last of those ran *inside a mode
        // transition*, so a node could sit for up to a minute serving no Blend
        // message at all.
        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_mins(1)),
            PreloadKmsService<_>,
            C::SdpService,
            C::ChainService,
            NetworkService<_, _>,
            TimeService<_, _>
        )
        .await?;

        // One dispatcher for the whole service. It has no mode or epoch
        // dependency, so building it per mode only meant building it again on
        // every transition.
        let payload_dispatcher = <C::Dispatcher as PayloadDispatcherTrait<RuntimeServiceId>>::new(
            overwatch_handle
                .relay::<NetworkService<_, _>>()
                .await
                .expect("Relay with network service should be available."),
            overwatch_handle
                .relay::<<C::Dispatcher as PayloadDispatcherTrait<RuntimeServiceId>>::MempoolService>()
                .await
                .expect("Relay with mempool service should be available."),
            settings.core.network.clone(),
        );

        let sdp_service_api =
            SdpServiceApi::<C::SdpService>::from_overwatch_handle(overwatch_handle).await;

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
            .public_key(settings.common.non_ephemeral_signing_key_id.clone())
            .await
            .expect("KMS does not have key with the specified ID.")
        else {
            panic!("Non-ephemeral signing key must be an Ed25519 key");
        };
        let local_node_id =
            C::NodeId::try_from_provider_id(non_ephemeral_signing_key_public.as_bytes())
                .expect("non-ephemeral signing public key should decode into a valid node id");

        // The chain reports a usable epoch state only once it is Online.
        let chain_api = CryptarchiaServiceApi::<C::ChainService, RuntimeServiceId>::new(
            overwatch_handle.relay::<C::ChainService>().await?,
        );
        info!(target: LOG_TARGET, "Waiting for chain to become Online mode");
        chain_api
            .wait_until_chain_becomes_online()
            .await
            .expect("Waiting for chain to be online should succeed");
        info!(target: LOG_TARGET, "Chain is now Online.");

        status_updater.notify_ready();
        info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        // `JoinAsCore` is answered here rather than by whichever mode happens to
        // be running: it is a declaration about the node, not about blending,
        // and only this level holds the keys it needs. Everything else passes
        // through to the mode, so a mode never sees a message it has no answer
        // for — no `unreachable!()` arm in two modes out of three.
        let mut inbound = Box::pin(inbound_relay.filter_map(|message| {
            let sdp_service_api = &sdp_service_api;
            async move {
                match message {
                    ProxyServiceMessage::Inner(inner) => Some(inner),
                    ProxyServiceMessage::JoinAsCore {
                        locator,
                        service_note_id,
                        reply,
                    } => {
                        let declaration = submit_blend_sdp_declaration(
                            sdp_service_api,
                            locator,
                            service_note_id,
                            non_ephemeral_signing_key_public,
                            zk_public_key,
                        )
                        .await;
                        if let Err(e) = reply.send(declaration) {
                            debug!(target: LOG_TARGET, "Failed to send JoinAsCore reply: {e:?}");
                        }
                        None
                    }
                }
            }
        }));

        loop {
            // Read the membership afresh each time a mode ends, rather than
            // holding a subscription across the whole run. A supervisor-held
            // stream would sit unpolled for a mode's entire lifetime, and a
            // lagging consumer of a broadcast stream drops items; re-reading is
            // always correct and mode changes are rare. It is also what removes
            // the duplicate steady-state subscription the proxy used to hold.
            let membership_stream =
                membership::chain::subscribe::<
                    C::ChainService,
                    C::NodeId,
                    C::TimeBackend,
                    RuntimeServiceId,
                >(overwatch_handle, non_ephemeral_signing_key_public, None)
                .await
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

            let mode = Mode::choose(&membership, minimal_network_size);
            info!(target: LOG_TARGET, members = membership.size(), "Entering {mode:?} mode");

            match mode {
                Mode::Core => {
                    // Core mode takes a subscription of its own — it needs the
                    // full epoch state, not just the membership. Dropping this
                    // one first is what keeps the count at one: holding an
                    // unread stream would leave the chain service driving a
                    // per-slot query for a subscriber nobody polls, which is
                    // the duplicate the proxy used to carry.
                    drop(remaining_membership_stream);
                    core::run_core_mode::<C, RuntimeServiceId>(
                        &mut inbound,
                        overwatch_handle,
                        payload_dispatcher.clone(),
                        settings.clone().into(),
                        last_saved_state.take(),
                        state_updater.clone(),
                        || {},
                    )
                    .await?;
                }
                Mode::Edge => {
                    // Same as core: the edge subscribes for itself.
                    drop(remaining_membership_stream);
                    edge::run_edge_mode::<C, RuntimeServiceId>(
                        &mut inbound,
                        overwatch_handle.clone(),
                        settings.clone().into(),
                        || {},
                    )
                    .await?;
                }
                Mode::Broadcast => {
                    let broadcast =
                        BroadcastMode::<C::Dispatcher, C::NodeId, RuntimeServiceId>::new(
                            payload_dispatcher.clone(),
                            local_node_id.clone(),
                        );
                    if run_broadcast_mode(
                        &broadcast,
                        &mut inbound,
                        &mut remaining_membership_stream,
                        minimal_network_size,
                    )
                    .await
                    .is_none()
                    {
                        return Ok(());
                    }
                }
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
