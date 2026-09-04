use lb_blend::{
    proofs::quota::inputs::prove::public::{CoreInputs, LeaderInputs, PowInputs},
    scheduling::membership::Membership,
};
use lb_chain_service::Epoch;
use lb_core::crypto::ZkHash;

#[derive(Clone, Debug)]
// TODO: Refactor this so that it's a struct with the common fields, and
// everything case-specific is an enum.
pub enum CoreEpochStateInfo<NodeId, CorePoQGenerator> {
    /// The node is a core node in the new epoch as well.
    Core(Box<CoreEpochInfo<NodeId, CorePoQGenerator>>),
    /// - The membership fell below the threshold, so Blend is running in
    ///   broadcast mode now
    /// - The node is simply not a core node for the new epoch, so it has
    ///   transitioned to edge mode.
    NotCore { epoch: Epoch, epoch_nonce: ZkHash },
}

/// The node is in the membership, but the core Merkle tree has no path for the
/// zk ID it is configured with.
#[derive(Clone, Debug, thiserror::Error)]
#[error(
    "The node is part of the membership but it declared a different zk ID than what has been configured. Please update your config with a matching zk ID and restart."
)]
pub struct MismatchedZkId;

impl<NodeId, CorePoQGenerator> From<(Epoch, ZkHash)>
    for CoreEpochStateInfo<NodeId, CorePoQGenerator>
{
    fn from((epoch, epoch_nonce): (Epoch, ZkHash)) -> Self {
        Self::NotCore { epoch, epoch_nonce }
    }
}

impl<NodeId, CorePoQGenerator> From<CoreEpochInfo<NodeId, CorePoQGenerator>>
    for CoreEpochStateInfo<NodeId, CorePoQGenerator>
{
    fn from(core_epoch_info: CoreEpochInfo<NodeId, CorePoQGenerator>) -> Self {
        Self::Core(Box::new(core_epoch_info))
    }
}

#[derive(Clone, Debug)]
/// All info that Blend services need to be available on new epochs.
pub struct CoreEpochInfo<NodeId, CorePoQGenerator> {
    /// The epoch info available to all nodes.
    pub public: CoreEpochPublicInfo<NodeId>,
    /// The core `PoQ` generator component.
    pub core_poq_generator: CorePoQGenerator,
}

#[derive(Clone, Debug)]
/// All public info that Blend services need to be available on new epochs.
pub struct CoreEpochPublicInfo<NodeId> {
    pub epoch: Epoch,
    pub poq_leadership_public_inputs: LeaderInputs,
    pub poq_core_public_inputs: CoreInputs,
    pub poq_pow_public_inputs: PowInputs,
    pub membership: Membership<NodeId>,
}
