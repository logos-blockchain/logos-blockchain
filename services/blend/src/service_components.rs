use overwatch::services::ServiceData;

use crate::{BlendService, broadcast, core, edge};

/// Exposes associated types for external modules that depend on
/// [`BlendService`], without requiring them to specify its generic parameters.
pub trait ServiceComponents {
    type NodeId;
}

impl<CoreService, EdgeService, BroadcastService, SdpService, RuntimeServiceId> ServiceComponents
    for BlendService<CoreService, EdgeService, BroadcastService, SdpService, RuntimeServiceId>
where
    CoreService: ServiceData + core::service_components::ServiceComponents<RuntimeServiceId>,
    EdgeService: ServiceData + edge::service_components::ServiceComponents,
    BroadcastService: ServiceData + broadcast::service_components::ServiceComponents,
{
    type NodeId = CoreService::NodeId;
}
