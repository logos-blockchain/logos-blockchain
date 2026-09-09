use crate::{broadcast::BlendService, core::dispatcher::PayloadDispatcher};

/// Exposes associated types for external modules that depend on
/// [`BlendService`], without requiring them to specify its generic parameters.
pub trait ServiceComponents {
    type PayloadDispatcher;
    /// Chain service, used by the orchestrator to derive membership from the
    /// chain.
    type ChainService;
    /// Time backend, used by the orchestrator to subscribe to slot ticks.
    type TimeBackend;
}

impl<NodeId, Dispatcher, TimeBackend, ChainService, RuntimeServiceId> ServiceComponents
    for BlendService<NodeId, Dispatcher, TimeBackend, ChainService, RuntimeServiceId>
where
    Dispatcher: PayloadDispatcher<RuntimeServiceId>,
{
    type PayloadDispatcher = Dispatcher;
    type ChainService = ChainService;
    type TimeBackend = TimeBackend;
}
