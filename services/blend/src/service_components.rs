use overwatch::services::ServiceData;

use crate::{BlendService, core, edge};

/// Exposes associated types for external modules that depend on
/// [`BlendService`], without requiring them to specify its generic parameters.
pub trait ServiceComponents {
    type NodeId;
}

impl<CoreService, EdgeService, SdpService, RuntimeServiceId> ServiceComponents
    for BlendService<CoreService, EdgeService, SdpService, RuntimeServiceId>
where
    CoreService: ServiceData + core::service_components::ServiceComponents<RuntimeServiceId>,
    EdgeService: ServiceData + edge::service_components::ServiceComponents,
{
    type NodeId = CoreService::NodeId;
}
