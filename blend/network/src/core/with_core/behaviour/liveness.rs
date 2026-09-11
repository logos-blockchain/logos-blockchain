use std::collections::HashMap;

use lb_blend_primitives::time::{Round, RoundCount};
use libp2p::PeerId;

/// Tracks which core neighbours are still delivering messages.
///
/// A connection whose neighbour is [`Silent`](PeerState::Silent) is closed, but
/// that neighbour is never blacklisted for it: falling silent is not
/// attributable, since a neighbour can be silenced by an attack on it rather
/// than by any choice of its own.
pub struct PeerLivenessMap {
    /// `W`: how long a neighbour may go without delivering a message before the
    /// node concludes that it has fallen silent.
    window_length: RoundCount,
    observations: HashMap<PeerId, Observation>,
}

/// What the node has observed of one core neighbour.
#[derive(Debug, Clone, Copy)]
struct Observation {
    /// The round the node started watching this identity, which is what its one
    /// window of grace is measured from.
    since: Round,
    /// The round of the most recent message the neighbour delivered, if it has
    /// delivered one at all.
    last_received_message_round: Option<Round>,
}

impl Observation {
    pub const fn new(current_round: Round) -> Self {
        Self {
            since: current_round,
            last_received_message_round: None,
        }
    }
}

/// What the node can conclude about a core neighbour from what it has
/// delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerState {
    /// The neighbour delivered a message within the trailing window.
    Live,
    /// The neighbour has not been watched for a whole window yet, so nothing
    /// can be concluded about it.
    Settling,
    /// The neighbour has been watched for at least a whole window and delivered
    /// nothing in it.
    Silent,
}

impl PeerLivenessMap {
    #[must_use]
    pub fn new(window_length: RoundCount) -> Self {
        Self {
            window_length,
            observations: HashMap::new(),
        }
    }

    /// Starts watching a neighbour, if it is not already being watched.
    ///
    /// Deliberately does not reset an existing observation: a neighbour that
    /// reconnects resumes where it left off rather than earning a fresh window
    /// of grace, to avoid that peer taking up a slot indefinitely by dropping
    /// and re-connecting before `W` rounds have elapsed.
    pub fn start_or_resume_observing(&mut self, peer: PeerId, now: Round) {
        self.observations
            .entry(peer)
            .or_insert_with(|| Observation::new(now));
    }

    /// Records that a neighbour delivered a message.
    pub fn record_message_from_neighbour(&mut self, peer: PeerId, now: Round) {
        self.observations
            .entry(peer)
            .or_insert_with(|| Observation::new(now))
            .last_received_message_round = Some(now);
    }

    /// What the node can conclude about a neighbour as of `now`.
    #[must_use]
    pub fn current_peer_state(&self, peer: &PeerId, now: Round) -> Option<PeerState> {
        let observation = self.observations.get(peer)?;

        Some(match observation.last_received_message_round {
            Some(last_message) if self.is_within_window(now, last_message) => PeerState::Live,
            None if self.is_within_window(now, observation.since) => PeerState::Settling,
            _ => PeerState::Silent,
        })
    }

    pub fn is_connection_unhealthy(&self, peer: &PeerId, now: Round) -> bool {
        let Some(current_peer_state) = self.current_peer_state(peer, now) else {
            return false;
        };
        matches!(current_peer_state, PeerState::Silent)
    }

    /// Whether `earlier` falls inside the window trailing `now`.
    const fn is_within_window(&self, now: Round, earlier: Round) -> bool {
        now.rounds_since(earlier) < self.window_length.get()
    }

    pub fn clear(&mut self) {
        self.observations.clear();
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use lb_blend_primitives::time::{Round, RoundCount};
    use libp2p::PeerId;

    use crate::core::with_core::behaviour::liveness::{PeerLivenessMap, PeerState};

    const WINDOW_ROUNDS: NonZeroU128 = NonZeroU128::new(30).unwrap();

    fn liveness() -> PeerLivenessMap {
        PeerLivenessMap::new(RoundCount::new(WINDOW_ROUNDS))
    }

    fn round(round: u128) -> Round {
        Round::from(round)
    }

    #[test]
    fn an_unwatched_peer_keeps_its_connection() {
        let peer_id = PeerId::random();
        let peer_state = liveness().current_peer_state(&peer_id, round(100));

        assert_eq!(peer_state, None);
        assert!(!liveness().is_connection_unhealthy(&peer_id, round(100)));
    }

    #[test]
    fn a_peer_that_delivers_nothing_settles_for_one_window_then_falls_silent() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer, round(0));

        assert_eq!(
            liveness.current_peer_state(&peer, round(WINDOW_ROUNDS.get() - 1)),
            Some(PeerState::Settling),
            "the window of grace has not run out yet"
        );
        assert_eq!(
            liveness.current_peer_state(&peer, round(WINDOW_ROUNDS.get())),
            Some(PeerState::Silent),
            "the observation has reached `W` with nothing delivered"
        );
    }

    #[test]
    fn a_delivered_message_makes_a_peer_live_and_extends_the_window() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer, round(0));

        liveness.record_message_from_neighbour(peer, round(WINDOW_ROUNDS.get() - 1));

        assert_eq!(
            liveness.current_peer_state(&peer, round(2 * WINDOW_ROUNDS.get() - 2)),
            Some(PeerState::Live)
        );
        assert_eq!(
            liveness.current_peer_state(&peer, round(2 * WINDOW_ROUNDS.get() - 1)),
            Some(PeerState::Silent)
        );
    }

    #[test]
    fn a_peer_that_delivered_once_never_settles_again() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer, round(0));
        liveness.record_message_from_neighbour(peer, round(1));

        // Past the window that message opened it is silent, not back to
        // settling: the node has watched it long enough to conclude something.
        assert_eq!(
            liveness.current_peer_state(&peer, round(WINDOW_ROUNDS.get() + 1)),
            Some(PeerState::Silent)
        );
    }

    #[test]
    fn reconnecting_does_not_earn_a_fresh_window() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer, round(0));

        // The neighbour reconnects just before its window runs out, without
        // ever having delivered anything.
        liveness.start_or_resume_observing(peer, round(WINDOW_ROUNDS.get() - 1));

        assert_eq!(
            liveness.current_peer_state(&peer, round(WINDOW_ROUNDS.get())),
            Some(PeerState::Silent)
        );
    }

    #[test]
    fn clearing_forgets_every_observation() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer, round(0));
        assert_eq!(
            liveness.current_peer_state(&peer, round(WINDOW_ROUNDS.get())),
            Some(PeerState::Silent)
        );

        liveness.clear();

        assert_eq!(
            liveness.current_peer_state(&peer, round(WINDOW_ROUNDS.get())),
            Some(PeerState::Settling)
        );
    }
}
