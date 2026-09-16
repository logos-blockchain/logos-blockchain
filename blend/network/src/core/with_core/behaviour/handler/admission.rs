use std::collections::VecDeque;

use lb_blend_primitives::time::{Round, RoundCount};

use crate::{OutgoingMessage, core::admission::RoundShare};

/// A message waiting for its turn on a connection.
struct MessageEntry {
    message: OutgoingMessage,
    /// The round after which this connection gives up on the message.
    last_round_before_expiry: Round,
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
    ///
    /// Infallible: whether the wire format can carry the message was settled
    /// when the [`OutgoingMessage`] was built, so a queue cannot be the place
    /// that discovers it cannot.
    pub fn enqueue(&mut self, message: OutgoingMessage, current_round: Round) {
        self.queue.push_back(MessageEntry {
            message,
            last_round_before_expiry: current_round.saturating_add(self.lifetime),
        });
    }

    /// Refreshes the share and gives up on whatever has waited too long,
    /// reporting how many messages that was.
    pub fn enter_round(&mut self, new_round: Round) -> usize {
        self.share.refill_for(new_round);

        // The queue is FIFO and every message is given the same lifetime, so deadlines
        // are non-decreasing and the expired ones are exactly the prefix.
        let mut discarded = 0usize;
        while self
            .queue
            .front()
            .is_some_and(|queued| new_round.rounds_since(queued.last_round_before_expiry) > 0)
        {
            self.queue.pop_front();
            discarded = discarded.checked_add(1).unwrap();
        }
        discarded
    }

    /// The next message to put on the wire, if there is one and the share
    /// allows it.
    pub fn pop_front(&mut self) -> Option<OutgoingMessage> {
        if self.queue.is_empty() || !self.share.try_spend() {
            return None;
        }
        self.queue.pop_front().map(|queued| queued.message)
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
        core::with_core::behaviour::handler::admission::{RoundShare, SendQueue},
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
        share.refill_for(Round::from(0));
        assert!(share.try_spend());
        assert!(share.try_spend());
        assert!(!share.try_spend());

        share.refill_for(Round::from(1));
        assert!(share.try_spend());
    }

    #[test]
    fn a_skipped_round_refreshes_the_share_exactly_once() {
        let mut share = RoundShare::new(SHARE, Round::from(0));
        while share.try_spend() {}

        // Ten rounds pass without the share being refreshed. It comes back to
        // one round's worth, not ten.
        share.refill_for(Round::from(10));

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
                queue.pop_front().map(|msg| msg.as_ref().to_vec()),
                Some(vec![byte])
            );
        }
        assert!(queue.pop_front().is_none(), "the share is spent");

        queue.enter_round(Round::from(1));
        assert_eq!(
            queue.pop_front().map(|msg| msg.as_ref().to_vec()),
            Some(vec![3])
        );
    }

    #[test]
    fn a_message_that_waits_longer_than_its_lifetime_is_given_up_on() {
        let mut queue = SendQueue::new(RoundShare::new(SHARE, Round::from(0)), LIFETIME);
        queue.enqueue(payload(0), Round::from(0));

        // Still within the lifetime: the message is kept.
        assert_eq!(queue.enter_round(Round::from(2)), 0);
        queue.enqueue(payload(1), Round::from(2));

        // Past it: the first message is dropped, the second is not, since it
        // joined the queue later.
        assert_eq!(queue.enter_round(Round::from(3)), 1);
        assert_eq!(
            queue.pop_front().map(|msg| msg.as_ref().to_vec()),
            Some(vec![1])
        );
    }
}
