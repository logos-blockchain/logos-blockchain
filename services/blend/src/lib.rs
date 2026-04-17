use std::{
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    time::Duration,
};

use async_trait::async_trait;
use futures::StreamExt as _;
pub use lb_blend::message::{crypto::proofs::RealProofsVerifier, encap::ProofsVerifier};
use lb_blend::scheduling::session::UninitializedSessionEventStream;
use lb_key_management_system_service::{api::KmsServiceApi, keys::PublicKeyEncoding};
use lb_network_service::NetworkService;
use lb_services_utils::wait_until_services_are_ready;
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
    membership::{Adapter as _, MembershipInfo},
    settings::{FIRST_STREAM_ITEM_READY_TIMEOUT, Settings},
};

pub mod core;
pub mod edge;
pub mod epoch_info;
pub mod membership;
pub mod message;
pub(crate) mod metrics;
pub mod session;
pub mod settings;

mod instance;
mod kms;
mod modes;
mod service_components;
pub use self::service_components::ServiceComponents;

#[cfg(test)]
mod test_utils;

const LOG_TARGET: &str = "blend::service";

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
    CoreService: ServiceData<Message: MessageComponents<Payload: Into<Vec<u8>>> + Send + Sync + 'static>
        + CoreServiceComponents<
            RuntimeServiceId,
            NetworkAdapter: NetworkAdapterTrait<
                RuntimeServiceId,
                BroadcastSettings = BroadcastSettings<CoreService>,
            > + Send
                                + Sync
                                + 'static,
            NodeId: Clone + Debug + Hash + Eq + Send + Sync + 'static,
            BackendSettings: Clone + Send + Sync,
        > + Send
        + 'static,
    EdgeService: ServiceData<Message = CoreService::Message>
        // We tie the core and edge proofs generator to be the same type, to avoid mistakes in the
        // node configuration where the two services use different verification logic
        + EdgeServiceComponents<BackendSettings: Clone + Send + Sync>
        + Send
        + 'static,
    EdgeService::MembershipAdapter:
        membership::Adapter<NodeId = CoreService::NodeId, Error: Send + Sync + 'static> + Send,
    membership::ServiceMessage<EdgeService::MembershipAdapter>: Send + Sync + 'static,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<CoreService>
        + AsServiceId<EdgeService>
        + AsServiceId<MembershipService<EdgeService>>
        + AsServiceId<PreloadKmsService<RuntimeServiceId>>
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
            MembershipService<EdgeService>,
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

        let membership_stream = <MembershipAdapter<EdgeService> as membership::Adapter>::new(
            overwatch_handle
                .relay::<MembershipService<EdgeService>>()
                .await?,
            non_ephemeral_signing_key_public,
            // We don't need to generate secret zk info in the proxy service, so we ignore the
            // secret key at this level.
            None,
        )
        .subscribe()
        .await?;

        let (MembershipInfo { membership, .. }, mut remaining_session_stream) =
            UninitializedSessionEventStream::new(
                membership_stream,
                FIRST_STREAM_ITEM_READY_TIMEOUT,
                settings.common.time.session_transition_period(),
            )
            .await_first_ready()
            .await
            .expect("The current session must be ready");

        info!(
            target: LOG_TARGET,
            members = membership.size(),
            "current membership is ready",
        );

        let mut instance = Instance::<CoreService, EdgeService, RuntimeServiceId>::new(
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
                Some(session_event) = remaining_session_stream.next() => {
                    debug!(target: LOG_TARGET, ?session_event, "received session event");
                    instance = instance.handle_session_event(session_event, overwatch_handle, minimal_network_size).await?;
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

type BroadcastSettings<CoreService> =
    <<CoreService as ServiceData>::Message as MessageComponents>::BroadcastSettings;

type MembershipAdapter<EdgeService> = <EdgeService as edge::ServiceComponents>::MembershipAdapter;

type MembershipService<EdgeService> =
    <MembershipAdapter<EdgeService> as membership::Adapter>::Service;
