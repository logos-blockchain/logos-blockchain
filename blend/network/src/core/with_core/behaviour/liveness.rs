use std::collections::HashMap;

use lb_blend_primitives::time::{Round, RoundCount};
use libp2p::PeerId;

/// Tracks which core neighbours are still delivering messages.
///
/// The Blend specification defines a connection with a core node as **live**
/// when the neighbour has delivered a message — duplicate or not — within the
/// trailing observation window `W`, and a connection that counts as live until
/// its observation reaches `W`. A connection that is not live is closed, but
/// its neighbour is never blacklisted for it: falling silent is not
/// attributable, since a neighbour can be silenced by an attack on it rather
/// than by any choice of its own.
pub struct PeerLivenessMap {
    /// `W`: how long a neighbour may go without delivering a message before its
    /// connection stops counting as live.
    window_length: RoundCount,
    /// The most recent round in which each neighbour gave evidence of being
    /// alive.
    last_evidence: HashMap<PeerId, Round>,
}

impl PeerLivenessMap {
    #[must_use]
    pub fn new(window_length: RoundCount) -> Self {
        Self {
            window_length,
            last_evidence: HashMap::new(),
        }
    }

    /// Starts observing a neighbour, if it is not already being observed.
    ///
    /// Deliberately does not reset an existing observation: a neighbour that
    /// reconnects resumes where it left off rather than earning a fresh window
    /// of grace, to avoid that peer taking up a slot indefinitely by dropping
    /// and re-connecting before `W` rounds have elapsed.
    pub fn start_or_resume_observing(&mut self, peer: PeerId, now: Round) {
        self.last_evidence.entry(peer).or_insert(now);
    }

    /// Records that a neighbour delivered a message.
    pub fn record_message_from_neighbour(&mut self, peer: PeerId, now: Round) {
        self.last_evidence.insert(peer, now);
    }

    /// Whether a neighbour has given evidence of being alive within the
    /// trailing window.
    #[must_use]
    pub fn is_neighbour_live(&self, peer: &PeerId, now: Round) -> bool {
        self.last_evidence
            .get(peer)
            .is_none_or(|last| self.window_length >= now.rounds_since(*last))
    }

    pub fn clear(&mut self) {
        self.last_evidence.clear();
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use lb_blend_primitives::time::{Round, RoundCount};
    use libp2p::PeerId;

    use crate::core::with_core::behaviour::liveness::PeerLivenessMap;

    const WINDOW_ROUNDS: NonZeroU128 = NonZeroU128::new(30).unwrap();

    fn liveness() -> PeerLivenessMap {
        PeerLivenessMap::new(RoundCount::new(WINDOW_ROUNDS))
    }

    #[test]
    fn an_unobserved_peer_is_live() {
        assert!(liveness().is_neighbour_live(&PeerId::random(), Round::from(100)));
    }

    #[test]
    fn a_new_peer_is_live_for_one_window() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer, Round::from(0));

        assert!(liveness.is_neighbour_live(&peer, Round::from(WINDOW_ROUNDS.get() - 1)));
        assert!(!liveness.is_neighbour_live(&peer, Round::from(WINDOW_ROUNDS.get())));
    }

    #[test]
    fn a_delivered_message_extends_the_window() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer, Round::from(0));

        liveness.record_message_from_neighbour(peer, Round::from(WINDOW_ROUNDS.get() - 1));

        assert!(liveness.is_neighbour_live(&peer, Round::from(2 * WINDOW_ROUNDS.get() - 2)));
        assert!(!liveness.is_neighbour_live(&peer, Round::from(2 * WINDOW_ROUNDS.get() - 1)));
    }

    #[test]
    fn reconnecting_does_not_earn_a_fresh_window() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer, Round::from(0));

        // The neighbour reconnects just before its window runs out, without
        // ever having delivered anything.
        liveness.start_or_resume_observing(peer, Round::from(WINDOW_ROUNDS.get() - 1));

        assert!(!liveness.is_neighbour_live(&peer, Round::from(WINDOW_ROUNDS.get())));
    }

    #[test]
    fn clearing_forgets_every_observation() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer, Round::from(0));
        assert!(!liveness.is_neighbour_live(&peer, Round::from(WINDOW_ROUNDS.get())));

        liveness.clear();

        assert!(liveness.is_neighbour_live(&peer, Round::from(WINDOW_ROUNDS.get())));
    }
}
