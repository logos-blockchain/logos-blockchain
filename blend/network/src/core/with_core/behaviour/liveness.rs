use std::collections::HashMap;

use lb_blend_primitives::time::{Round, RoundCount};
use lb_log_targets::blend;
use libp2p::PeerId;

const MAINTENANCE_TARGET: &str = blend::network::core::core::behaviour::MAINTENANCE;

/// Tracks which core neighbours are still delivering messages.
///
/// A connection whose neighbour is [`Silent`](PeerState::Silent) is closed, and
/// the neighbour refused for `W` rounds (see [`NotLiveRefusals`]), but never
/// blacklisted for it: falling silent is not attributable, since a neighbour
/// can be silenced by an attack on it rather than by any choice of its own.
///
/// The window is measured in **connected** rounds, not in elapsed ones. Rounds
/// during which the node held no connection with a neighbour are not rounds in
/// which that neighbour failed to deliver anything, and counting them would
/// make a peer returning from an outage longer than `W` unreachable: it would
/// read as silent the moment it reconnected, be closed before it could deliver,
/// and reconnect into the same verdict.
///
/// Similarly, not keeping track of rounds during re-connects would open the
/// door for vulnerability where a peer constantly disconnects before the
/// observation window matures, without being considered silent, leading to one
/// slot constantly taken: multiplied by `N` malicious nodes, this one node can
/// be easily eclipsed.
///
/// The count of a neighbour starts over only when the node closes its
/// connection for not being live, and the neighbour has to sit out its refusal
/// to get the fresh window: a silent identity holds a slot for at most `W` of
/// every `2·W` rounds.
pub struct PeerLivenessMap {
    /// `W`: how many **connected** rounds a neighbour may go without delivering
    /// a message before the node concludes that it has fallen silent.
    window_length: RoundCount,
    observations: HashMap<PeerId, Observation>,
}

/// What the node has observed of one core neighbour, counted per identity for
/// the epoch and across every connection the node has held with it, since it
/// last closed one for not being live.
#[derive(Debug, Clone, Copy, Default)]
struct Observation {
    /// How many rounds the node has held a connection with this identity.
    connection_duration_in_rounds: u128,
    /// How many rounds this identity had been connected for when it last
    /// delivered a message, if it has delivered one at all.
    connection_maturity_at_last_delivery: Option<u128>,
}

impl Observation {
    pub const fn increase_connection_duration(&mut self) {
        self.connection_duration_in_rounds = self.connection_duration_in_rounds.saturating_add(1);
    }

    pub const fn record_delivery(&mut self) {
        self.connection_maturity_at_last_delivery = Some(self.connection_duration_in_rounds);
    }
}

/// What the node can conclude about a core neighbour from what it has
/// delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerState {
    /// The neighbour delivered a message within the trailing window.
    Live,
    /// The neighbour has not been connected for a whole window yet, so nothing
    /// can be concluded about it.
    Settling,
    /// The neighbour has been connected for at least a whole window and
    /// delivered nothing in it.
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
    /// and re-connecting before it has delivered anything.
    pub fn start_or_resume_observing(&mut self, peer: PeerId) {
        self.observations.entry(peer).or_default();
    }

    /// Forgets what the node has observed of a neighbour, so that the next
    /// connection with it starts a fresh count.
    ///
    /// Only for a neighbour whose connection the node closed for not being
    /// live, which is refused for `W` rounds first.
    pub fn forget(&mut self, peer: &PeerId) {
        self.observations.remove(peer);
    }

    /// Counts one connected round for each neighbour the node currently holds a
    /// connection with.
    pub fn enter_new_round_with_peers<'peer, Peers>(&mut self, connected_peers: Peers)
    where
        Peers: Iterator<Item = &'peer PeerId>,
    {
        for peer in connected_peers {
            self.observations
                .entry(*peer)
                .or_default()
                .increase_connection_duration();
        }
    }

    /// Records that a neighbour delivered a message.
    pub fn record_message_from_neighbour(&mut self, peer: PeerId) {
        self.observations.entry(peer).or_default().record_delivery();
    }

    /// What the node can conclude about a neighbour.
    #[must_use]
    pub fn current_peer_state(&self, peer: &PeerId) -> Option<PeerState> {
        let observation = self.observations.get(peer)?;

        Some(match observation.connection_maturity_at_last_delivery {
            Some(last_delivery) if self.is_within_window(observation, last_delivery) => {
                PeerState::Live
            }
            None if self.is_within_window(observation, 0) => PeerState::Settling,
            _ => PeerState::Silent,
        })
    }

    #[must_use]
    pub fn is_connection_unhealthy(&self, peer: &PeerId) -> bool {
        matches!(self.current_peer_state(peer), Some(PeerState::Silent))
    }

    /// Whether `earlier` falls inside the window of connected rounds trailing
    /// the ones this neighbour has accrued.
    const fn is_within_window(&self, observation: &Observation, earlier: u128) -> bool {
        observation
            .connection_duration_in_rounds
            .saturating_sub(earlier)
            < self.window_length.get()
    }

    pub fn clear(&mut self) {
        self.observations.clear();
    }
}

/// The neighbours this node refuses on incoming and on outgoing connections,
/// for `W` rounds after it closed a connection with them for not being live.
pub struct NotLiveRefusals {
    /// `W`: how many rounds a refusal lasts.
    duration: RoundCount,
    /// The round each refused neighbour may connect again from.
    refused_until: HashMap<PeerId, Round>,
}

impl NotLiveRefusals {
    #[must_use]
    pub fn new(duration: RoundCount) -> Self {
        Self {
            duration,
            refused_until: HashMap::new(),
        }
    }

    /// Sets how long the refusals added from now on last. The ones already in
    /// place keep the window they were added with.
    pub const fn update_duration(&mut self, duration: RoundCount) {
        self.duration = duration;
    }

    /// Refuses a neighbour for `W` rounds from `now`.
    pub fn refuse(&mut self, peer: PeerId, now: Round) {
        self.refused_until
            .insert(peer, now.saturating_add(self.duration));
    }

    /// Whether the neighbour is refused as of `now`.
    #[must_use]
    pub fn is_refused_at(&self, peer: &PeerId, now: Round) -> bool {
        self.refused_until
            .get(peer)
            .is_some_and(|refused_until| now < *refused_until)
    }

    /// The neighbours refused as of `now`.
    pub fn iter(&self, now: Round) -> impl Iterator<Item = &PeerId> {
        self.refused_until
            .iter()
            .filter(move |(_, refused_until)| now < **refused_until)
            .map(|(peer, _)| peer)
    }

    /// Drops the refusals whose window has passed as of `now`.
    pub fn prune_expired(&mut self, now: Round) {
        self.refused_until.retain(|peer, refused_until| {
            if now < *refused_until {
                return true;
            }
            tracing::debug!(
                target: MAINTENANCE_TARGET,
                "Peer {peer:?} is no longer refused: the window since its connection was closed for not being live has passed."
            );
            false
        });
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use lb_blend_primitives::time::RoundCount;
    use libp2p::PeerId;

    use crate::core::with_core::behaviour::liveness::{PeerLivenessMap, PeerState};

    const WINDOW_ROUNDS: NonZeroU128 = NonZeroU128::new(30).unwrap();

    fn liveness() -> PeerLivenessMap {
        PeerLivenessMap::new(RoundCount::new(WINDOW_ROUNDS))
    }

    /// Turns `rounds` rounds with `peer` connected throughout.
    fn connected_for(liveness: &mut PeerLivenessMap, peer: &PeerId, rounds: u128) {
        for _ in 0..rounds {
            liveness.enter_new_round_with_peers(core::iter::once(peer));
        }
    }

    #[test]
    fn an_unwatched_peer_keeps_its_connection() {
        let peer_id = PeerId::random();

        assert_eq!(liveness().current_peer_state(&peer_id), None);
        assert!(!liveness().is_connection_unhealthy(&peer_id));
    }

    #[test]
    fn a_peer_that_delivers_nothing_settles_for_one_window_then_falls_silent() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer);

        connected_for(&mut liveness, &peer, WINDOW_ROUNDS.get() - 1);
        assert_eq!(
            liveness.current_peer_state(&peer),
            Some(PeerState::Settling),
            "the window of grace has not run out yet"
        );

        connected_for(&mut liveness, &peer, 1);
        assert_eq!(
            liveness.current_peer_state(&peer),
            Some(PeerState::Silent),
            "the neighbour has been connected for `W` rounds with nothing delivered"
        );
    }

    #[test]
    fn a_delivered_message_makes_a_peer_live_and_extends_the_window() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer);

        connected_for(&mut liveness, &peer, WINDOW_ROUNDS.get() - 1);
        liveness.record_message_from_neighbour(peer);

        connected_for(&mut liveness, &peer, WINDOW_ROUNDS.get() - 1);
        assert_eq!(liveness.current_peer_state(&peer), Some(PeerState::Live));

        connected_for(&mut liveness, &peer, 1);
        assert_eq!(liveness.current_peer_state(&peer), Some(PeerState::Silent));
    }

    #[test]
    fn a_peer_that_delivered_once_never_settles_again() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer);
        liveness.record_message_from_neighbour(peer);

        // Past the window that message opened it is silent, not back to
        // settling: the node has watched it long enough to conclude something.
        connected_for(&mut liveness, &peer, WINDOW_ROUNDS.get());
        assert_eq!(liveness.current_peer_state(&peer), Some(PeerState::Silent));
    }

    #[test]
    fn rounds_spent_disconnected_do_not_count_against_a_neighbour() {
        let (mut liveness, peer, other) = (liveness(), PeerId::random(), PeerId::random());
        liveness.start_or_resume_observing(peer);

        // It is connected for part of its window, then drops.
        connected_for(&mut liveness, &peer, WINDOW_ROUNDS.get() - 1);

        // Many windows pass with only the other neighbour connected.
        for _ in 0..(10 * WINDOW_ROUNDS.get()) {
            liveness.enter_new_round_with_peers(core::iter::once(&other));
        }

        // It comes back, and resumes with the one round of grace it had left
        // rather than being silent on arrival. Closing it here, and on every
        // reconnection after, is what would keep two honest nodes from ever
        // re-peering.
        liveness.start_or_resume_observing(peer);
        assert_eq!(
            liveness.current_peer_state(&peer),
            Some(PeerState::Settling)
        );
    }

    #[test]
    fn reconnecting_does_not_earn_a_fresh_window() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer);
        connected_for(&mut liveness, &peer, WINDOW_ROUNDS.get() - 1);

        // The neighbour reconnects just before its window runs out, without
        // ever having delivered anything.
        liveness.start_or_resume_observing(peer);

        connected_for(&mut liveness, &peer, 1);
        assert_eq!(liveness.current_peer_state(&peer), Some(PeerState::Silent));
    }

    #[test]
    fn clearing_forgets_every_observation() {
        let (mut liveness, peer) = (liveness(), PeerId::random());
        liveness.start_or_resume_observing(peer);
        connected_for(&mut liveness, &peer, WINDOW_ROUNDS.get());
        assert_eq!(liveness.current_peer_state(&peer), Some(PeerState::Silent));

        liveness.clear();

        assert_eq!(liveness.current_peer_state(&peer), None);
    }
}
