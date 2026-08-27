//! The epoch this node is blending under, and what belongs to it alone.

use core::{hash::Hash, num::NonZeroU64};

use lb_blend::{
    membership::Membership,
    message::{
        crypto::proofs::PoQVerificationInputsMinusSigningKey,
        encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    },
    proofs::quota::{
        inputs::prove::public::{CoreInputs, LeaderInputs, PowInputs},
        pow::PowTarget,
    },
    scheduling::message_blend::provers::leader_and_pow::LeaderAndPowProofsGenerator,
};
use lb_chain_service::Epoch;
use lb_groth16::Fr;
use overwatch::overwatch::OverwatchHandle;
use tracing::debug;

use crate::{
    edge::{
        LOG_TARGET, RunningSettings,
        backends::BlendBackend,
        handlers::{Error, MessageHandler},
    },
    epoch_info::PolEpochInfo,
    membership::{MembershipInfo, ZkInfo, chain::BlendEpochState},
    pending::{
        EncapsulationResult, MessageKind, NextLocalMessage, PendingProposals, PendingTransactions,
        next_local_message, resolve_encapsulation,
    },
};

/// Which of the two this node is in for the epoch it is on.
pub enum CurrentEpoch<Backend, NodeId, ProofsGenerator, RuntimeServiceId> {
    AwaitingSecretInfo(AwaitingSecretInfo<NodeId>),
    Blending(Blending<Backend, NodeId, ProofsGenerator, RuntimeServiceId>),
}

/// The epoch this node is blending under, before its secret `PoL` info has
/// arrived.
///
/// It can take proposals but not mint anything: leadership proofs need that
/// info, and this is the window a queued proposal waits out. Everything here is
/// replaced when the epoch turns and nothing here outlives it, which is what
/// decides membership — queued proposals belong because one is built for a slot
/// in this epoch, so a rotation makes it worthless. Transactions are not
/// slot-bound and are held outside, by whatever outlives epochs.
pub struct AwaitingSecretInfo<NodeId> {
    info: ValidBlendEpochState<NodeId>,
    proposals: PendingProposals,
}

/// Epoch state for a valid Blend session (i.e., membership above the minimum
/// network size).
#[derive(Clone, Debug)]
pub struct ValidBlendEpochState<NodeId> {
    pub epoch: Epoch,
    pub nonce: Fr,
    pub aged: Fr,
    pub lottery_0: Fr,
    pub lottery_1: Fr,
    pub pow_difficulty: PowTarget,
    pub membership_info: ValidMembershipInfo<NodeId>,
}

#[derive(Clone, Debug)]
pub struct ValidMembershipInfo<NodeId> {
    pub membership: Membership<NodeId>,
    pub zk: ZkInfo,
}

/// The same epoch, once both halves are in and a handler exists to mint for it.
///
/// The handler is a field rather than an `Option` on one type, so the methods
/// that need one — encapsulating and sending — exist only where there is one.
pub struct Blending<Backend, NodeId, ProofsGenerator, RuntimeServiceId> {
    awaiting: AwaitingSecretInfo<NodeId>,
    handler: MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>,
}

impl<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
    CurrentEpoch<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
{
    const fn awaiting_mut(&mut self) -> &mut AwaitingSecretInfo<NodeId> {
        match self {
            Self::AwaitingSecretInfo(awaiting) | Self::Blending(Blending { awaiting, .. }) => {
                awaiting
            }
        }
    }

    /// Queues a proposal to be sent `copies` times, to go out under this epoch
    /// or not at all.
    pub fn queue_proposal(&mut self, proposal: Vec<u8>, copies: NonZeroU64) {
        self.awaiting_mut().proposals.queue(proposal, copies);
    }

    pub const fn proposals_mut(&mut self) -> &mut PendingProposals {
        &mut self.awaiting_mut().proposals
    }

    const fn awaiting(&self) -> &AwaitingSecretInfo<NodeId> {
        match self {
            Self::AwaitingSecretInfo(awaiting) | Self::Blending(Blending { awaiting, .. }) => {
                awaiting
            }
        }
    }

    const fn awaiting_epoch(&self) -> Epoch {
        self.awaiting().info.epoch
    }

    #[cfg(test)]
    pub const fn info(&self) -> &ValidBlendEpochState<NodeId> {
        &self.awaiting().info
    }

    #[cfg(test)]
    pub const fn has_handler(&self) -> bool {
        matches!(self, Self::Blending(_))
    }
}

impl<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
    CurrentEpoch<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone + Send + Eq + Hash + 'static,
    ProofsGenerator: LeaderAndPowProofsGenerator,
{
    /// A new epoch, which by definition has no handler yet: one needs secret
    /// `PoL` info that has not been matched to it.
    ///
    /// Fails when the membership says this node has no business being an edge
    /// node of this epoch, which shuts the service down. Doing it here is what
    /// makes the rest of this type unconditional: a `CurrentEpoch` that exists
    /// is one whose membership was accepted, so nothing downstream has to ask
    /// again — and the handler, which used to re-check the same two conditions,
    /// no longer does.
    pub fn try_new(
        BlendEpochState {
            aged,
            epoch,
            lottery_0,
            lottery_1,
            membership_info: MembershipInfo { membership, zk },
            nonce,
            pow_difficulty,
        }: BlendEpochState<NodeId>,
        settings: &RunningSettings<Backend, NodeId, RuntimeServiceId>,
    ) -> Result<Self, Error> {
        let Some(zk_info) = zk else {
            return Err(Error::NetworkIsTooSmall(0));
        };

        let membership_size = membership.size();
        if membership_size < settings.minimum_network_size.get() as usize {
            return Err(Error::NetworkIsTooSmall(membership_size));
        }
        if membership.contains_local() {
            return Err(Error::LocalIsCoreNode);
        }

        Ok(Self::AwaitingSecretInfo(AwaitingSecretInfo {
            info: ValidBlendEpochState {
                epoch,
                nonce,
                aged,
                lottery_0,
                lottery_1,
                pow_difficulty,
                membership_info: ValidMembershipInfo {
                    membership,
                    zk: zk_info,
                },
            },
            proposals: PendingProposals::new(),
        }))
    }

    /// This epoch with the handler it can have, given what secret `PoL` info is
    /// to hand.
    ///
    /// The stash outlives epochs — info can arrive for one this node has not
    /// reached — so it is only drawn from when it names *this* epoch, and left
    /// alone otherwise.
    pub fn with_available_secret_info(
        self,
        stashed_secret_info: &mut Option<PolEpochInfo>,
        settings: RunningSettings<Backend, NodeId, RuntimeServiceId>,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Self {
        let epoch = self.awaiting_epoch();
        if let Some(secret_epoch_info) =
            stashed_secret_info.take_if(|stashed| stashed.epoch == epoch)
        {
            return self.with_secret_info(secret_epoch_info, settings, overwatch_handle);
        }

        // Nothing to do, and nothing to undo: an epoch this node has not
        // reached having secret info stashed for it says nothing about the one
        // it is on, which keeps whatever handler it had.
        debug!(target: LOG_TARGET, "No secret PoL info for epoch {epoch:?} yet, leaving this epoch as it is.");
        self
    }

    /// This epoch with the handler its secret `PoL` info makes possible.
    ///
    /// The info must be *this* epoch's; matching it against the epoch is the
    /// caller's job, since the caller is what holds one that has not found its
    /// epoch yet. Infallible: the only thing that could have gone wrong was the
    /// membership, and a `CurrentEpoch` cannot exist unless that was accepted.
    ///
    /// Any handler this epoch already had goes with it. That is the point of
    /// consuming `self`: a fresh secret means a fresh winning-`PoL` stream, so
    /// the old handler is no longer the right one. Everything else travels —
    /// its queued proposals above all — because secret info arriving is not an
    /// epoch change.
    pub fn with_secret_info(
        self,
        secret_epoch_info: PolEpochInfo,
        settings: RunningSettings<Backend, NodeId, RuntimeServiceId>,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Self {
        let awaiting = match self {
            Self::AwaitingSecretInfo(awaiting) | Self::Blending(Blending { awaiting, .. }) => {
                awaiting
            }
        };
        debug_assert_eq!(
            secret_epoch_info.epoch, awaiting.info.epoch,
            "Secret `PoL` info must belong to the epoch it is being attached to."
        );

        let new_public_inputs = PoQVerificationInputsMinusSigningKey {
            core: CoreInputs {
                quota: settings.cover.epoch_core_quota(
                    settings.num_blend_layers,
                    &settings.time,
                    awaiting.info.membership_info.membership.size(),
                ),
                zk_root: awaiting.info.membership_info.zk.root,
            },
            leader: LeaderInputs {
                lottery_0: awaiting.info.lottery_0,
                lottery_1: awaiting.info.lottery_1,
                pol_epoch_nonce: awaiting.info.nonce,
                pol_ledger_aged: awaiting.info.aged,
                message_quota: settings.epoch_leadership_quota(),
            },
            pow: PowInputs {
                pow_blend_difficulty: awaiting.info.pow_difficulty,
                pow_quota: settings.epoch_pow_quota(),
            },
        };

        debug!(target: LOG_TARGET, "Creating new handler for epoch {:?}", awaiting.info.epoch);
        let handler = MessageHandler::new(
            settings,
            awaiting.info.membership_info.membership.clone(),
            new_public_inputs,
            secret_epoch_info.winning_pol_info_stream,
            overwatch_handle,
            awaiting.info.epoch,
        );

        Self::Blending(Blending { awaiting, handler })
    }
}

impl<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
    Blending<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Sync,
    NodeId: Clone + core::fmt::Debug + Eq + Hash + Send + Sync + 'static,
    ProofsGenerator: LeaderAndPowProofsGenerator + Send,
{
    /// Encapsulates one locally-originated message, once proofs back it.
    ///
    /// Proposals go first; see [`next_local_message`]. Neither queue is popped
    /// here: `select!` drops this future whenever another branch wins the race,
    /// and one that popped before awaiting would take the message down with it
    /// every time that happened. The caller updates the queues once the race is
    /// settled, which is also why only one copy of a proposal is wrapped per
    /// call.
    async fn encapsulate_next_local_message(
        &mut self,
        transactions: &PendingTransactions,
    ) -> Option<EncapsulationResult> {
        let (kind, encapsulated) = match next_local_message(&self.awaiting.proposals, transactions)?
        {
            NextLocalMessage::ProposalCopy(proposal) => (
                MessageKind::Proposal,
                self.handler.encapsulate_block_proposal(proposal).await,
            ),
            NextLocalMessage::Transaction(transaction) => (
                MessageKind::Transaction,
                self.handler.encapsulate_transaction(transaction).await,
            ),
        };

        match resolve_encapsulation(encapsulated, kind) {
            // Not something the caller acts on, and handing it back would be
            // worse than useless: the branch this feeds would then complete
            // immediately every time round with nothing to do, which is a busy
            // loop rather than a wait. `None` fails the branch's pattern and
            // disables it instead.
            EncapsulationResult::Retry => None,
            acted_on => Some(acted_on),
        }
    }

    /// Hands a finished message to the backend.
    ///
    /// Apart from the encapsulation that produced it because this is where the
    /// message leaves the node, and `select!` drops the losing branch futures:
    /// a send cancelled midway would bin the proofs backing that
    /// encapsulation while the payload stayed queued. Handler bodies are
    /// never cancelled.
    async fn send(&mut self, message: EncapsulatedMessageWithVerifiedPublicHeader) {
        self.handler.send(message).await;
    }
}

impl<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
    CurrentEpoch<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Sync,
    NodeId: Clone + core::fmt::Debug + Eq + Hash + Send + Sync + 'static,
    ProofsGenerator: LeaderAndPowProofsGenerator + Send,
{
    /// Encapsulates one locally-originated message, if this epoch can mint at
    /// all yet.
    ///
    /// `None` when it cannot — no handler, nothing queued, or no proofs — which
    /// is what leaves the `select!` branch free to wait on the others.
    pub async fn encapsulate_next_local_message(
        &mut self,
        transactions: &PendingTransactions,
    ) -> Option<EncapsulationResult> {
        match self {
            Self::AwaitingSecretInfo(_) => None,
            Self::Blending(blending) => blending.encapsulate_next_local_message(transactions).await,
        }
    }

    /// Hands a finished message to the backend.
    ///
    /// # Panics
    ///
    /// If this epoch has no handler, which cannot happen: a message only exists
    /// because that handler encapsulated it a moment ago, and nothing runs in
    /// between that could take it away.
    pub async fn send(&mut self, message: EncapsulatedMessageWithVerifiedPublicHeader) {
        match self {
            Self::Blending(blending) => blending.send(message).await,
            Self::AwaitingSecretInfo(_) => {
                unreachable!("A message can only have been encapsulated by this epoch's handler.")
            }
        }
    }
}
