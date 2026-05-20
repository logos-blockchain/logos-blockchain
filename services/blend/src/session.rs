use lb_blend::{
    proofs::quota::inputs::prove::public::CoreInputs, scheduling::membership::Membership,
};

#[derive(Clone, Debug)]
// TODO: Refactor this so that it's a struct with the common fields, and
// everything case-specific is an enum.
pub enum MaybeEmptyCoreSessionInfo<NodeId, CorePoQGenerator> {
    Empty { session: u64 },
    NonEmpty(CoreSessionInfo<NodeId, CorePoQGenerator>),
}

impl<NodeId, CorePoQGenerator> From<u64> for MaybeEmptyCoreSessionInfo<NodeId, CorePoQGenerator> {
    fn from(session: u64) -> Self {
        Self::Empty { session }
    }
}

impl<NodeId, CorePoQGenerator> From<CoreSessionInfo<NodeId, CorePoQGenerator>>
    for MaybeEmptyCoreSessionInfo<NodeId, CorePoQGenerator>
{
    fn from(core_session_info: CoreSessionInfo<NodeId, CorePoQGenerator>) -> Self {
        Self::NonEmpty(core_session_info)
    }
}

#[derive(Clone, Debug)]
/// All info that Blend services need to be available on new sessions.
pub struct CoreSessionInfo<NodeId, CorePoQGenerator> {
    /// The session info available to all nodes.
    pub public: CoreSessionPublicInfo<NodeId>,
    /// The core `PoQ` generator component. `None` when Blend is running
    /// (network large enough), but local node is not part of the core
    /// membership.
    pub core_poq_generator: Option<CorePoQGenerator>,
}

#[derive(Clone, Debug)]
/// All public info that Blend services need to be available on new sessions.
pub struct CoreSessionPublicInfo<NodeId> {
    /// The list of core Blend nodes for the new session.
    pub membership: Membership<NodeId>,
    /// The session number.
    pub session: u64,
    /// The set of public inputs to verify core `PoQ`s.
    pub poq_core_public_inputs: CoreInputs,
}
