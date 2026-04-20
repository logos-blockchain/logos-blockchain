use lb_utils::blake_rng::BlakeRng;
use tokio::sync::oneshot;

use crate::{
    core::{BlendService, backends::BlendBackend},
    message::ServiceMessage,
};

/// Helper trait to help the Blend proxy service rely on the concrete types of
/// the core Blend service without having to specify all the generics the core
/// service expects.
pub trait ServiceComponents<RuntimeServiceId> {
    type NetworkAdapter;
    type BackendSettings;
    type NodeId;
    type Rng;
    type ProofsGenerator;
}

impl<
    Backend,
    NodeId,
    Network,
    MembershipAdapter,
    SdpAdapter,
    ProofsGenerator,
    ProofsVerifier,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> ServiceComponents<RuntimeServiceId>
    for BlendService<
        Backend,
        NodeId,
        Network,
        MembershipAdapter,
        SdpAdapter,
        ProofsGenerator,
        ProofsVerifier,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, BlakeRng, RuntimeServiceId>,
    Network: crate::core::network::NetworkAdapter<RuntimeServiceId>,
{
    type NetworkAdapter = Network;
    type BackendSettings = Backend::Settings;
    type NodeId = NodeId;
    type Rng = BlakeRng;
    type ProofsGenerator = ProofsGenerator;
}

pub type NetworkBackendOfService<Service, RuntimeServiceId> = <<Service as ServiceComponents<
    RuntimeServiceId,
>>::NetworkAdapter as crate::core::network::NetworkAdapter<RuntimeServiceId>>::Backend;
pub type BlendBackendSettingsOfService<Service, RuntimeServiceId> =
    <Service as ServiceComponents<RuntimeServiceId>>::BackendSettings;

use crate::message::NetworkInfo;

pub trait MessageComponents<NodeId> {
    type Payload;
    type BroadcastSettings;

    fn into_components(self) -> (Self::Payload, Self::BroadcastSettings);

    /// Try to extract a network info request from the message.
    /// Returns `Ok(sender)` if the message is a `NetworkInfo` request,
    /// or `Err(self)` if it is not.
    fn try_into_network_info_request(
        self,
    ) -> Result<oneshot::Sender<Option<NetworkInfo<NodeId>>>, Self>
    where
        Self: Sized;
}

impl<BroadcastSettings, NodeId> MessageComponents<NodeId>
    for ServiceMessage<BroadcastSettings, NodeId>
{
    type Payload = Vec<u8>;
    type BroadcastSettings = BroadcastSettings;

    fn into_components(self) -> (Self::Payload, Self::BroadcastSettings) {
        match self {
            Self::Blend(network_message) => {
                (network_message.message, network_message.broadcast_settings)
            }
            Self::GetNetworkInfo { .. } => {
                panic!("NetworkInfo messages should be handled before calling into_components")
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
