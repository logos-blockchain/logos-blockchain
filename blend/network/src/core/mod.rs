pub mod with_core;
pub mod with_edge;

#[cfg(test)]
mod tests;

use lb_blend_message::encap;
use lb_blend_proofs::quota::inputs::prove::public::LeaderInputs;
use lb_blend_scheduling::membership::Membership;
use libp2p::{PeerId, StreamProtocol};

use self::{
    with_core::behaviour::Behaviour as CoreToCoreBehaviour,
    with_edge::behaviour::Behaviour as CoreToEdgeBehaviour,
};
use crate::core::{
    with_core::behaviour::Config as CoreToCoreConfig,
    with_edge::behaviour::Config as CoreToEdgeConfig,
};

/// A composed behaviour that wraps the two sub-behaviours for dealing with core
/// and edge nodes.
#[derive(lb_libp2p::NetworkBehaviour)]
pub struct NetworkBehaviour<ProofsVerifier, ObservationWindowClockProvider> {
    with_core: CoreToCoreBehaviour<ProofsVerifier, ObservationWindowClockProvider>,
    with_edge: CoreToEdgeBehaviour<ProofsVerifier>,
}

impl<ProofsVerifier, ObservationWindowClockProvider>
    NetworkBehaviour<ProofsVerifier, ObservationWindowClockProvider>
{
    pub const fn with_core(
        &self,
    ) -> &CoreToCoreBehaviour<ProofsVerifier, ObservationWindowClockProvider> {
        &self.with_core
    }

    pub const fn with_core_mut(
        &mut self,
    ) -> &mut CoreToCoreBehaviour<ProofsVerifier, ObservationWindowClockProvider> {
        &mut self.with_core
    }

    pub const fn with_edge(&self) -> &CoreToEdgeBehaviour<ProofsVerifier> {
        &self.with_edge
    }

    pub const fn with_edge_mut(&mut self) -> &mut CoreToEdgeBehaviour<ProofsVerifier> {
        &mut self.with_edge
    }

    pub fn finish_session_transition(&mut self) {
        self.with_core_mut().finish_session_transition();
        self.with_edge_mut().finish_session_transition();
    }
}

pub struct Config {
    pub with_core: CoreToCoreConfig,
    pub with_edge: CoreToEdgeConfig,
}

impl<ProofsVerifier, ObservationWindowClockProvider>
    NetworkBehaviour<ProofsVerifier, ObservationWindowClockProvider>
where
    ProofsVerifier: Clone,
{
    pub fn new(
        config: &Config,
        observation_window_clock_provider: ObservationWindowClockProvider,
        current_membership: Membership<PeerId>,
        local_peer_id: PeerId,
        protocol_name: StreamProtocol,
        poq_verifier: ProofsVerifier,
    ) -> Self {
        Self {
            with_core: CoreToCoreBehaviour::new(
                &config.with_core,
                observation_window_clock_provider,
                current_membership.clone(),
                local_peer_id,
                protocol_name.clone(),
                poq_verifier.clone(),
            ),
            with_edge: CoreToEdgeBehaviour::new(
                &config.with_edge,
                current_membership,
                protocol_name,
                poq_verifier,
            ),
        }
    }

    pub fn start_new_session(
        &mut self,
        new_membership: Membership<PeerId>,
        new_verifier: ProofsVerifier,
    ) {
        self.with_core_mut()
            .start_new_session(new_membership.clone(), new_verifier.clone());
        self.with_edge_mut()
            .start_new_session(new_membership, new_verifier);
    }
}

impl<ProofsVerifier, ObservationWindowClockProvider>
    NetworkBehaviour<ProofsVerifier, ObservationWindowClockProvider>
where
    ProofsVerifier: encap::ProofsVerifier,
{
    pub fn start_new_epoch(&mut self, new_pol_inputs: LeaderInputs) {
        self.with_core_mut().start_new_epoch(new_pol_inputs);
        self.with_edge_mut().start_new_epoch(new_pol_inputs);
    }

    pub fn finish_epoch_transition(&mut self) {
        self.with_core_mut().finish_epoch_transition();
        self.with_edge_mut().finish_epoch_transition();
    }
}
