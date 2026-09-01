use crate::{
    BlendService,
    core::{
        backends::BlendBackend as CoreBlendBackend, dispatcher::PayloadDispatcher,
        service_components::Components as CoreComponents,
    },
    edge::{
        backends::BlendBackend as EdgeBlendBackend,
        service_components::Components as EdgeComponents,
    },
};

/// Exposes the node id [`BlendService`] identifies peers by, without requiring
/// a caller to name its generic parameters.
pub trait ServiceComponents {
    type NodeId;
}

impl<Core, Edge, Broadcast, SdpService, RuntimeServiceId> ServiceComponents
    for BlendService<Core, Edge, Broadcast, SdpService, RuntimeServiceId>
where
    Core: CoreComponents<RuntimeServiceId>,
    Core::Backend: CoreBlendBackend<
            Core::NodeId,
            rand_chacha::ChaCha20Rng,
            Core::ProofsVerifier,
            RuntimeServiceId,
        >,
    Core::Dispatcher: PayloadDispatcher<RuntimeServiceId>,
    Edge: EdgeComponents<
            RuntimeServiceId,
            NodeId: Clone,
            Dispatcher: PayloadDispatcher<RuntimeServiceId>,
        >,
    Edge::Backend: EdgeBlendBackend<Edge::NodeId, RuntimeServiceId>,
{
    type NodeId = Core::NodeId;
}
