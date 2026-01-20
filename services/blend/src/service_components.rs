use overwatch::services::ServiceData;

use crate::{BlendService, core, edge};

/// Exposes associated types for external modules that depend on
/// [`BlendService`], without requiring them to specify its generic parameters.
pub trait ServiceComponents {
    /// Settings for broadcasting messages that have passed through the blend
    /// network.
    type BroadcastSettings;
}

impl<CoreService, EdgeService, RuntimeServiceId> ServiceComponents
    for BlendService<CoreService, EdgeService, RuntimeServiceId>
where
    CoreService: ServiceData + core::service_components::ServiceComponents<RuntimeServiceId>,
    EdgeService: ServiceData + edge::service_components::ServiceComponents,
{
    type BroadcastSettings = EdgeService::BroadcastSettings;
}
