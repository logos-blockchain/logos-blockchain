pub mod with_core;
pub mod with_edge;

pub(crate) mod admission;

mod poq_verification;

#[cfg(test)]
mod tests;

use core::num::{NonZeroU64, NonZeroUsize};

use lb_blend_membership::Membership;
use lb_blend_primitives::time::RoundClock;
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
pub struct NetworkBehaviour<ProofsVerifier> {
    with_core: CoreToCoreBehaviour<ProofsVerifier>,
    with_edge: CoreToEdgeBehaviour<ProofsVerifier>,
}

pub struct Config {
    pub common: CommonConfig,
    pub with_core: CoreToCoreConfig,
    pub with_edge: CoreToEdgeConfig,
}

pub struct CommonConfig {
    pub round_duration_in_seconds: NonZeroU64,
    pub minimum_network_size: NonZeroUsize,
    pub num_blend_layers: NonZeroU64,
}

impl<ProofsVerifier> NetworkBehaviour<ProofsVerifier>
where
    ProofsVerifier: Clone,
{
    pub fn new(
        config: &Config,
        current_epoch_info: (Membership<PeerId>, Epoch),
        proofs_verifier: ProofsVerifier,
        local_peer_id: PeerId,
        protocol_name: StreamProtocol,
    ) -> Self {
        let round_clock = RoundClock::new(config.common.round_duration_in_seconds);
        Self {
            with_core: CoreToCoreBehaviour::new(
                (&config.common, &config.with_core),
                current_epoch_info.clone(),
                proofs_verifier.clone(),
                local_peer_id,
                round_clock.clone(),
                protocol_name.clone(),
            ),
            with_edge: CoreToEdgeBehaviour::new(
                (&config.common, &config.with_edge),
                current_epoch_info,
                round_clock,
                proofs_verifier,
                protocol_name,
            ),
        }
    }

    pub fn start_new_epoch(
        &mut self,
        new_epoch_info: (Membership<PeerId>, Epoch),
        new_proofs_verifier: ProofsVerifier,
        new_era: Option<(&Config, StreamProtocol)>,
    ) {
        let new_era_components = new_era.map(|(new_era_config, new_era_protocol_name)| {
            let new_era_round_duration = new_era_config.common.round_duration_in_seconds;
            let new_era_round_clock = (new_era_round_duration
                != self.with_core().round_duration_in_seconds())
            .then(|| {
                self.with_core()
                    .new_round_clock_with_updated_round_duration(new_era_round_duration)
            });
            (new_era_config, new_era_round_clock, new_era_protocol_name)
        });
        self.with_core_mut().start_new_epoch(
            new_epoch_info.clone(),
            new_proofs_verifier.clone(),
            new_era_components.as_ref().map(
                |(new_era_config, new_era_round_clock, new_era_protocol_name)| {
                    (
                        (&new_era_config.common, &new_era_config.with_core),
                        new_era_round_clock.clone(),
                        new_era_protocol_name.clone(),
                    )
                },
            ),
        );
        self.with_edge_mut().start_new_epoch(
            new_epoch_info,
            new_proofs_verifier,
            new_era_components.map(
                |(new_era_config, new_era_round_clock, new_era_protocol_name)| {
                    (
                        (&new_era_config.common, &new_era_config.with_edge),
                        new_era_round_clock,
                        new_era_protocol_name,
                    )
                },
            ),
        );
    }

    pub const fn with_core(&self) -> &CoreToCoreBehaviour<ProofsVerifier> {
        &self.with_core
    }

    pub const fn with_core_mut(&mut self) -> &mut CoreToCoreBehaviour<ProofsVerifier> {
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
