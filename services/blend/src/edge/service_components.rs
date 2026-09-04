use crate::{
    core::network::NetworkAdapter,
    edge::{BlendService, backends::BlendBackend},
};

/// Exposes associated types for external modules that depend on
/// [`BlendService`], without requiring them to specify its generic parameters.
pub trait ServiceComponents {
    /// Settings for broadcasting messages that have passed through the blend
    /// network.
    type BroadcastSettings;
    /// The adapter that puts a payload on the broadcasting channel, which an
    /// edge node needs to broadcast one the network failed to deliver.
    type NetworkAdapter;
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
    NetAdapter,
    ProofsGenerator,
    TimeBackend,
    ChainService,
    PolInfoProvider,
    RuntimeServiceId,
> ServiceComponents
    for BlendService<
        Backend,
        NodeId,
        NetAdapter,
        ProofsGenerator,
        TimeBackend,
        ChainService,
        PolInfoProvider,
        RuntimeServiceId,
    >
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone,
    NetAdapter: NetworkAdapter<RuntimeServiceId>,
{
    type BackendSettings = Backend::Settings;
    type BroadcastSettings = NetAdapter::BroadcastSettings;
    type NetworkAdapter = NetAdapter;
    type ProofsGenerator = ProofsGenerator;
    type ChainService = ChainService;
    type TimeBackend = TimeBackend;
}
