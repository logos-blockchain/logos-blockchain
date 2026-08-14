use lb_utils::blake_rng::BlakeRng;
use tokio::sync::oneshot;

use crate::{
    core::{BlendService, backends::BlendBackend, dispatcher::PayloadDispatcher},
    message::{BlendPayload, NetworkInfo, ServiceMessage},
};

/// Helper trait to help the Blend proxy service rely on the concrete types of
/// the core Blend service without having to specify all the generics the core
/// service expects.
pub trait ServiceComponents<RuntimeServiceId> {
    type PayloadDispatcher: PayloadDispatcher<RuntimeServiceId>;
    type BackendSettings;
    type NodeId;
    type Rng;
    type ProofsGenerator;
}

impl<
    Backend,
    NodeId,
    Network,
    SdpAdapter,
    ProofsGenerator,
    ProofsVerifier,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    StateStorage,
    RuntimeServiceId,
> ServiceComponents<RuntimeServiceId>
    for BlendService<
        Backend,
        NodeId,
        Network,
        SdpAdapter,
        ProofsGenerator,
        ProofsVerifier,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        StateStorage,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId>,
    Network: PayloadDispatcher<RuntimeServiceId>,
    StateStorage: lb_services_utils::overwatch::recovery::RecoveryBackend<
            RuntimeServiceId,
            State = crate::core::state::RecoveryServiceState<Backend::Settings, Network::Settings>,
        > + Send
        + Sync,
{
    type PayloadDispatcher = Network;
    type BackendSettings = Backend::Settings;
    type NodeId = NodeId;
    type Rng = BlakeRng;
    type ProofsGenerator = ProofsGenerator;
}

pub type NetworkBackendOfService<Service, RuntimeServiceId> =
    <<Service as ServiceComponents<RuntimeServiceId>>::PayloadDispatcher as PayloadDispatcher<
        RuntimeServiceId,
    >>::Backend;
pub type BlendBackendSettingsOfService<Service, RuntimeServiceId> =
    <Service as ServiceComponents<RuntimeServiceId>>::BackendSettings;

/// The settings the core service's network adapter needs in order to
/// republish a message — deployment configuration, never carried in a payload.
/// The mempool service the core service's dispatcher hands transactions to.
pub type MempoolOfService<Service, RuntimeServiceId> = <<Service as ServiceComponents<
    RuntimeServiceId,
>>::PayloadDispatcher as PayloadDispatcher<RuntimeServiceId>>::MempoolService;

pub type PayloadDispatcherSettingsOfService<Service, RuntimeServiceId> =
    <<Service as ServiceComponents<RuntimeServiceId>>::PayloadDispatcher as PayloadDispatcher<
        RuntimeServiceId,
    >>::Settings;

pub trait MessageComponents<NodeId> {
    type Payload;

    fn into_payload(self) -> Self::Payload;

    /// Try to extract a network info request from the message.
    /// Returns `Ok(sender)` if the message is a `NetworkInfo` request,
    /// or `Err(self)` if it is not.
    fn try_into_network_info_request(
        self,
    ) -> Result<oneshot::Sender<Option<NetworkInfo<NodeId>>>, Self>
    where
        Self: Sized;
}

impl<NodeId> MessageComponents<NodeId> for ServiceMessage<NodeId> {
    type Payload = BlendPayload;

    fn into_payload(self) -> Self::Payload {
        match self {
            Self::Blend(message) => message,
            Self::GetNetworkInfo { .. } => {
                panic!("NetworkInfo messages should be handled before calling into_payload")
            }
        }
    }

    fn try_into_network_info_request(
        self,
    ) -> Result<oneshot::Sender<Option<NetworkInfo<NodeId>>>, Self> {
        match self {
            Self::GetNetworkInfo { reply } => Ok(reply),
            other @ Self::Blend(_) => Err(other),
        }
    }
}
