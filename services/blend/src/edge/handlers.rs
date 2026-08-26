use std::{hash::Hash, marker::PhantomData, sync::Arc};

use lb_blend::{
    message::{
        Error as MessageError, crypto::proofs::PoQVerificationInputsMinusSigningKey,
        encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    },
    scheduling::{
        membership::Membership,
        message_blend::{
            crypto::leader::send::EpochCryptographicProcessor,
            provers::{WinningPolInfoStream, leader_and_pow::LeaderAndPowProofsGenerator},
        },
    },
};
use lb_chain_service::Epoch;
use lb_utils::blake_rng::BlakeRng;
use overwatch::overwatch::OverwatchHandle;
use rand::SeedableRng as _;

use crate::edge::{RunningSettings as Settings, backends::BlendBackend};

pub struct MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId> {
    cryptographic_processor: EpochCryptographicProcessor<NodeId, ProofsGenerator>,
    backend: Backend,
    _phantom: PhantomData<RuntimeServiceId>,
}

impl<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
    MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone,
    ProofsGenerator: LeaderAndPowProofsGenerator,
{
    #[cfg(test)]
    pub const fn epoch(&self) -> Epoch {
        self.cryptographic_processor.epoch()
    }
}

impl<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
    MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
where
    Backend: BlendBackend<NodeId, RuntimeServiceId>,
    NodeId: Clone + Send + 'static,
    ProofsGenerator: LeaderAndPowProofsGenerator,
{
    /// Creates a [`MessageHandler`] with the given membership.
    ///
    /// It returns [`Error`] if the membership does not satisfy the following
    /// edge node condition:
    /// 1. The membership size is at least `settings.minimum_network_size`.
    /// 2. The local node is not a core node.
    pub fn try_new_with_edge_condition_check(
        settings: Settings<Backend, NodeId, RuntimeServiceId>,
        membership: Membership<NodeId>,
        public_info: PoQVerificationInputsMinusSigningKey,
        winning_pol_info_stream: WinningPolInfoStream,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        epoch: Epoch,
    ) -> Result<Self, Error>
    where
        NodeId: Eq + Hash,
    {
        let membership_size = membership.size();
        if membership_size < settings.minimum_network_size.get() as usize {
            Err(Error::NetworkIsTooSmall(membership_size))
        } else if membership.contains_local() {
            Err(Error::LocalIsCoreNode)
        } else {
            Ok(Self::new(
                settings,
                membership,
                public_info,
                winning_pol_info_stream,
                overwatch_handle,
                epoch,
            ))
        }
    }

    fn new(
        settings: Settings<Backend, NodeId, RuntimeServiceId>,
        membership: Membership<NodeId>,
        public_info: PoQVerificationInputsMinusSigningKey,
        winning_pol_info_stream: WinningPolInfoStream,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        epoch: Epoch,
    ) -> Self {
        let pow_mining_pool = Arc::clone(&settings.pow_mining_pool);
        let cryptographic_processor = EpochCryptographicProcessor::new(
            settings.num_blend_layers,
            membership.clone(),
            public_info,
            winning_pol_info_stream,
            epoch,
            pow_mining_pool,
        );
        let backend = Backend::new(
            settings.backend,
            overwatch_handle,
            membership,
            BlakeRng::from_entropy(),
            settings.non_ephemeral_signing_key,
        );
        Self {
            cryptographic_processor,
            backend,
            _phantom: PhantomData,
        }
    }
}

impl<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
    MessageHandler<Backend, NodeId, ProofsGenerator, RuntimeServiceId>
where
    NodeId: Eq + Hash + Clone + Send + 'static,
    Backend: BlendBackend<NodeId, RuntimeServiceId> + Sync,
    ProofsGenerator: LeaderAndPowProofsGenerator,
{
    /// Encapsulate a block proposal, spending leadership quota on its layer
    /// proofs.
    ///
    /// A failure is handed back raw for
    /// [`resolve_encapsulation`](crate::pending::resolve_encapsulation) to
    /// judge, since only it knows whether the message stays queued. Leadership
    /// proofs are not simply there for the asking: they need this epoch's
    /// secret `PoL` info, which can land after the first proposal does.
    pub async fn encapsulate_block_proposal(
        &mut self,
        proposal: &[u8],
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, MessageError> {
        self.cryptographic_processor
            .encapsulate_block_proposal_payload(proposal)
            .await
    }

    /// Encapsulate a transaction, whose layer proofs are backed by a proof of
    /// work.
    ///
    /// Those proofs come from a puzzle search, so the caller has to be
    /// somewhere it can afford to wait. A failure is handed back raw, as above.
    pub async fn encapsulate_transaction(
        &mut self,
        transaction: &[u8],
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, MessageError> {
        self.cryptographic_processor
            .encapsulate_transaction_payload(transaction)
            .await
    }

    /// Hand a finished message to the backend.
    ///
    /// Kept apart from the encapsulation that produced it so the caller can put
    /// this where cancellation cannot reach it — see
    /// [`send_local_encapsulated_message`](super::send_local_encapsulated_message).
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "The exclusive borrow is what keeps this future `Send` without a `Sync` bound."
    )]
    pub async fn send(&mut self, message: EncapsulatedMessageWithVerifiedPublicHeader) {
        self.backend.send(message).await;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Network is too small: {0}")]
    NetworkIsTooSmall(usize),
    #[error("Local node is a core node")]
    LocalIsCoreNode,
}
