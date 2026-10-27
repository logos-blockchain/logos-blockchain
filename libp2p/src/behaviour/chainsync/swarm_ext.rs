use std::collections::HashSet;

use lb_cryptarchia_sync::{BoxedStream, ChainSyncError, HeaderId, SerialisedBlock, TipResponse};
use libp2p::PeerId;
use rand::RngCore;
use tokio::sync::oneshot;

use crate::{Swarm, behaviour::BehaviourError, protocol_name::StreamProtocol};

type SerialisedBlockStream = BoxedStream<Result<SerialisedBlock, ChainSyncError>>;

impl<R: Clone + Send + RngCore + 'static> Swarm<R> {
    /// Starts a new era of chain sync, over the connections already open: every
    /// request goes over `protocol_name` from now on, and the protocol of the
    /// era before is refused, while the requests already under way on it run
    /// until [`Self::finish_chain_sync_era_transition`].
    pub fn start_new_chain_sync_era(&mut self, protocol_name: StreamProtocol) {
        self.swarm
            .behaviour_mut()
            .chain_sync
            .start_new_era(protocol_name.into());
    }

    /// Drops the chain sync requests still under way over the protocol of the
    /// era before.
    pub fn finish_chain_sync_era_transition(&mut self) {
        self.swarm
            .behaviour_mut()
            .chain_sync
            .finish_era_transition();
    }

    pub fn request_tip(
        &self,
        peer_id: PeerId,
        reply_sender: oneshot::Sender<Result<TipResponse, ChainSyncError>>,
    ) -> Result<(), BehaviourError> {
        let chain_sync = &self.swarm.behaviour().chain_sync;

        chain_sync
            .request_tip(peer_id, reply_sender)
            .map_err(Into::into)
    }

    pub fn request_blocks(
        &self,
        peer_id: PeerId,
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: HashSet<HeaderId>,
        reply_sender: oneshot::Sender<SerialisedBlockStream>,
    ) -> Result<(), BehaviourError> {
        let chain_sync = &self.swarm.behaviour().chain_sync;

        chain_sync
            .request_blocks(
                peer_id,
                target_block,
                local_tip,
                latest_immutable_block,
                additional_blocks,
                reply_sender,
            )
            .map_err(Into::into)
    }
}
