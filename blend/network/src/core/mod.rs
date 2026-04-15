pub mod with_core;
pub mod with_edge;

#[cfg(test)]
mod tests;

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
pub struct NetworkBehaviour<ObservationWindowClockProvider> {
    with_core: CoreToCoreBehaviour<ObservationWindowClockProvider>,
    with_edge: CoreToEdgeBehaviour,
}

pub struct Config {
    pub with_core: CoreToCoreConfig,
    pub with_edge: CoreToEdgeConfig,
}

impl<ObservationWindowClockProvider> NetworkBehaviour<ObservationWindowClockProvider> {
    pub fn new(
        config: &Config,
        observation_window_clock_provider: ObservationWindowClockProvider,
        current_session_info: (Membership<PeerId>, u64),
        local_peer_id: PeerId,
        protocol_name: StreamProtocol,
    ) -> Self {
        Self {
            with_core: CoreToCoreBehaviour::new(
                &config.with_core,
                observation_window_clock_provider,
                current_session_info.clone(),
                local_peer_id,
                protocol_name.clone(),
            ),
            with_edge: CoreToEdgeBehaviour::new(
                &config.with_edge,
                current_session_info.0,
                protocol_name,
            ),
        }
    }

    pub fn start_new_session(&mut self, new_session_info: (Membership<PeerId>, u64)) {
        self.with_core_mut()
            .start_new_session(new_session_info.clone());
        self.with_edge_mut().start_new_session(new_session_info.0);
    }

    pub const fn with_core(&self) -> &CoreToCoreBehaviour<ObservationWindowClockProvider> {
        &self.with_core
    }

    pub const fn with_core_mut(
        &mut self,
    ) -> &mut CoreToCoreBehaviour<ObservationWindowClockProvider> {
        &mut self.with_core
    }

    pub const fn with_edge(&self) -> &CoreToEdgeBehaviour {
        &self.with_edge
    }

    pub const fn with_edge_mut(&mut self) -> &mut CoreToEdgeBehaviour {
        &mut self.with_edge
    }

    pub fn finish_session_transition(&mut self) {
        self.with_core_mut().finish_session_transition();
    }
}
