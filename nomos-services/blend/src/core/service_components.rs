use nomos_utils::blake_rng::BlakeRng;

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
    Backend: BlendBackend<NodeId, BlakeRng, ProofsVerifier, RuntimeServiceId>,
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

pub trait MessageComponents {
    type Payload;
    type BroadcastSettings;

    fn into_components(self) -> (Self::Payload, Self::BroadcastSettings);
}

impl<BroadcastSettings> MessageComponents for ServiceMessage<BroadcastSettings> {
    type Payload = Vec<u8>;
    type BroadcastSettings = BroadcastSettings;

    fn into_components(self) -> (Self::Payload, Self::BroadcastSettings) {
        let Self::Blend(network_message) = self;
        (network_message.message, network_message.broadcast_settings)
    }
}
