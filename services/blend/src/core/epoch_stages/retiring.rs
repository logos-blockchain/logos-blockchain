use lb_blend::message::reward::OldEpochBlendingTokenCollector;
use lb_chain_service::Epoch;

use crate::core::{
    OldEpochCryptographicProcessor as Processor,
    epoch_stages::{OldEpochScheduler, transitioning::TransitioningEpoch},
};

/// Everything a node leaving core mode still has to finish: the epoch it is
/// draining, and the blending tokens that epoch is owed for.
///
/// The two travel together because retirement is where they stop having
/// separate owners. While the service keeps running the tokens live in the
/// recovery state, so [`TransitioningEpoch`] alone is what the event loop
/// carries.
pub struct RetiringEpoch<Rng, ProofsVerifier> {
    transitioning: TransitioningEpoch<Rng, ProofsVerifier>,
    tokens: OldEpochBlendingTokenCollector,
}

impl<Rng, ProofsVerifier> RetiringEpoch<Rng, ProofsVerifier> {
    pub const fn new(
        transitioning: TransitioningEpoch<Rng, ProofsVerifier>,
        tokens: OldEpochBlendingTokenCollector,
    ) -> Self {
        Self {
            transitioning,
            tokens,
        }
    }

    /// The epoch being drained, which every message it releases is published
    /// under.
    pub const fn epoch(&self) -> Epoch {
        self.transitioning.epoch()
    }

    pub const fn scheduler_mut(&mut self) -> &mut OldEpochScheduler<Rng> {
        self.transitioning.scheduler_mut()
    }

    /// All three at once, which the incoming-message path needs: it reads the
    /// old processor to decapsulate, and writes both the old scheduler and the
    /// token collector with what comes out.
    pub const fn split_mut(
        &mut self,
    ) -> (
        &Processor<ProofsVerifier>,
        &mut OldEpochScheduler<Rng>,
        &mut OldEpochBlendingTokenCollector,
    ) {
        let (crypto, scheduler) = self.transitioning.split_mut();
        (crypto, scheduler, &mut self.tokens)
    }

    /// The tokens, once there is nothing left to drain and they are worth an
    /// activity proof.
    pub fn into_tokens(self) -> OldEpochBlendingTokenCollector {
        self.tokens
    }
}
