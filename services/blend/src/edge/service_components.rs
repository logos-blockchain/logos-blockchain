use crate::edge::{BlendService, backends::BlendBackend};

/// Exposes associated types for external modules that depend on
/// [`BlendService`], without requiring them to specify its generic parameters.
pub trait ServiceComponents {
    /// Settings for broadcasting messages that have passed through the blend
    /// network.
    type BroadcastSettings;
    type ProofsGenerator;
    type BackendSettings;
    /// Chain service, used by the proxy to derive membership from the chain.
    type ChainService;
    /// Time backend, used by the proxy to subscribe to slot ticks.
    type TimeBackend;
}

impl<
    Backend,
    NodeId,
    BroadcastSettings,
    ProofsGenerator,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> ServiceComponents
    for BlendService<
        Backend,
        NodeId,
        BroadcastSettings,
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
    type BroadcastSettings = BroadcastSettings;
    type ProofsGenerator = ProofsGenerator;
    type ChainService = ChainService;
    type TimeBackend = TimeBackend;
}
