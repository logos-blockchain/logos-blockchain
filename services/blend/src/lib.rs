use std::{
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    time::Duration,
};

use async_trait::async_trait;
use futures::StreamExt as _;
pub use lb_blend::message::{crypto::proofs::RealProofsVerifier, encap::ProofsVerifier};
use lb_blend::scheduling::epoch::UninitializedEpochEventStream;
use lb_chain_service::api::CryptarchiaServiceData;
use lb_key_management_system_service::{api::KmsServiceApi, keys::PublicKeyEncoding};
use lb_log_targets::blend;
use lb_network_service::NetworkService;
use lb_services_utils::wait_until_services_are_ready;
use lb_time_service::TimeService;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use tracing::{debug, error, info};

use crate::{
    core::{
        network::NetworkAdapter as NetworkAdapterTrait,
        service_components::{
            BlendBackendSettingsOfService, MessageComponents, NetworkBackendOfService,
            ServiceComponents as CoreServiceComponents,
        },
    },
    edge::service_components::ServiceComponents as EdgeServiceComponents,
    instance::{Instance, Mode},
    kms::PreloadKmsService,
    membership::{
        MembershipInfo,
        chain::BlendEpochState,
        node_id::{self, TryFrom as _},
    },
    settings::Settings,
};

pub mod core;
pub mod edge;
pub mod epoch;
pub mod epoch_info;
pub mod membership;
pub mod message;
pub(crate) mod metrics;
pub mod settings;

mod instance;
mod kms;
mod modes;
mod service_components;
pub use self::service_components::ServiceComponents;

#[cfg(test)]
mod test_utils;

const LOG_TARGET: &str = blend::service::ROOT;

pub struct BlendService<CoreService, EdgeService, RuntimeServiceId>
where
    CoreService: ServiceData + CoreServiceComponents<RuntimeServiceId>,
    EdgeService: EdgeServiceComponents,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    _phantom: PhantomData<(CoreService, EdgeService)>,
}

impl<CoreService, EdgeService, RuntimeServiceId> ServiceData
    for BlendService<CoreService, EdgeService, RuntimeServiceId>
where
    CoreService: ServiceData + CoreServiceComponents<RuntimeServiceId>,
    EdgeService: EdgeServiceComponents,
{
    type Settings = Settings<
        BlendBackendSettingsOfService<CoreService, RuntimeServiceId>,
        <EdgeService as EdgeServiceComponents>::BackendSettings,
    >;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = CoreService::Message;
}

#[async_trait]
impl<CoreService, EdgeService, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for BlendService<CoreService, EdgeService, RuntimeServiceId>
where
    CoreService: ServiceData<
            Message: MessageComponents<CoreService::NodeId, Payload: Into<Vec<u8>>>
                         + Send
                         + Sync
                         + 'static,
        > + CoreServiceComponents<
            RuntimeServiceId,
            NetworkAdapter: NetworkAdapterTrait<
                RuntimeServiceId,
                BroadcastSettings = BroadcastSettings<CoreService, RuntimeServiceId>,
            > + Send
                                + Sync
                                + 'static,
            NodeId: Clone + Debug + Hash + Eq + Send + Sync + node_id::TryFrom + 'static,
            BackendSettings: Clone + Send + Sync,
        > + Send
        + 'static,
    EdgeService: ServiceData<Message = CoreService::Message>
        // We tie the core and edge proofs generator to be the same type, to avoid mistakes in the
        // node configuration where the two services use different verification logic
        + EdgeServiceComponents<
            BackendSettings: Clone + Send + Sync,
            ChainService: CryptarchiaServiceData<Tx: Send + Sync>,
            TimeBackend: lb_time_service::backends::TimeBackend + Send,
        > + Send
        + 'static,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<CoreService>
        + AsServiceId<EdgeService>
        + AsServiceId<<EdgeService as EdgeServiceComponents>::ChainService>
        + AsServiceId<
            TimeService<<EdgeService as EdgeServiceComponents>::TimeBackend, RuntimeServiceId>,
        > + AsServiceId<PreloadKmsService<RuntimeServiceId>>
        + AsServiceId<
            NetworkService<
                NetworkBackendOfService<CoreService, RuntimeServiceId>,
                RuntimeServiceId,
            >,
        > + Debug
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
        let minimal_network_size = settings.common.minimum_network_size.get() as usize;

        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_mins(1)),
            PreloadKmsService<_>
        )
        .await?;

        let kms = KmsServiceApi::<PreloadKmsService<_>, RuntimeServiceId>::new(
            overwatch_handle.relay::<PreloadKmsService<_>>().await?,
        );

        let PublicKeyEncoding::Ed25519(non_ephemeral_signing_key_public) = kms
            .public_key(settings.common.non_ephemeral_signing_key_id)
            .await
            .expect("KMS does not have key with the specified ID.")
        else {
            panic!("Non-ephemeral signing key must be an Ed25519 key");
        };
        let local_node_id =
            CoreService::NodeId::try_from_provider_id(non_ephemeral_signing_key_public.as_bytes())
                .expect("non-ephemeral signing public key should decode into a valid node id");

        let membership_stream = membership::chain::subscribe::<
            <EdgeService as EdgeServiceComponents>::ChainService,
            CoreService::NodeId,
            <EdgeService as EdgeServiceComponents>::TimeBackend,
            RuntimeServiceId,
        >(
            overwatch_handle,
            non_ephemeral_signing_key_public,
            // We don't need to generate secret zk info in the proxy service, so we ignore the
            // secret key at this level.
            None,
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

        let mut instance = Instance::<CoreService, EdgeService, RuntimeServiceId>::new(
            Mode::choose(&membership, minimal_network_size),
            local_node_id.clone(),
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
                            local_node_id.clone(),
                        )
                        .await?;
                },
                Some(message) = inbound_relay.next() => {
                    if let Err(e) = instance.handle_inbound_message(message).await {
                        error!(target: LOG_TARGET, "Failed to handle inbound message: {e:?}");
                    }
                },
            }
        }
    }
}

type BroadcastSettings<CoreService, RuntimeServiceId> =
    <<CoreService as ServiceData>::Message as MessageComponents<
        <CoreService as CoreServiceComponents<RuntimeServiceId>>::NodeId,
    >>::BroadcastSettings;
