use std::{collections::HashSet, fmt::Debug};

use lb_libp2p::{
    PeerId,
    cryptarchia_sync::{BoxedStream, ChainSyncError, GetTipResponse, HeaderId, SerialisedBlock},
};
use rand::RngCore;
use tokio::sync::oneshot;

use crate::{backends::libp2p::swarm::SwarmHandler, message::ChainSyncEvent};

type SerialisedBlockStream = BoxedStream<Result<SerialisedBlock, ChainSyncError>>;

pub enum ChainSyncCommand {
    RequestTip {
        peer: PeerId,
        reply_sender: oneshot::Sender<Result<GetTipResponse, ChainSyncError>>,
    },
    DownloadBlocks {
        peer: PeerId,
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: HashSet<HeaderId>,
        reply_sender: oneshot::Sender<SerialisedBlockStream>,
    },
}

impl Debug for ChainSyncCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequestTip { peer, .. } => {
                f.debug_struct("RequestTip").field("peer", peer).finish()
            }
            Self::DownloadBlocks {
                peer,
                target_block,
                local_tip,
                latest_immutable_block,
                additional_blocks,
                ..
            } => f
                .debug_struct("DownloadBlocks")
                .field("peer", peer)
                .field("target_block", target_block)
                .field("local_tip", local_tip)
                .field("latest_immutable_block", latest_immutable_block)
                .field("additional_blocks", additional_blocks)
                .finish(),
        }
    }
}

impl<R: Clone + Send + RngCore + 'static> SwarmHandler<R> {
    pub(super) fn handle_chainsync_command(&self, command: ChainSyncCommand) {
        match command {
            ChainSyncCommand::RequestTip { peer, reply_sender } => {
                if let Err(e) = self.swarm.request_tip(peer, reply_sender) {
                    tracing::error!("failed to request tip: {e:?}");
                }
            }
            ChainSyncCommand::DownloadBlocks {
                peer,
                target_block,
                local_tip,
                latest_immutable_block,
                additional_blocks,
                reply_sender,
            } => {
                if let Err(e) = self.swarm.start_blocks_download(
                    peer,
                    target_block,
                    local_tip,
                    latest_immutable_block,
                    additional_blocks,
                    reply_sender,
                ) {
                    tracing::error!("failed to request blocks download: {e:?}");
                }
            }
        }
    }

    pub(super) fn handle_chainsync_event(&self, event: lb_cryptarchia_sync::Event) {
        let event = ChainSyncEvent::from(event);
        if let Err(e) = self.chainsync_events_tx.send(event) {
            tracing::error!("failed to send chainsync event: {e:?}");
        }
    }
}

// Convert libp2p specific type to a common type.
impl From<lb_cryptarchia_sync::Event> for ChainSyncEvent {
    fn from(event: lb_cryptarchia_sync::Event) -> Self {
        match event {
            lb_cryptarchia_sync::Event::ProvideBlocksRequest {
                target_block,
                local_tip,
                latest_immutable_block,
                additional_blocks,
                reply_sender,
                peer_id,
            } => Self::ProvideBlocksRequest {
                target_block,
                local_tip,
                latest_immutable_block,
                additional_blocks,
                reply_sender,
                peer_id,
            },
            lb_cryptarchia_sync::Event::ProvideTipsRequest {
                reply_sender,
                peer_id,
            } => Self::ProvideTipRequest {
                reply_sender,
                peer_id,
            },
        }
    }
}
