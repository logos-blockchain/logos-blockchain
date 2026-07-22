use lb_blend::{
    proofs::quota::inputs::prove::public::{CoreInputs, LeaderInputs},
    scheduling::membership::Membership,
};
use lb_chain_service::Epoch;
use lb_core::crypto::ZkHash;

#[derive(Clone, Debug)]
// TODO: Refactor this so that it's a struct with the common fields, and
// everything case-specific is an enum.
pub enum MaybeEmptyCoreEpochInfo<NodeId, CorePoQGenerator> {
    Empty { epoch: Epoch, epoch_nonce: ZkHash },
    NonEmpty(Box<CoreEpochInfo<NodeId, CorePoQGenerator>>),
}

impl<NodeId, CorePoQGenerator> From<(Epoch, ZkHash)>
    for MaybeEmptyCoreEpochInfo<NodeId, CorePoQGenerator>
{
    fn from((epoch, epoch_nonce): (Epoch, ZkHash)) -> Self {
        Self::Empty { epoch, epoch_nonce }
    }
}

impl<NodeId, CorePoQGenerator> From<CoreEpochInfo<NodeId, CorePoQGenerator>>
    for MaybeEmptyCoreEpochInfo<NodeId, CorePoQGenerator>
{
    fn from(core_epoch_info: CoreEpochInfo<NodeId, CorePoQGenerator>) -> Self {
        Self::NonEmpty(Box::new(core_epoch_info))
    }
}

#[derive(Clone, Debug)]
/// All info that Blend services need to be available on new epochs.
pub struct CoreEpochInfo<NodeId, CorePoQGenerator> {
    /// The epoch info available to all nodes.
    pub public: CoreEpochPublicInfo<NodeId>,
    /// The core `PoQ` generator component. `None` when Blend is running
    /// (network large enough), but local node is not part of the core
    /// membership.
    pub core_poq_generator: Option<CorePoQGenerator>,
}

#[derive(Clone, Debug)]
/// All public info that Blend services need to be available on new epochs.
pub struct CoreEpochPublicInfo<NodeId> {
    pub epoch: Epoch,
    pub poq_leadership_public_inputs: LeaderInputs,
    pub poq_core_public_inputs: CoreInputs,
    pub membership: Membership<NodeId>,
}
