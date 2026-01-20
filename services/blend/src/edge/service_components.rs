use crate::edge::{BlendService, backends::BlendBackend};

/// Exposes associated types for external modules that depend on
/// [`BlendService`], without requiring them to specify its generic parameters.
pub trait ServiceComponents {
    /// Settings for broadcasting messages that have passed through the blend
    /// network.
    type BroadcastSettings;
    /// Adapter for membership service.
    type MembershipAdapter;
    type ProofsGenerator;
    type BackendSettings;
}

impl<
    Backend,
    NodeId,
    BroadcastSettings,
    MembershipAdapter,
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
        MembershipAdapter,
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
    type MembershipAdapter = MembershipAdapter;
    type ProofsGenerator = ProofsGenerator;
}
