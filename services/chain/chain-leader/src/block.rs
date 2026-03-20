use lb_blend_service::{ServiceComponents, core::network::NetworkAdapter as NetworkAdapterTrait};
use overwatch::services::ServiceData;

use crate::blend::BlendAdapter;

pub enum BlockProposalStrategy<'a, BlendService, NetworkAdapter, RuntimeServiceId>
where
    BlendService: ServiceData + ServiceComponents,
    NetworkAdapter: NetworkAdapterTrait<RuntimeServiceId>,
{
    Blend(&'a BlendAdapter<BlendService>),
    Broadcast {
        adapter: &'a NetworkAdapter,
        settings: NetworkAdapter::BroadcastSettings,
    },
}
