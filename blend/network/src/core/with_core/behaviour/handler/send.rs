use std::collections::VecDeque;

use lb_blend_primitives::time::{Round, RoundCount};

use crate::{OutgoingMessage, core::admission::RoundShare};

/// What a connection may put on the wire right now.
pub enum PollOutcome {
    /// The message to send.
    Message(OutgoingMessage),
    /// Messages are waiting, but this connection has already carried its share
    /// for the round.
    ShareSpent,
}

impl PollOutcome {
    #[cfg(test)]
    fn take_message(self) -> Option<OutgoingMessage> {
        match self {
            Self::Message(message) => Some(message),
            Self::ShareSpent => None,
        }
    }
}

/// A message waiting for its turn on a connection.
struct MessageEntry {
    message: OutgoingMessage,
    /// The first round this connection gives up on the message in, by which
    /// point it has waited the `η` rounds the specification allows it.
    expires_at: Round,
}

/// What a connection still owes its neighbour, and the share it may send under.
pub struct SendQueue {
    queue: VecDeque<MessageEntry>,
    /// `η`: how long a message may wait for this connection.
    lifetime: RoundCount,
    share: RoundShare,
}

impl SendQueue {
    #[must_use]
    pub const fn new(share_per_round: RoundShare, lifetime: RoundCount) -> Self {
        Self {
            queue: VecDeque::new(),
            lifetime,
            share: share_per_round,
        }
    }

    /// Adds a message to what this connection owes its neighbour.
    pub fn enqueue(&mut self, message: OutgoingMessage, current_round: Round) {
        self.queue.push_back(MessageEntry {
            message,
            expires_at: current_round.saturating_add(self.lifetime),
        });
    }

    /// Refreshes the share and gives up on whatever has waited too long,
    /// reporting how many messages that was.
    pub fn enter_new_round_and_refresh_shares(&mut self, new_round: Round) -> usize {
        self.share.refresh(new_round);

        // The queue is FIFO and every message is given the same lifetime, so deadlines
        // are non-decreasing and the expired ones are exactly the prefix.
        let mut discarded = 0usize;
        while self
            .queue
            .front()
            .is_some_and(|queued| new_round >= queued.expires_at)
        {
            self.queue.pop_front();
            discarded = discarded.checked_add(1).unwrap();
        }
        discarded
    }

    /// The next message to put on the wire, if there is one and the share
    /// allows it.
    ///
    /// An empty queue and a spent share are both "nothing to send now", but
    /// only one of them is the protocol throttling a connection that has more
    /// to say, and that one is worth reporting. Telling them apart is the
    /// caller's only way to know which it is.
    pub fn pop_front(&mut self) -> Option<PollOutcome> {
        if self.queue.is_empty() {
            return None;
        }
        if !self.share.try_spend() {
            return Some(PollOutcome::ShareSpent);
        }
        self.queue
            .pop_front()
            .map(|queued| PollOutcome::Message(queued.message))
    }

    pub fn clear(&mut self) {
        self.queue.clear();
    }
}

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU64, NonZeroU128};

    use lb_blend_primitives::time::{Round, RoundCount};

    use crate::{
        OutgoingMessage,
        core::with_core::behaviour::handler::send::{PollOutcome, RoundShare, SendQueue},
    };

    const SHARE: NonZeroU64 = NonZeroU64::new(3).unwrap();
    const LIFETIME: RoundCount = RoundCount::new(NonZeroU128::new(2).unwrap());

    fn payload(byte: u8) -> OutgoingMessage {
        OutgoingMessage::from_bytes([byte])
    }

    #[test]
    fn a_share_is_spent_at_most_once_per_message() {
        let mut share = RoundShare::new(SHARE, Round::from(0));

        for _ in 0..SHARE.get() {
            assert!(share.try_spend());
        }

        assert!(!share.try_spend(), "the share for this round is spent");
    }

    #[test]
    fn a_share_refreshes_once_per_round_however_often_it_is_asked() {
        let mut share = RoundShare::new(SHARE, Round::from(0));
        assert!(share.try_spend());

        // Being told about the same round again must not refill it.
        share.refresh(Round::from(0));
        assert!(share.try_spend());
        assert!(share.try_spend());
        assert!(!share.try_spend());

        share.refresh(Round::from(1));
        assert!(share.try_spend());
    }

    #[test]
    fn a_skipped_round_refreshes_the_share_exactly_once() {
        let mut share = RoundShare::new(SHARE, Round::from(0));
        while share.try_spend() {}

        // Ten rounds pass without the share being refreshed. It comes back to
        // one round's worth, not ten.
        share.refresh(Round::from(10));

        for _ in 0..SHARE.get() {
            assert!(share.try_spend());
        }
        assert!(!share.try_spend());
    }

    #[test]
    fn the_queue_sends_at_most_a_round_s_share() {
        let mut queue = SendQueue::new(RoundShare::new(SHARE, Round::from(0)), LIFETIME);
        for byte in 0..5 {
            queue.enqueue(payload(byte), Round::from(0));
        }

        for byte in 0..u8::try_from(SHARE.get()).unwrap() {
            assert_eq!(
                queue
                    .pop_front()
                    .unwrap()
                    .take_message()
                    .map(|msg| msg.as_ref().to_vec()),
                Some(vec![byte])
            );
        }
        assert!(
            matches!(queue.pop_front(), Some(PollOutcome::ShareSpent)),
            "the share is spent, which is not the same as having nothing to send"
        );

        queue.enter_new_round_and_refresh_shares(Round::from(1));
        assert_eq!(
            queue
                .pop_front()
                .unwrap()
                .take_message()
                .map(|msg| msg.as_ref().to_vec()),
            Some(vec![3])
        );
    }

    #[test]
    fn a_message_that_waits_longer_than_its_lifetime_is_given_up_on() {
        let mut queue = SendQueue::new(RoundShare::new(SHARE, Round::from(0)), LIFETIME);
        queue.enqueue(payload(0), Round::from(0));

        // Still within the lifetime: the message is kept.
        assert_eq!(queue.enter_new_round_and_refresh_shares(Round::from(1)), 0);
        queue.enqueue(payload(1), Round::from(1));

        // Past it: the first message is dropped, the second is not, since it
        // joined the queue later.
        assert_eq!(queue.enter_new_round_and_refresh_shares(Round::from(2)), 1);
        assert_eq!(
            queue
                .pop_front()
                .unwrap()
                .take_message()
                .map(|msg| msg.as_ref().to_vec()),
            Some(vec![1])
        );
    }
}
