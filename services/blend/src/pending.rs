//! Locally-originated messages waiting for the proofs that will back them.

use core::num::NonZeroU64;
use std::collections::VecDeque;

use lb_blend::message::{
    Error as MessageError, encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
};

use crate::LOG_TARGET;

/// One locally-originated message, encapsulated and ready for the wire.
///
/// Which queue it came from, so the caller knows what to mark as sent once it
/// is actually out. Both modes produce this; what differs is only the processor
/// that made it.
pub enum LocalEncapsulation {
    ProposalCopy(EncapsulatedMessageWithVerifiedPublicHeader),
    Transaction(EncapsulatedMessageWithVerifiedPublicHeader),
}

/// Turns one encapsulation attempt into what the event loop should do about it.
///
/// `None` means "nothing to do this time round": the branch backing this
/// message has no proofs yet, and it stays queued to be retried. `Err(())`
/// means the head can never be encapsulated and has to go — the head is
/// retried before anything else is looked at, so one that keeps failing would
/// take everything queued behind it down with it.
///
/// The failure is resolved here rather than handed back because a
/// [`MessageError`] is not `Send`, and a `select!` branch output has to be.
pub fn resolve_encapsulation<WrapFn>(
    encapsulated: Result<EncapsulatedMessageWithVerifiedPublicHeader, MessageError>,
    wrap: WrapFn,
) -> Option<Result<LocalEncapsulation, ()>>
where
    WrapFn: FnOnce(EncapsulatedMessageWithVerifiedPublicHeader) -> LocalEncapsulation,
{
    match encapsulated {
        Ok(message) => Some(Ok(wrap(message))),
        Err(error) => match report_encapsulation_failure(&error) {
            Verdict::Retry => None,
            Verdict::Discard => Some(Err(())),
        },
    }
}

/// Whether a message that failed to encapsulate is worth keeping.
enum Verdict {
    /// A failure that says "not yet": leave the message queued.
    Retry,
    /// A failure that will repeat however long the message waits: drop it.
    Discard,
}

/// Logs a failed encapsulation at the level it deserves, and says whether the
/// message should stay queued.
///
/// Running out of proofs is the ordinary "not yet" — the branch backing this
/// message has nothing to give until, say, the epoch's secret `PoL` info lands
/// — and the message is retried every time the loop turns, so reporting it as
/// an error would bury the log in noise while waiting. Everything else is a
/// genuine failure, and a permanent one: a payload too large to fit will not
/// shrink, so retrying it forever would block everything queued behind it.
fn report_encapsulation_failure(error: &MessageError) -> Verdict {
    if matches!(error, MessageError::ProofNotAvailable) {
        tracing::trace!(target: LOG_TARGET, "No proofs available yet to encapsulate a message. Leaving it queued.");
        Verdict::Retry
    } else {
        tracing::error!(target: LOG_TARGET, "Dropping a message that cannot be encapsulated: {error:?}");
        Verdict::Discard
    }
}

/// What [`PendingLocalMessages::next`] would have the caller work on.
#[derive(Debug, PartialEq, Eq)]
pub enum NextLocalMessage<'a> {
    /// One copy of the longest-waiting block proposal.
    ProposalCopy(&'a [u8]),
    /// The longest-waiting transaction.
    Transaction(&'a [u8]),
}

/// What [`PendingLocalMessages::discard_head`] dropped, handed back so a caller
/// that persists its queue can drop it there too.
#[derive(Debug, PartialEq, Eq)]
pub enum Discarded {
    Proposal(Vec<u8>),
    Transaction(Vec<u8>),
}

/// The queue of messages this node has produced but not yet put on the wire.
///
/// Both modes need the same bookkeeping. A message might not be sent the moment
/// it arrives: a transaction waits on a `PoW` solution, and a proposal waits on
/// this epoch's secret `PoL` info, which could land *after* the first
/// proposal does. Whatever cannot go yet stays here and is retried.
///
/// Nothing is removed by looking at it. The branch that does the sending is a
/// `select!` arm, so its future is dropped whenever another branch wins the
/// race, and a queue that popped before awaiting would lose the message every
/// time that happened. The caller reports what actually went out instead, once
/// the race is settled.
///
/// Queued proposals do not survive an epoch change; queued transactions do.
/// A proposal is built for one slot, and leadership quota is one message's
/// worth per winning slot, so a proposal still waiting when the epoch turns
/// would spend the quota that the *new* epoch's block needs. Losing the block
/// this node is about to produce is worse than losing one it failed to send in
/// time. A transaction is not slot-bound and its `PoW` branch is redrawn
/// anyway, so it stays.
#[derive(Debug)]
pub struct PendingLocalMessages {
    /// Each proposal with the copies it still owes. Proposals are replicated;
    /// transactions are not.
    proposals: VecDeque<(Vec<u8>, NonZeroU64)>,
    transactions: VecDeque<Vec<u8>>,
}

impl Default for PendingLocalMessages {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingLocalMessages {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            proposals: VecDeque::new(),
            transactions: VecDeque::new(),
        }
    }

    /// Queues a proposal to be sent `copies` times.
    pub fn queue_proposal(&mut self, proposal: Vec<u8>, copies: NonZeroU64) {
        self.proposals.push_back((proposal, copies));
    }

    pub fn queue_transaction(&mut self, transaction: Vec<u8>) {
        self.transactions.push_back(transaction);
    }

    /// The next message to try, or `None` when there is nothing waiting.
    ///
    /// Proposals go first: one is tied to the slot it was built for and goes
    /// stale, whereas a transaction keeps.
    #[must_use]
    pub fn next(&self) -> Option<NextLocalMessage<'_>> {
        if let Some((proposal, _)) = self.proposals.front() {
            return Some(NextLocalMessage::ProposalCopy(proposal));
        }
        self.transactions
            .front()
            .map(|transaction| NextLocalMessage::Transaction(transaction))
    }

    /// Records that one copy of the head proposal went out, dropping it once it
    /// owes no more.
    ///
    /// # Panics
    ///
    /// If no proposal is queued, which would mean a copy was reported for a
    /// message this queue never handed out.
    pub fn mark_proposal_copy_as_sent(&mut self) {
        let Some((_, remaining_copies)) = self.proposals.front_mut() else {
            panic!("A proposal copy was reported sent, but none is queued");
        };
        let copies_before = *remaining_copies;
        // If the copy sent was the last one, drop the proposal from the queue.
        let Some(copies_after) = NonZeroU64::new(copies_before.get() - 1) else {
            drop(self.proposals.pop_front());
            return;
        };
        *remaining_copies = copies_after;
    }

    /// Records that the head transaction went out, handing it back so a caller
    /// that persists its queue can drop it there too.
    ///
    /// # Panics
    ///
    /// If no transaction is queued.
    pub fn mark_transaction_as_sent(&mut self) -> Vec<u8> {
        self.transactions
            .pop_front()
            .expect("A transaction was reported sent, but none is queued")
    }

    /// Drops every queued proposal, returning how many went, for a caller whose
    /// epoch has turned over. Transactions are left alone.
    ///
    /// See the note on this type for why proposals cannot outlive the epoch
    /// they were queued in.
    pub fn discard_proposals(&mut self) -> usize {
        let discarded = self.proposals.len();
        self.proposals.clear();
        discarded
    }

    /// Drops the message [`Self::next`] would hand out, for a caller that has
    /// found it can never be sent — one too large to fit a payload, say, as
    /// opposed to one merely waiting on proofs.
    ///
    /// Without this a message like that would sit at the head forever and take
    /// everything queued behind it down with it, since the head is retried
    /// before anything else is looked at. A proposal goes in full, outstanding
    /// copies and all: every copy would fail the same way.
    pub fn discard_head(&mut self) -> Option<Discarded> {
        if let Some((proposal, _)) = self.proposals.pop_front() {
            return Some(Discarded::Proposal(proposal));
        }
        self.transactions.pop_front().map(Discarded::Transaction)
    }

    /// The transactions still waiting, oldest first.
    #[must_use]
    pub fn transactions(&self) -> impl ExactSizeIterator<Item = &Vec<u8>> {
        self.transactions.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proposal_goes_before_a_transaction() {
        let mut pending = PendingLocalMessages::new();
        pending.queue_transaction(b"tx".to_vec());
        pending.queue_proposal(b"proposal".to_vec(), 1.try_into().unwrap());

        assert_eq!(
            pending.next(),
            Some(NextLocalMessage::ProposalCopy(b"proposal")),
            "a proposal is slot-bound and stales; a transaction keeps"
        );
    }

    #[test]
    fn a_proposal_stays_until_every_copy_is_out() {
        let mut pending = PendingLocalMessages::new();
        pending.queue_proposal(b"proposal".to_vec(), 3.try_into().unwrap());
        pending.queue_transaction(b"tx".to_vec());

        for _ in 0..3 {
            assert_eq!(
                pending.next(),
                Some(NextLocalMessage::ProposalCopy(b"proposal"))
            );
            pending.mark_proposal_copy_as_sent();
        }

        assert_eq!(
            pending.next(),
            Some(NextLocalMessage::Transaction(b"tx")),
            "the transaction is reached only once the proposal owes nothing"
        );
    }

    #[test]
    fn looking_does_not_consume() {
        let mut pending = PendingLocalMessages::new();
        pending.queue_transaction(b"tx".to_vec());

        // However often a `select!` arm is built and dropped without sending,
        // the message is still there.
        for _ in 0..3 {
            assert_eq!(pending.next(), Some(NextLocalMessage::Transaction(b"tx")));
        }
        assert_eq!(pending.mark_transaction_as_sent(), b"tx".to_vec());
        assert_eq!(pending.next(), None);
    }

    #[test]
    fn a_message_that_can_never_be_sent_does_not_block_the_rest() {
        let mut pending = PendingLocalMessages::new();
        pending.queue_proposal(b"proposal".to_vec(), 3.try_into().unwrap());
        pending.queue_transaction(b"tx".to_vec());

        assert_eq!(
            pending.discard_head(),
            Some(Discarded::Proposal(b"proposal".to_vec())),
            "outstanding copies go with it: they would all fail the same way"
        );
        assert_eq!(
            pending.next(),
            Some(NextLocalMessage::Transaction(b"tx")),
            "what was queued behind it must get its turn"
        );
    }

    #[test]
    fn an_epoch_change_takes_the_proposals_and_leaves_the_transactions() {
        let mut pending = PendingLocalMessages::new();
        pending.queue_proposal(b"proposal".to_vec(), 3.try_into().unwrap());
        pending.queue_transaction(b"tx".to_vec());

        assert_eq!(
            pending.discard_proposals(),
            1,
            "outstanding copies go with it: they would all spend the new epoch's quota"
        );
        assert_eq!(
            pending.next(),
            Some(NextLocalMessage::Transaction(b"tx")),
            "a transaction is not slot-bound and outlives the epoch it arrived in"
        );
    }
}
