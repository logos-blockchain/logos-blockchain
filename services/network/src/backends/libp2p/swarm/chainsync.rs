use std::{collections::HashSet, fmt::Debug};

use lb_libp2p::{
    PeerId,
    cryptarchia_sync::{BoxedStream, ChainSyncError, HeaderId, SerialisedBlock, TipResponse},
};
use lb_log_targets::network_service;
use rand::RngCore;
use tokio::sync::{broadcast::error::SendError, oneshot};

use crate::{backends::libp2p::swarm::SwarmHandler, message::ChainSyncEvent};

const LOG_TARGET: &str = network_service::backends::libp2p::CHAINSYNC;

type SerialisedBlockStream = BoxedStream<Result<SerialisedBlock, ChainSyncError>>;

pub enum ChainSyncCommand {
    EligiblePeers {
        reply_sender: oneshot::Sender<HashSet<PeerId>>,
    },
    RequestTip {
        peer: PeerId,
        reply_sender: oneshot::Sender<Result<TipResponse, ChainSyncError>>,
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
            Self::EligiblePeers { .. } => f.debug_struct("EligiblePeers").finish(),
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
    #[expect(
        clippy::cognitive_complexity,
        reason = "The command handler keeps all chainsync command dispatch in one place."
    )]
    pub(super) fn handle_chainsync_command(&self, command: ChainSyncCommand) {
        match command {
            ChainSyncCommand::EligiblePeers { reply_sender } => {
                log_error!(reply_sender.send(self.chainsync_eligible_peers()));
            }
            ChainSyncCommand::RequestTip { peer, reply_sender } => {
                if let Err(e) = self.swarm.request_tip(peer, reply_sender) {
                    tracing::error!(target: LOG_TARGET, "failed to request tip: {e:?}");
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
                if let Err(e) = self.swarm.request_blocks(
                    peer,
                    target_block,
                    local_tip,
                    latest_immutable_block,
                    additional_blocks,
                    reply_sender,
                ) {
                    tracing::error!(
                        target: LOG_TARGET,
                        "failed to request blocks download: {e:?}"
                    );
                }
            }
        }
    }

    pub(super) fn handle_chainsync_event(&self, event: lb_cryptarchia_sync::Event) {
        let event = ChainSyncEvent::from(event);
        if let Err(SendError(event)) = self.chainsync_events_tx.send(event) {
            tracing::debug!(
                target: LOG_TARGET,
                ?event,
                "no chainsync event subscriber yet; dropping the event"
            );
        }
    }
}

// Convert libp2p specific type to a common type.
impl From<lb_cryptarchia_sync::Event> for ChainSyncEvent {
    fn from(event: lb_cryptarchia_sync::Event) -> Self {
        match event {
            lb_cryptarchia_sync::Event::BlocksRequest {
                target_block,
                requester_tip,
                latest_immutable_block,
                additional_blocks,
                reply_sender,
            } => Self::BlocksRequest {
                target_block,
                requester_tip,
                latest_immutable_block,
                additional_blocks,
                reply_sender,
            },
            lb_cryptarchia_sync::Event::TipRequest { reply_sender } => {
                Self::TipRequest { reply_sender }
            }
        }
    }
}
