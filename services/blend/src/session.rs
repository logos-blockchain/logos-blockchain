use lb_blend::{
    proofs::quota::inputs::prove::public::CoreInputs, scheduling::membership::Membership,
};

#[derive(Clone)]
/// All info that Blend services need to be available on new sessions.
pub struct CoreSessionInfo<NodeId, CorePoQGenerator> {
    /// The session info available to all nodes.
    pub public: CoreSessionPublicInfo<NodeId>,
    /// The core `PoQ` generator component.
    pub core_poq_generator: CorePoQGenerator,
}

#[derive(Clone)]
/// All public info that Blend services need to be available on new sessions.
pub struct CoreSessionPublicInfo<NodeId> {
    /// The list of core Blend nodes for the new session.
    pub membership: Membership<NodeId>,
    /// The session number.
    pub session: u64,
    /// The set of public inputs to verify core `PoQ`s.
    pub poq_core_public_inputs: CoreInputs,
}
