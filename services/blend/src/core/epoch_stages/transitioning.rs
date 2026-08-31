use lb_chain_service::Epoch;

use crate::core::{OldEpochCryptographicProcessor as Processor, epoch_stages::OldEpochScheduler};

/// An epoch that has ended but is not finished with.
///
/// For the length of the transition period a node still has to decapsulate
/// messages minted under the old epoch's `PoQ` and release whatever that epoch
/// had queued, so both halves outlive the rotation together and are dropped
/// together. They were previously two independent `Option`s that every caller
/// had to keep in step; pairing them makes "an old epoch is transitioning" a
/// single fact rather than an invariant held by hand.
///
/// The blending tokens the old epoch is still earning are *not* here, because
/// while the service is running the persisted recovery state owns them — which
/// is what carries them across a restart. A node on its way out of core has
/// consumed that state, so it pairs this with the tokens itself: see
/// [`RetiringEpoch`].
pub struct TransitioningEpoch<Rng, ProofsVerifier> {
    crypto: Processor<ProofsVerifier>,
    scheduler: OldEpochScheduler<Rng>,
}

impl<Rng, ProofsVerifier> TransitioningEpoch<Rng, ProofsVerifier> {
    pub const fn new(crypto: Processor<ProofsVerifier>, scheduler: OldEpochScheduler<Rng>) -> Self {
        Self { crypto, scheduler }
    }

    #[cfg(test)]
    pub const fn scheduler(&self) -> &OldEpochScheduler<Rng> {
        &self.scheduler
    }

    pub const fn scheduler_mut(&mut self) -> &mut OldEpochScheduler<Rng> {
        &mut self.scheduler
    }

    /// The epoch being drained, which every message it releases is published
    /// under so it reaches the peers still negotiated for it.
    pub const fn epoch(&self) -> Epoch {
        self.crypto.epoch()
    }

    /// Both halves at once, which the incoming-message path needs: it reads the
    /// old processor to decapsulate and writes the old scheduler to queue the
    /// result. Borrowing them through one method keeps that disjoint.
    pub const fn split_mut(&mut self) -> (&Processor<ProofsVerifier>, &mut OldEpochScheduler<Rng>) {
        (&self.crypto, &mut self.scheduler)
    }
}
