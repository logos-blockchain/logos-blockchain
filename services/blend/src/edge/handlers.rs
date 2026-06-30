use std::{hash::Hash, marker::PhantomData};

use lb_blend::{
    message::crypto::proofs::PoQVerificationInputsMinusSigningKey,
    scheduling::{
        membership::Membership,
        message_blend::{
            crypto::leader::send::EpochCryptographicProcessor,
            provers::{WinningPolInfoStream, leader::LeaderProofsGenerator},
        },
    },
};
use lb_chain_service::Epoch;
use lb_utils::blake_rng::BlakeRng;
use overwatch::overwatch::OverwatchHandle;
use rand::SeedableRng as _;

use crate::edge::{LOG_TARGET, RunningSettings as Settings, backends::BlendBackend};

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
    ProofsGenerator: LeaderProofsGenerator,
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
    ProofsGenerator: LeaderProofsGenerator,
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
        let cryptographic_processor = EpochCryptographicProcessor::new(
            settings.num_blend_layers,
            membership.clone(),
            public_info,
            winning_pol_info_stream,
            epoch,
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
    ProofsGenerator: LeaderProofsGenerator,
{
    /// Blend a new message received from another service.
    pub async fn handle_message_to_blend(&mut self, message: Vec<u8>) {
        let Ok(message) = self
            .cryptographic_processor
            .encapsulate_data_payload(&message)
            .await
            .inspect_err(|e| {
                tracing::error!(target: LOG_TARGET, "Failed to encapsulate message: {e:?}");
            })
        else {
            return;
        };
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
