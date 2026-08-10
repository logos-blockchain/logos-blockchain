pub mod poq_verification;
pub mod with_core;
pub mod with_edge;

#[cfg(test)]
mod tests;

use lb_blend_scheduling::membership::Membership;
use lb_cryptarchia_engine::Epoch;
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
pub struct NetworkBehaviour<ObservationWindowClockProvider, ProofsVerifier> {
    with_core: CoreToCoreBehaviour<ObservationWindowClockProvider, ProofsVerifier>,
    with_edge: CoreToEdgeBehaviour<ProofsVerifier>,
}

pub struct Config {
    pub with_core: CoreToCoreConfig,
    pub with_edge: CoreToEdgeConfig,
}

impl<ObservationWindowClockProvider, ProofsVerifier>
    NetworkBehaviour<ObservationWindowClockProvider, ProofsVerifier>
where
    ProofsVerifier: Clone,
{
    pub fn new(
        config: &Config,
        observation_window_clock_provider: ObservationWindowClockProvider,
        current_epoch_info: (Membership<PeerId>, Epoch),
        proofs_verifier: ProofsVerifier,
        local_peer_id: PeerId,
        protocol_name: StreamProtocol,
    ) -> Self {
        Self {
            with_core: CoreToCoreBehaviour::new(
                &config.with_core,
                observation_window_clock_provider,
                current_epoch_info.clone(),
                proofs_verifier.clone(),
                local_peer_id,
                protocol_name.clone(),
            ),
            with_edge: CoreToEdgeBehaviour::new(
                &config.with_edge,
                current_epoch_info,
                proofs_verifier,
                protocol_name,
            ),
        }
    }

    pub fn start_new_epoch(
        &mut self,
        new_epoch_info: (Membership<PeerId>, Epoch),
        new_proofs_verifier: ProofsVerifier,
    ) {
        self.with_core_mut()
            .start_new_epoch(new_epoch_info.clone(), new_proofs_verifier.clone());
        self.with_edge_mut()
            .start_new_epoch(new_epoch_info, new_proofs_verifier);
    }

    pub const fn with_core(
        &self,
    ) -> &CoreToCoreBehaviour<ObservationWindowClockProvider, ProofsVerifier> {
        &self.with_core
    }

    pub const fn with_core_mut(
        &mut self,
    ) -> &mut CoreToCoreBehaviour<ObservationWindowClockProvider, ProofsVerifier> {
        &mut self.with_core
    }

    pub const fn with_edge(&self) -> &CoreToEdgeBehaviour<ProofsVerifier> {
        &self.with_edge
    }

    pub const fn with_edge_mut(&mut self) -> &mut CoreToEdgeBehaviour<ProofsVerifier> {
        &mut self.with_edge
    }

    pub fn finish_epoch_transition(&mut self) {
        self.with_core_mut().finish_epoch_transition();
    }
}
