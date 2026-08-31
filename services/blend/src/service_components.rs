use core::hash::Hash;

use lb_services_utils::overwatch::recovery::operators::RecoveryBackend as RecoveryBackendTrait;
use lb_utils::blake_rng::BlakeRng;

use crate::{
    BlendService,
    core::{backends::BlendBackend as CoreBlendBackend, dispatcher::PayloadDispatcher},
    edge::backends::BlendBackend as EdgeBlendBackend,
};

/// What every mode needs, whichever one the node is in.
pub trait Components<RuntimeServiceId> {
    type Settings;
    type NodeId;
    type Dispatcher;
    type TimeBackend;
    type ChainService;
    type PolInfoProvider;
    type SdpService;
    type StateStorage;
}

/// The deployment settings the dispatcher republishes through.
pub type NetworkSettingsOf<C, RuntimeServiceId> = <<C as Components<RuntimeServiceId>>::Dispatcher
    as PayloadDispatcher<RuntimeServiceId>>::Settings;

/// Exposes associated types for external modules that depend on
/// [`BlendService`], without requiring them to specify its generic parameters.
pub trait ServiceComponents {
    type NodeId;
}

impl<C, RuntimeServiceId> ServiceComponents for BlendService<C, RuntimeServiceId>
where
    C: Components<RuntimeServiceId, StateStorage: RecoveryBackendTrait<RuntimeServiceId>>
        + Components<
            RuntimeServiceId,
            Dispatcher: PayloadDispatcher<RuntimeServiceId>,
            Backend: CoreBlendBackend<
                C::NodeId,
                BlakeRng,
                <C as Components<RuntimeServiceId>>::ProofsVerifier,
                RuntimeServiceId,
            >,
        > + Components<RuntimeServiceId, Backend: EdgeBlendBackend<C::NodeId, RuntimeServiceId>>,
    C::NodeId: Clone + Eq + Hash,
{
    type NodeId = C::NodeId;
}
