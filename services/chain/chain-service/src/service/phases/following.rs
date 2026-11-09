use core::fmt::{self, Debug};
use std::collections::HashSet;

use futures::{StreamExt as _, future::OptionFuture};
use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode};
use lb_core::mantle::{
    ledger::verification_mode::StandardMode,
    traits::{PreverifiedMantleTransaction, SignedMantleTx, StorageSize},
    transactions::states::Preverified,
};
use lb_cryptarchia_sync::{ProviderResponse, TipResponse};
use lb_network_service::message::ChainSyncEvent;
use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, error, info, trace};

use super::{Phase, PhaseTag};
use crate::{ConsensusMsg, LOG_TARGET, service::Service};

/// `Following` phase: Follow the chain in real time. This phase never ends.
pub struct Following;

impl Phase for Following {
    const TAG: PhaseTag = PhaseTag::Following;
}

impl Debug for Following {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Self::TAG.fmt(f)
    }
}

impl<Tx> Service<Following, Tx>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Send
        + Sync
        + Unpin
        + 'static,
{
    /// Runs the phase forever.
    pub async fn process_following(mut self) {
        info!(target: LOG_TARGET, "entering {:?} phase", self.phase);

        loop {
            tokio::select! {
                Some(msg) = self.inbound_relay.next() => match msg {
                    ConsensusMsg::Query(query) => {
                        self.process_query(query);
                    }
                    ConsensusMsg::ApplyBlock { block, reply_channel } => {
                        self.apply_block(*block, reply_channel);
                    }
                    ConsensusMsg::ChainSync(event) => {
                        self.handle_chainsync_event(event);
                    }
                    ConsensusMsg::IbdCompleted => {
                        debug!(target: LOG_TARGET, "ignoring IbdCompleted: already in {:?} phase", self.phase);
                    }
                },
                Some(preparation) = OptionFuture::from(self.block_underway.as_mut()) => {
                    self.finish_block(preparation).await;
                }
                Some(()) = self.pending_queries.next() => {}
                Some(tick) = self.slot_timer.next() => self.current_slot = tick.slot,
                _ = self.state_recording_timer.tick() => self.record_recovery_state(),
            }
        }
    }

    /// Serve a chainsync request from a peer.
    ///
    /// Blocks are served by a future the service loop polls along with its
    /// other inputs, from the block tree as it is when the request comes. The
    /// protocol caps how many are served at once.
    fn handle_chainsync_event(&self, event: ChainSyncEvent) {
        match event {
            ChainSyncEvent::BlocksRequest {
                target_block,
                requester_tip,
                latest_immutable_block,
                additional_blocks,
                reply_sender,
            } => {
                let known_blocks = vec![requester_tip, latest_immutable_block]
                    .into_iter()
                    .chain(additional_blocks)
                    .collect::<HashSet<_>>();

                let blocks_provider = self.sync_blocks_provider.clone();
                let block_tree = self.cryptarchia.consensus.clone();
                self.pending_queries.push(Box::pin(async move {
                    blocks_provider
                        .reply_with_blocks(&block_tree, target_block, &known_blocks, reply_sender)
                        .await;
                }));
            }
            ChainSyncEvent::TipRequest { reply_sender } => {
                let tip = self.cryptarchia.tip_branch();
                let response = ProviderResponse::Available(TipResponse::Tip {
                    tip: tip.id(),
                    slot: tip.slot(),
                    height: tip.length(),
                });

                trace!(target: LOG_TARGET, ?response, "sending tip response");
                // The channel is the request's own, with room for its one reply.
                if let Err(err) = reply_sender.try_send(response) {
                    error!(target: LOG_TARGET, %err, "failed to send tip header");
                }
            }
        }
    }
}
