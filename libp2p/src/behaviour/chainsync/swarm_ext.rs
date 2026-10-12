use std::collections::HashSet;

use lb_cryptarchia_sync::{BoxedStream, ChainSyncError, GetTipResponse, HeaderId, SerialisedBlock};
use libp2p::PeerId;
use rand::RngCore;
use tokio::sync::oneshot;

use crate::{Swarm, behaviour::BehaviourError, protocol_name::StreamProtocol};

type SerialisedBlockStream = BoxedStream<Result<SerialisedBlock, ChainSyncError>>;

impl<R: Clone + Send + RngCore + 'static> Swarm<R> {
    /// Speaks `protocol_name` for chain sync from now on, with a chain sync
    /// started from scratch over the connections already open.
    pub fn set_new_chain_sync_protocol(&mut self, protocol_name: StreamProtocol) {
        self.swarm
            .behaviour_mut()
            .chain_sync
            .switch_to_protocol(protocol_name.into());
    }

    pub fn request_tip(
        &self,
        peer_id: PeerId,
        reply_sender: oneshot::Sender<Result<GetTipResponse, ChainSyncError>>,
    ) -> Result<(), BehaviourError> {
        let chain_sync = &self.swarm.behaviour().chain_sync;

        chain_sync
            .request_tip(peer_id, reply_sender)
            .map_err(Into::into)
    }

    pub fn start_blocks_download(
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
            .start_blocks_download(
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
