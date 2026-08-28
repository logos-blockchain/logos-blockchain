//! Locally-originated messages waiting for the proofs that will back them.

use core::num::NonZeroU64;
use std::collections::VecDeque;

use lb_blend::message::{
    Error as MessageError, encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
};
use lb_chain_service::Epoch;

use crate::LOG_TARGET;

/// Turns one encapsulation attempt into what the event loop should do about it.
///
/// The failure is resolved here rather than handed back because a
/// [`MessageError`] is not `Send`, and a `select!` branch output has to be.
#[must_use]
pub fn resolve_encapsulation(
    encapsulated: Result<EncapsulatedMessageWithVerifiedPublicHeader, MessageError>,
    kind: MessageKind,
) -> EncapsulationResult {
    match encapsulated {
        Ok(message) => {
            EncapsulationResult::Complete(Box::new(LocalEncapsulation { message, kind }))
        }
        Err(MessageError::ProofNotAvailable) => {
            tracing::trace!(target: LOG_TARGET, "No proofs available yet to encapsulate a message. Leaving it queued.");
            EncapsulationResult::Retry
        }
        Err(error) => {
            tracing::error!(target: LOG_TARGET, "Dropping a message that cannot be encapsulated: {error:?}");
            EncapsulationResult::Discard(kind)
        }
    }
}

/// What the event loop should do about one encapsulation attempt.
pub enum EncapsulationResult {
    /// It worked, and this is ready for the wire.
    Complete(Box<LocalEncapsulation>),
    /// Not this time: the branch backing this message has no proofs yet, so it
    /// stays queued and is tried again next time.
    Retry,
    /// Never: the head of this queue can never be encapsulated, so it goes. The
    /// head is retried before anything else is looked at, so one that keeps
    /// failing would take everything queued behind it down with it.
    Discard(MessageKind),
}

/// An encapsulated message of a specific kind.
pub struct LocalEncapsulation {
    pub message: EncapsulatedMessageWithVerifiedPublicHeader,
    pub kind: MessageKind,
}

/// Which of the two kinds a locally-originated message is.
///
/// The two are queued apart, because only one of them is bound to the epoch it
/// arrived in, so anything that acts on "whichever was next" has to say which
/// queue it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    Proposal,
    Transaction,
}

/// What [`next_local_message`] would have the caller work on.
#[derive(Debug, PartialEq, Eq)]
pub enum NextLocalMessage<'a> {
    /// One copy of the longest-waiting block proposal.
    ProposalCopy(&'a [u8]),
    /// The longest-waiting transaction.
    Transaction(&'a [u8]),
}

/// The next locally-originated message to try, or `None` when nothing is
/// waiting.
///
/// The one statement of the rule that proposals go first: one is tied to the
/// slot it was built for and goes stale, whereas a transaction keeps. The two
/// queues are owned separately — a proposal by the epoch it belongs to, a
/// transaction by whatever outlives epochs — so the rule that spans them lives
/// here rather than in either.
///
/// Nothing is removed by looking. The branch that does the sending is a
/// `select!` arm, so its future is dropped whenever another branch wins the
/// race, and a queue that popped before awaiting would lose the message every
/// time that happened. The caller reports what actually went out instead, once
/// the race is settled.
#[must_use]
pub fn next_local_message<'a>(
    proposals: &'a PendingProposals,
    transactions: &'a PendingTransactions,
) -> Option<NextLocalMessage<'a>> {
    proposals.head().map_or_else(
        || transactions.head().map(NextLocalMessage::Transaction),
        |proposal| Some(NextLocalMessage::ProposalCopy(proposal)),
    )
}

/// Block proposals this node has produced but not yet put on the wire, each
/// with the copies it still owes.
///
/// **Bound to the epoch it was queued in.** A proposal is built for one slot,
/// and leadership quota is one message's worth per winning slot, so a proposal
/// still waiting when the epoch turns would spend the quota that the *new*
/// epoch's block needs.
#[derive(Debug)]
pub struct PendingProposals {
    epoch: Epoch,
    queued: VecDeque<(Vec<u8>, NonZeroU64)>,
}

impl PendingProposals {
    #[must_use]
    pub const fn new(epoch: Epoch) -> Self {
        Self {
            epoch,
            queued: VecDeque::new(),
        }
    }

    /// Queues a proposal to be sent `copies` times.
    pub fn queue(&mut self, proposal: Vec<u8>, copies: NonZeroU64) {
        self.queued.push_back((proposal, copies));
    }

    fn head(&self) -> Option<&[u8]> {
        self.queued.front().map(|(proposal, _)| proposal.as_slice())
    }

    /// Records that one copy of the head went out, dropping it once it owes no
    /// more.
    ///
    /// # Panics
    ///
    /// If none is queued, which would mean a copy was reported for a message
    /// this queue never handed out.
    pub fn mark_copy_as_sent(&mut self) {
        let Some((_, remaining_copies)) = self.queued.front_mut() else {
            panic!("A proposal copy was reported sent, but none is queued");
        };
        match NonZeroU64::new(remaining_copies.get() - 1) {
            // The copy sent was the last one it owed.
            None => drop(self.queued.pop_front()),
            Some(left) => *remaining_copies = left,
        }
    }

    /// Drops the head, for a caller that has found it can never be sent — one
    /// too large to fit a payload, say, as opposed to one merely waiting on
    /// proofs.
    pub fn discard_head(&mut self) {
        drop(self.queued.pop_front());
    }
}

impl Drop for PendingProposals {
    fn drop(&mut self) {
        let dropping = self.queued.len();
        if dropping > 0 {
            tracing::warn!(
                target: LOG_TARGET,
                "Dropping {dropping} block proposal(s) queued under epoch {:?} without sending them.",
                self.epoch
            );
        }
    }
}

/// Transactions this node has accepted but not yet put on the wire.
///
/// **Not bound to any epoch.** A transaction stays valid however long it waits,
/// its `PoW` branch is redrawn under whichever epoch is current when it finally
/// goes, and it is persisted so it survives a restart. So it is owned by
/// something that outlives epochs, not by one of them.
#[derive(Debug, Default)]
pub struct PendingTransactions(VecDeque<Vec<u8>>);

impl PendingTransactions {
    #[must_use]
    pub const fn new() -> Self {
        Self(VecDeque::new())
    }

    pub fn queue(&mut self, transaction: Vec<u8>) {
        self.0.push_back(transaction);
    }

    fn head(&self) -> Option<&[u8]> {
        self.0.front().map(Vec::as_slice)
    }

    /// Records that the head went out, handing it back so a caller that
    /// persists this queue can drop it there too.
    ///
    /// # Panics
    ///
    /// If none is queued.
    pub fn mark_as_sent(&mut self) -> Vec<u8> {
        self.0
            .pop_front()
            .expect("A transaction was reported sent, but none is queued")
    }

    /// Drops the head, for a caller that has found it can never be sent, and
    /// hands it back so the caller can drop it from the recovery state too. See
    /// [`PendingProposals::discard_head`] for why the head cannot simply stay.
    pub fn discard_head(&mut self) -> Option<Vec<u8>> {
        self.0.pop_front()
    }

    /// Those still waiting, oldest first.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Vec<u8>> {
        self.0.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(copies: u64) -> PendingProposals {
        let mut proposals = PendingProposals::new(Epoch::new(1));
        proposals.queue(b"proposal".to_vec(), copies.try_into().unwrap());
        proposals
    }

    fn transaction() -> PendingTransactions {
        let mut transactions = PendingTransactions::new();
        transactions.queue(b"tx".to_vec());
        transactions
    }

    #[test]
    fn a_proposal_goes_before_a_transaction() {
        assert_eq!(
            next_local_message(&proposal(1), &transaction()),
            Some(NextLocalMessage::ProposalCopy(b"proposal")),
            "a proposal is slot-bound and stales; a transaction keeps"
        );
    }

    #[test]
    fn a_proposal_stays_until_every_copy_is_out() {
        let mut proposals = proposal(3);
        let transactions = transaction();
        for _ in 0..3 {
            assert_eq!(
                next_local_message(&proposals, &transactions),
                Some(NextLocalMessage::ProposalCopy(b"proposal"))
            );
            proposals.mark_copy_as_sent();
        }
        assert_eq!(
            next_local_message(&proposals, &transactions),
            Some(NextLocalMessage::Transaction(b"tx")),
            "once it owes no more copies the transaction behind it gets its turn"
        );
    }

    #[test]
    fn looking_does_not_consume() {
        let proposals = proposal(1);
        let transactions = transaction();
        assert_eq!(
            next_local_message(&proposals, &transactions),
            next_local_message(&proposals, &transactions),
            "the sending branch is dropped and rebuilt, so looking must not pop"
        );
    }

    #[test]
    fn a_message_that_can_never_be_sent_does_not_block_the_rest() {
        let mut proposals = proposal(3);
        let transactions = transaction();

        proposals.discard_head();
        assert_eq!(
            next_local_message(&proposals, &transactions),
            Some(NextLocalMessage::Transaction(b"tx")),
            "outstanding copies go with it, and what was queued behind gets its turn"
        );
    }
}
