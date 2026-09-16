use core::{
    error,
    fmt::{self, Display, Formatter},
    num::NonZeroUsize,
};
use std::collections::VecDeque;

use lb_blend_primitives::time::{Round, RoundCount};
use libp2p::PeerId;

use crate::core::with_core::error::ReceiveError;

/// Why a peer was blacklisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlacklistReason {
    /// The bytes did not decode into a message of the expected shape.
    UndeserializableMessage,
    /// The public header carried a signature that does not verify.
    InvalidHeaderSignature,
    /// The sender could not prove it had the quota to send the message.
    InvalidProofOfQuota,
}

impl From<ReceiveError> for BlacklistReason {
    fn from(value: ReceiveError) -> Self {
        match value {
            ReceiveError::UndeserializableMessage => Self::UndeserializableMessage,
            ReceiveError::InvalidHeaderSignature => Self::InvalidHeaderSignature,
        }
    }
}

impl AsRef<str> for BlacklistReason {
    fn as_ref(&self) -> &str {
        match self {
            Self::UndeserializableMessage => "undeserializable_message",
            Self::InvalidHeaderSignature => "invalid_header_signature",
            Self::InvalidProofOfQuota => "invalid_proof_of_quota",
        }
    }
}

impl Display for BlacklistReason {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_ref())
    }
}

impl error::Error for BlacklistReason {}

#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub peer: PeerId,
    pub reason: BlacklistReason,
    /// The round at which the entry is removed from the blacklist.
    pub expires_at: Round,
}

/// The peers this node refuses to exchange Blend messages with, for a while.
///
/// Instead of pro-actively clearing expired entries, the blacklist maintains
/// full capacity and start overriding oldest entries. This is fine for small
/// values of `capacity`, as it does not justify introducing a complex
/// round-based machinery to clean up expired entries.
#[derive(Debug)]
pub struct PeerBlacklist {
    /// Ordered oldest-first, which is both the eviction order and the expiry
    /// order: rounds only move forward, so an entry added later can never
    /// expire earlier.
    entries: VecDeque<Entry>,
    /// How many peers may be blacklisted at once. Reaching it evicts the oldest
    /// entry.
    capacity: NonZeroUsize,
    /// `W`: how long an entry lasts before the peer may be dialed and accepted
    /// again.
    expiry: RoundCount,
}

impl PeerBlacklist {
    #[must_use]
    pub fn new(capacity: NonZeroUsize, expiry: RoundCount) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity.get()),
            capacity,
            expiry,
        }
    }

    /// Blacklists a peer, or refreshes the entry of one already blacklisted.
    ///
    /// Refreshing moves the peer to the back, so re-offending both restarts its
    /// expiry and makes it the last to be evicted rather than the first.
    pub fn insert_or_extend(&mut self, peer: PeerId, reason: BlacklistReason, now: Round) {
        if let Some(position) = self.entries.iter().position(|entry| entry.peer == peer) {
            self.entries.remove(position);
        } else if self.entries.len() >= self.capacity.get() {
            self.entries.pop_front();
        }
        self.entries.push_back(Entry {
            peer,
            reason,
            expires_at: now.saturating_add(self.expiry),
        });
    }

    /// The peers blacklisted as of `now`.
    pub fn entries(&self, now: Round) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(move |entry| is_unexpired(entry, now))
    }

    /// Why the peer is blacklisted as of `now`, if it is.
    #[must_use]
    pub fn reason(&self, peer: &PeerId, now: Round) -> Option<BlacklistReason> {
        self.entries(now)
            .find(|entry| entry.peer == *peer)
            .map(|entry| entry.reason)
    }

    /// Whether the peer is blacklisted as of `now`.
    #[must_use]
    pub fn contains(&self, peer: &PeerId, now: Round) -> bool {
        self.reason(peer, now).is_some()
    }

    /// Drops entries that have outlived their expiry.
    pub fn prune_expired_entries(&mut self, now: Round) {
        while self
            .entries
            .front()
            .is_some_and(|entry| !is_unexpired(entry, now))
        {
            self.entries.pop_front();
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Whether the entry still has rounds left as of `now`.
const fn is_unexpired(entry: &Entry, now: Round) -> bool {
    now.inner() < entry.expires_at.inner()
}

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU128, NonZeroUsize};
    use std::iter::repeat_with;

    use lb_blend_primitives::time::{Round, RoundCount};
    use libp2p::PeerId;

    use crate::core::with_core::behaviour::blacklist::{BlacklistReason, PeerBlacklist};

    const CAPACITY: NonZeroUsize = NonZeroUsize::new(8).unwrap();
    const EXPIRY_ROUNDS: NonZeroU128 = NonZeroU128::new(30).unwrap();

    fn blacklist() -> PeerBlacklist {
        PeerBlacklist::new(CAPACITY, RoundCount::new(EXPIRY_ROUNDS))
    }

    fn round(round: u128) -> Round {
        Round::from(round)
    }

    #[test]
    fn a_blacklisted_peer_is_excluded_until_its_entry_expires() {
        let (mut blacklist, peer) = (blacklist(), PeerId::random());
        blacklist.insert_or_extend(peer, BlacklistReason::InvalidProofOfQuota, round(10));

        assert!(blacklist.contains(&peer, round(10)));
        assert!(
            blacklist.contains(&peer, round(10 + EXPIRY_ROUNDS.get() - 1)),
            "the last round of the window still excludes the peer"
        );
        assert!(
            !blacklist.contains(&peer, round(10 + EXPIRY_ROUNDS.get())),
            "`W` rounds after the offence the peer may be dialed again"
        );
    }

    #[test]
    fn an_unknown_peer_is_never_excluded() {
        assert!(!blacklist().contains(&PeerId::random(), round(0)));
        assert_eq!(blacklist().reason(&PeerId::random(), round(0)), None);
    }

    #[test]
    fn re_offending_restarts_the_window_without_adding_an_entry() {
        let (mut blacklist, peer) = (blacklist(), PeerId::random());
        blacklist.insert_or_extend(peer, BlacklistReason::UndeserializableMessage, round(0));
        blacklist.insert_or_extend(peer, BlacklistReason::InvalidHeaderSignature, round(20));

        assert_eq!(
            blacklist.len(),
            1,
            "the same peer must not occupy two slots"
        );
        assert_eq!(
            blacklist.reason(&peer, round(20)),
            Some(BlacklistReason::InvalidHeaderSignature),
            "the latest reason is the one kept"
        );
        assert!(
            blacklist.contains(&peer, round(EXPIRY_ROUNDS.get() + 10)),
            "the window runs from the second offence, not the first"
        );
    }

    #[test]
    fn filling_the_blacklist_evicts_the_oldest_entry() {
        let mut blacklist = blacklist();
        let peers: Vec<_> = repeat_with(PeerId::random).take(CAPACITY.get()).collect();
        for (offset, peer) in peers.iter().enumerate() {
            blacklist.insert_or_extend(
                *peer,
                BlacklistReason::InvalidProofOfQuota,
                round(offset as u128),
            );
        }
        assert_eq!(blacklist.len(), CAPACITY.get());

        let newcomer = PeerId::random();
        blacklist.insert_or_extend(newcomer, BlacklistReason::InvalidProofOfQuota, round(10));

        assert_eq!(blacklist.len(), CAPACITY.get(), "the cap is never exceeded");
        assert!(
            !blacklist.contains(&peers[0], round(10)),
            "the oldest entry made room"
        );
        assert!(blacklist.contains(&peers[1], round(10)));
        assert!(blacklist.contains(&newcomer, round(10)));
    }

    #[test]
    fn expiring_reclaims_room_without_changing_who_is_excluded() {
        let (mut blacklist, old, recent) = (blacklist(), PeerId::random(), PeerId::random());
        blacklist.insert_or_extend(old, BlacklistReason::InvalidProofOfQuota, round(0));
        blacklist.insert_or_extend(recent, BlacklistReason::InvalidProofOfQuota, round(20));

        let now = round(EXPIRY_ROUNDS.get() + 1);
        // The answer is the same before and after the sweep: `expire` only
        // reclaims room, it never decides who is excluded.
        assert!(!blacklist.contains(&old, now));
        assert!(blacklist.contains(&recent, now));

        blacklist.prune_expired_entries(now);

        assert_eq!(blacklist.len(), 1);
        assert!(!blacklist.contains(&old, now));
        assert!(blacklist.contains(&recent, now));
    }

    #[test]
    fn iterating_lists_only_unexpired_entries() {
        let (mut blacklist, old, recent) = (blacklist(), PeerId::random(), PeerId::random());
        blacklist.insert_or_extend(old, BlacklistReason::InvalidProofOfQuota, round(0));
        blacklist.insert_or_extend(recent, BlacklistReason::InvalidProofOfQuota, round(20));

        let now = round(EXPIRY_ROUNDS.get() + 1);
        let listed: Vec<_> = blacklist.entries(now).map(|entry| entry.peer).collect();

        assert_eq!(listed, vec![recent]);
    }
}
