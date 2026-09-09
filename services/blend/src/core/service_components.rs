use rand_chacha::ChaCha20Rng;

use crate::core::{BlendService, backends::BlendBackend, dispatcher::PayloadDispatcher};

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
    Backend: BlendBackend<NodeId, ChaCha20Rng, ProofsVerifier, RuntimeServiceId>,
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
    type Rng = ChaCha20Rng;
    type ProofsGenerator = ProofsGenerator;
}

pub type NetworkBackendOfService<Service, RuntimeServiceId> =
    <<Service as ServiceComponents<RuntimeServiceId>>::PayloadDispatcher as PayloadDispatcher<
        RuntimeServiceId,
    >>::Backend;
pub type BlendBackendSettingsOfService<Service, RuntimeServiceId> =
    <Service as ServiceComponents<RuntimeServiceId>>::BackendSettings;

/// The mempool service the core service's dispatcher hands transactions to.
pub type MempoolOfService<Service, RuntimeServiceId> = <<Service as ServiceComponents<
    RuntimeServiceId,
>>::PayloadDispatcher as PayloadDispatcher<RuntimeServiceId>>::MempoolService;

pub type ChainNetworkOfService<Service, RuntimeServiceId> = <<Service as ServiceComponents<
    RuntimeServiceId,
>>::PayloadDispatcher as PayloadDispatcher<RuntimeServiceId>>::ChainNetworkService;

pub type PayloadDispatcherSettingsOfService<Service, RuntimeServiceId> =
    <<Service as ServiceComponents<RuntimeServiceId>>::PayloadDispatcher as PayloadDispatcher<
        RuntimeServiceId,
    >>::Settings;
