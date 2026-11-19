use std::{collections::HashSet, fmt::Debug};

use lb_cryptarchia_sync::{BlocksUnavailableReason, ProviderResponse, TipResponseFailureReason};
use lb_libp2p::{
    PeerId,
    cryptarchia_sync::{BoxedStream, ChainSyncError, HeaderId, SerialisedBlock, TipResponse},
};
use lb_log_targets::network_service;
use rand::RngCore;
use tokio::sync::{
    mpsc::{self, error::TrySendError},
    oneshot,
};

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
    /// Hand the chain sync requests to `events_tx`, in place of whoever had
    /// them.
    SetSubscriber {
        events_tx: mpsc::Sender<ChainSyncEvent>,
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
            Self::SetSubscriber { .. } => f.debug_struct("SetSubscriber").finish(),
        }
    }
}

impl<R: Clone + Send + RngCore + 'static> SwarmHandler<R> {
    #[expect(
        clippy::cognitive_complexity,
        reason = "The command handler keeps all chainsync command dispatch in one place."
    )]
    pub(super) fn handle_chainsync_command(&mut self, command: ChainSyncCommand) {
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
            ChainSyncCommand::SetSubscriber { events_tx } => {
                self.chainsync_events_tx = Some(events_tx);
            }
        }
    }

    /// Hands a peer's request to the subscriber, or, if it can't take it now,
    /// tells the peer straight away that it's unavailable: the swarm doesn't
    /// wait, and no request goes unanswered.
    pub(super) fn handle_chainsync_event(&self, event: lb_cryptarchia_sync::Event) {
        let event = ChainSyncEvent::from(event);
        let Some(events_tx) = &self.chainsync_events_tx else {
            tracing::debug!(
                target: LOG_TARGET,
                ?event,
                "no chainsync event subscriber yet; refusing the request"
            );
            refuse(event);
            return;
        };
        match events_tx.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(event)) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    ?event,
                    "chain sync requests come in faster than they're served; refusing one"
                );
                refuse(event);
            }
            Err(TrySendError::Closed(event)) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    ?event,
                    "the chainsync event subscriber is gone; refusing the request"
                );
                refuse(event);
            }
        }
    }
}

/// Answers the peer behind `event` that what it asked for is unavailable.
fn refuse(event: ChainSyncEvent) {
    // A reply channel holds the one answer to its request, so there is room.
    let refused = match event {
        ChainSyncEvent::BlocksRequest { reply_sender, .. } => reply_sender
            .try_send(ProviderResponse::Unavailable {
                reason: BlocksUnavailableReason::Unknown,
            })
            .is_ok(),
        ChainSyncEvent::TipRequest { reply_sender } => reply_sender
            .try_send(ProviderResponse::Unavailable {
                reason: TipResponseFailureReason::Unknown,
            })
            .is_ok(),
    };
    if !refused {
        tracing::debug!(target: LOG_TARGET, "the peer stopped waiting for its answer");
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
