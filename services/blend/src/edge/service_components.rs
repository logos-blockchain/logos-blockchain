use crate::edge::{BlendService, backends::BlendBackend};

/// Exposes associated types for external modules that depend on
/// [`BlendService`], without requiring them to specify its generic parameters.
pub trait ServiceComponents {
    type ProofsGenerator;
    type BackendSettings;
    /// Chain service, used by the proxy to derive membership from the chain.
    type ChainService;
    /// Time backend, used by the proxy to subscribe to slot ticks.
    type TimeBackend;
}

impl<Backend, NodeId, ProofsGenerator, TimeBackend, ChainService, PolInfoProvider, RuntimeServiceId>
    ServiceComponents
    for BlendService<
        Backend,
        NodeId,
        ProofsGenerator,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone,
{
    type BackendSettings = Backend::Settings;
    type ProofsGenerator = ProofsGenerator;
    type ChainService = ChainService;
    type TimeBackend = TimeBackend;
}
