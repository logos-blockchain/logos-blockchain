use lb_blend::{
    proofs::quota::inputs::prove::public::{CoreInputs, LeaderInputs, PowInputs},
    scheduling::membership::Membership,
};
use lb_chain_service::Epoch;
use lb_core::crypto::ZkHash;
use lb_cryptarchia_engine::era::Era;

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
    NotCore {
        era: Era,
        epoch: Epoch,
        epoch_nonce: ZkHash,
    },
}

impl<NodeId, CorePoQGenerator> CoreEpochStateInfo<NodeId, CorePoQGenerator> {
    /// The era of the epoch.
    #[must_use]
    pub const fn era(&self) -> Era {
        match self {
            Self::Core(info) => info.public.era,
            Self::NotCore { era, .. } => *era,
        }
    }

    /// The epoch.
    #[must_use]
    pub const fn epoch(&self) -> Epoch {
        match self {
            Self::Core(info) => info.public.epoch,
            Self::NotCore { epoch, .. } => *epoch,
        }
    }
}

impl<NodeId, CorePoQGenerator> CoreEpochStateInfo<NodeId, CorePoQGenerator> {
    #[must_use]
    pub fn epoch(&self) -> Epoch {
        match self {
            Self::Core(info) => info.epoch(),
            Self::NotCore { epoch, .. } => *epoch,
        }
    }
}

/// The node is in the membership, but the core Merkle tree has no path for the
/// zk ID it is configured with.
#[derive(Clone, Debug, thiserror::Error)]
#[error(
    "The node is part of the membership but it declared a different zk ID than what has been configured. Please update your config with a matching zk ID and restart."
)]
pub struct MismatchedZkId;

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

impl<NodeId, CorePoQGenerator> CoreEpochInfo<NodeId, CorePoQGenerator> {
    pub const fn epoch(&self) -> Epoch {
        self.public.epoch
    }
}

#[derive(Clone, Debug)]
/// All public info that Blend services need to be available on new epochs.
pub struct CoreEpochPublicInfo<NodeId> {
    /// The era of the epoch, whose settings the epoch runs under.
    pub era: Era,
    pub epoch: Epoch,
    pub poq_leadership_public_inputs: LeaderInputs,
    pub poq_core_public_inputs: CoreInputs,
    pub poq_pow_public_inputs: PowInputs,
    pub membership: Membership<NodeId>,
}
