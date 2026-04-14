use std::fmt::{self, Debug};

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

impl<BlendService, NetworkAdapter, RuntimeServiceId> Debug
    for BlockProposalStrategy<'_, BlendService, NetworkAdapter, RuntimeServiceId>
where
    BlendService: ServiceData + ServiceComponents,
    NetworkAdapter: NetworkAdapterTrait<RuntimeServiceId>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blend(_) => write!(f, "BlockProposalStrategy::Blend"),
            Self::Broadcast { .. } => {
                write!(f, "BlockProposalStrategy::Broadcast")
            }
        }
    }
}
