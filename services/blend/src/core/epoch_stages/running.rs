use core::hash::Hash;

use futures::StreamExt as _;
use lb_blend::{
    provers::crypto::EncapsulatedMessageWithVerifiedPublicHeader,
    scheduling::{EpochMessageScheduler, message_scheduler::round_info::RoundInfo},
};
use lb_chain_service::Epoch;

use crate::{
    core::{
        CoreEpochPublicInfo, CoreLeaderAndPowProofsGenerator,
        CurrentEpochCryptographicProcessor as CryptoProcessor, OldEpochCryptographicProcessor,
        encapsulate_next_local_message,
        epoch_stages::{OldEpochScheduler, transitioning::TransitioningEpoch},
    },
    message::ProcessedMessage,
    pending::{EncapsulationResult, PendingProposals, PendingTransactions},
};

type MessageScheduler<Rng> =
    EpochMessageScheduler<Rng, ProcessedMessage, EncapsulatedMessageWithVerifiedPublicHeader>;

/// What scheduling a locally-encapsulated message borrows: see
/// [`CurrentEpoch::scheduling_borrows`].
type SchedulingBorrows<'a, NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng> = (
    &'a mut PendingProposals,
    &'a CryptoProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    &'a mut MessageScheduler<Rng>,
);

/// What decapsulating an incoming message borrows while an epoch is
/// transitioning: see [`CurrentEpochDuringTransition::decapsulation_borrows`].
type DecapsulationBorrows<'a, NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng> = (
    &'a mut MessageScheduler<Rng>,
    &'a mut OldEpochScheduler<Rng>,
    &'a CryptoProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    &'a OldEpochCryptographicProcessor<ProofsVerifier>,
);

/// What a rotation consumes: see [`CurrentEpoch::into_components`].
pub type Components<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng> = (
    CryptoProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    MessageScheduler<Rng>,
    CoreEpochPublicInfo<NodeId>,
);
type Round = RoundInfo<ProcessedMessage, EncapsulatedMessageWithVerifiedPublicHeader>;

/// The epoch the node is blending under: what it needs to encapsulate, release
/// and account for messages minted right now.
pub struct CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng> {
    crypto: CryptoProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    scheduler: MessageScheduler<Rng>,
    epoch_info: CoreEpochPublicInfo<NodeId>,
    proposals: PendingProposals,
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
    CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
{
    pub const fn new(
        crypto: CryptoProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
        scheduler: MessageScheduler<Rng>,
        epoch_info: CoreEpochPublicInfo<NodeId>,
    ) -> Self {
        let proposals = PendingProposals::new(epoch_info.epoch);
        Self {
            crypto,
            scheduler,
            epoch_info,
            proposals,
        }
    }

    pub const fn proposals_mut(&mut self) -> &mut PendingProposals {
        &mut self.proposals
    }

    pub const fn crypto_processor_mut(
        &mut self,
    ) -> &mut CryptoProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier> {
        &mut self.crypto
    }

    pub const fn epoch_info(&self) -> &CoreEpochPublicInfo<NodeId> {
        &self.epoch_info
    }

    /// The two parts the paths that schedule a locally-encapsulated message
    /// need at once: they read the processor for its epoch and write the
    /// message into the scheduler. Borrowing them through one method keeps them
    /// disjoint.
    pub const fn scheduling_borrows(
        &mut self,
    ) -> SchedulingBorrows<'_, NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng> {
        (&mut self.proposals, &self.crypto, &mut self.scheduler)
    }

    /// The two parts decapsulating an incoming message minted under *this*
    /// epoch needs. Borrowing them through one method keeps them disjoint.
    pub const fn decapsulation_borrows(
        &mut self,
    ) -> (
        &mut MessageScheduler<Rng>,
        &CryptoProcessor<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier>,
    ) {
        (&mut self.scheduler, &self.crypto)
    }

    pub fn into_components(
        self,
    ) -> Components<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng> {
        (self.crypto, self.scheduler, self.epoch_info)
    }
}

/// Something the current epoch produced on its own, as opposed to something
/// that arrived from outside it.
pub enum CurrentEpochEvent {
    /// A queued message now has proofs behind it, or never will.
    ///
    /// Never [`EncapsulationResult::Retry`]: that one means "nothing to act
    /// on", and
    /// is what disables the branch below rather than a value to report.
    Encapsulated(EncapsulationResult),
    /// This epoch's scheduler says it is time to release.
    ReleaseRound(Round),
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
    CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
where
    NodeId: Eq + Hash + 'static,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
    Rng: rand::Rng + Clone + Unpin,
{
    /// Races this epoch's own two sources and reports whichever is ready first.
    ///
    /// Borrows rather than consumes, so the caller's `select!` may drop this
    /// future — which it does whenever an outside event wins the race. That is
    /// safe for the same reason it was safe when these were separate branches,
    /// and for the same reasons: nothing here commits before its await. The
    /// queue is only read, a partial proof draw is left on the encapsulator
    /// rather than in this frame, and the scheduler keeps its timers.
    pub async fn next_event(&mut self, transactions: &PendingTransactions) -> CurrentEpochEvent {
        let proposals = &self.proposals;
        let crypto = &mut self.crypto;
        let scheduler = &mut self.scheduler;

        tokio::select! {
            Some(encapsulation) = encapsulate_next_local_message(proposals, transactions, crypto) => {
                CurrentEpochEvent::Encapsulated(encapsulation)
            }
            Some(round_info) = scheduler.next() => CurrentEpochEvent::ReleaseRound(round_info),
        }
    }
}

/// The current epoch, while the epoch before it is still being drained.
///
/// A previous epoch is live for exactly one window — its transition period —
/// and outside that window there is none at all. Holding it as an `Option`
/// made every use restate that: the release branch had to be a
/// `match`-in-an-`async`-block yielding `None` to disable itself, and the
/// decapsulation path handed back two `Option`s that were always both `Some`
/// or both `None`. Here the window *is* the type, so the branch that drains the
/// previous epoch exists only where there is something to drain — and the stage
/// without one, [`CurrentEpoch`], has no way to name it at all.
///
/// The current epoch is a field rather than repeated, so the two stages cannot
/// drift apart.
pub struct CurrentEpochDuringTransition<
    NodeId,
    CorePoQGenerator,
    ProofsGenerator,
    ProofsVerifier,
    Rng,
> {
    current: CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>,
    previous: TransitioningEpoch<Rng, ProofsVerifier>,
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
    CurrentEpochDuringTransition<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
{
    pub const fn new(
        current: CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>,
        previous: TransitioningEpoch<Rng, ProofsVerifier>,
    ) -> Self {
        Self { current, previous }
    }

    pub const fn current_mut(
        &mut self,
    ) -> &mut CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng> {
        &mut self.current
    }

    /// The four halves the incoming-message path needs at once: a message names
    /// the epoch it was minted under, and only one of the two epochs can
    /// decapsulate it. Borrowing them through one method keeps them disjoint.
    pub const fn decapsulation_borrows(
        &mut self,
    ) -> DecapsulationBorrows<'_, NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
    {
        let (previous_crypto, previous_scheduler) = self.previous.split_mut();
        (
            &mut self.current.scheduler,
            previous_scheduler,
            &self.current.crypto,
            previous_crypto,
        )
    }

    pub const fn previous_epoch(&self) -> Epoch {
        self.previous.epoch()
    }

    /// Gives up the epoch being drained, leaving the current one whole —
    /// proposals included, because a transition period ending is not an epoch
    /// change. What the drained epoch still held is dropped: past its
    /// transition period no peer would accept what it releases, having rotated
    /// its verifier on.
    pub fn end_transition(
        self,
    ) -> CurrentEpoch<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng> {
        self.current
    }

    /// Splits into the parts a rotation consumes. The epoch being drained goes
    /// with them: a rotation replaces it with the epoch that has just ended,
    /// and one older than that has no peers left that would accept it.
    pub fn into_components(
        self,
    ) -> Components<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng> {
        self.current.into_components()
    }
}

/// The same, plus what the epoch being drained produced.
pub enum DuringTransitionEvent {
    Current(CurrentEpochEvent),
    /// The transitioning epoch's scheduler says it is time to release, for the
    /// epoch it names — every message it releases is published under that one,
    /// so it reaches the peers still negotiated for it.
    PreviousEpochReleaseRound(Round, Epoch),
}

impl<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
    CurrentEpochDuringTransition<NodeId, CorePoQGenerator, ProofsGenerator, ProofsVerifier, Rng>
where
    NodeId: Eq + Hash + 'static,
    ProofsGenerator: CoreLeaderAndPowProofsGenerator<CorePoQGenerator>,
    Rng: rand::Rng + Clone + Unpin,
{
    /// Races both epochs' own sources and reports whichever is ready first.
    ///
    /// Cancel-safe on the same terms as [`CurrentEpoch::next_event`], which it
    /// wraps.
    pub async fn next_event(
        &mut self,
        transactions: &PendingTransactions,
    ) -> DuringTransitionEvent {
        let previous_epoch = self.previous.epoch();
        let current = &mut self.current;
        let previous = &mut self.previous;

        tokio::select! {
            event = current.next_event(transactions) => DuringTransitionEvent::Current(event),
            Some(round_info) = previous.scheduler_mut().next() => {
                DuringTransitionEvent::PreviousEpochReleaseRound(round_info, previous_epoch)
            }
        }
    }
}
