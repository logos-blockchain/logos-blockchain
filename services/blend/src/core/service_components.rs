use rand_chacha::ChaCha20Rng;

use crate::core::{
    backends::BlendBackend, dispatcher::PayloadDispatcher, state::RecoveryServiceState,
};

pub trait Components<RuntimeServiceId> {
    /// How this node is identified in a membership.
    type NodeId;
    /// The libp2p behaviour a core node listens and blends with.
    type Backend;
    /// Blend's exit door: where a fully decapsulated payload goes.
    type Dispatcher;
    /// Where an activity proof is submitted at the end of an epoch.
    type SdpService;
    /// Supplies this node's proofs across all three quota branches.
    type ProofsGenerator;
    /// Checks the proofs on an incoming message. Core-only: no other mode
    /// receives one.
    type ProofsVerifier;
    /// Where slot ticks come from.
    type TimeBackend;
    /// Where membership and epoch state come from.
    type ChainService;
    /// Where this node's winning-slot `PoL` info comes from.
    type PolInfoProvider;
    /// Where the recovery state is persisted between runs.
    type StateStorage;
}

/// The deployment settings the core backend is built from.
pub type BackendSettingsOf<Core, RuntimeServiceId> =
    <<Core as Components<RuntimeServiceId>>::Backend as BlendBackend<
        <Core as Components<RuntimeServiceId>>::NodeId,
        ChaCha20Rng,
        <Core as Components<RuntimeServiceId>>::ProofsVerifier,
        RuntimeServiceId,
    >>::Settings;

/// The deployment settings the dispatcher republishes through.
pub type NetworkSettingsOf<Core, RuntimeServiceId> =
    <<Core as Components<RuntimeServiceId>>::Dispatcher as PayloadDispatcher<
        RuntimeServiceId,
    >>::Settings;

/// The recovery state this service persists, as the storage backend sees it.
pub type RecoveryStateOf<Core, RuntimeServiceId> = RecoveryServiceState<
    BackendSettingsOf<Core, RuntimeServiceId>,
    NetworkSettingsOf<Core, RuntimeServiceId>,
>;

/// The network backend the dispatcher republishes through.
pub type NetworkBackendOfComponents<Core, RuntimeServiceId> = <<Core as Components<
    RuntimeServiceId,
>>::Dispatcher as PayloadDispatcher<
    RuntimeServiceId,
>>::Backend;

/// The mempool service the dispatcher hands transactions to.
pub type MempoolOfComponents<Core, RuntimeServiceId> =
    <<Core as Components<RuntimeServiceId>>::Dispatcher as PayloadDispatcher<
        RuntimeServiceId,
    >>::MempoolService;

/// The chain-network service the dispatcher observes broadcasts through.
pub type ChainNetworkOfComponents<Core, RuntimeServiceId> = <<Core as Components<
    RuntimeServiceId,
>>::Dispatcher as PayloadDispatcher<RuntimeServiceId>>::ChainNetworkService;
