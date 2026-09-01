use crate::{core::dispatcher::PayloadDispatcher, edge::backends::BlendBackend};

pub trait Components<RuntimeServiceId> {
    /// How this node is identified in a membership.
    type NodeId;
    /// The libp2p behaviour an edge node dials out with. It never listens,
    /// which is why a draining core backend and a new edge one can coexist.
    type Backend;
    /// Supplies this node's proofs — leadership and `PoW`, but no core branch:
    /// an edge node holds no core quota.
    type ProofsGenerator;
    /// Where slot ticks come from.
    type TimeBackend;
    /// Where membership and epoch state come from.
    type ChainService;
    /// Where this node's winning-slot `PoL` info comes from.
    type PolInfoProvider;
    /// Blend's exit door. An edge node needs one so it can watch its own
    /// messages come back off the wire, and re-send the ones that never did.
    type Dispatcher;
}

/// The deployment settings the edge backend is built from.
pub type EdgeBackendSettingsOf<Edge, RuntimeServiceId> =
    <<Edge as Components<RuntimeServiceId>>::Backend as BlendBackend<
        <Edge as Components<RuntimeServiceId>>::NodeId,
        RuntimeServiceId,
    >>::Settings;

/// The deployment settings the dispatcher republishes through.
pub type EdgeNetworkSettingsOf<Edge, RuntimeServiceId> =
    <<Edge as Components<RuntimeServiceId>>::Dispatcher as PayloadDispatcher<
        RuntimeServiceId,
    >>::Settings;
