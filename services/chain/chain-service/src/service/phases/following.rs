use core::fmt::{self, Debug, Display};
use std::collections::HashSet;

use bytes::Bytes;
use futures::StreamExt as _;
use lb_core::{
    block::Block,
    events::Events,
    mantle::traits::{MantleTxWithProofs, PreverifiedMantleTx},
};
use lb_cryptarchia_sync::{GetTipResponse, ProviderResponse};
use lb_network_service::message::ChainSyncEvent;
use lb_storage_service::{api::chain::StorageChainApi, backends::StorageBackend};
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

impl<Tx, Storage, RuntimeServiceId> Service<Following, Tx, Storage, RuntimeServiceId>
where
    Tx: PreverifiedMantleTx
        + MantleTxWithProofs
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>> + Into<Bytes>,
    <Storage as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Display + 'static,
{
    /// Runs the phase forever.
    pub async fn process_following(mut self) {
        info!(target: LOG_TARGET, "entering {:?} phase", self.phase);

        loop {
            tokio::select! {
                Some(msg) = self.inbound_relay.next() => match msg {
                    ConsensusMsg::Query(query) => {
                        self.process_query(query).await;
                    }
                    ConsensusMsg::ApplyBlock { block, reply_channel } => {
                        self.apply_block_and_reply(*block, reply_channel).await;
                    }
                    ConsensusMsg::ChainSync(event) => {
                        self.handle_chainsync_event(event).await;
                    }
                    ConsensusMsg::IbdCompleted => {
                        debug!(target: LOG_TARGET, "ignoring IbdCompleted: already in {:?} phase", self.phase);
                    }
                },
                Some(tick) = self.slot_timer.next() => self.current_slot = tick.slot,
                _ = self.state_recording_timer.tick() => self.record_recovery_state(),
            }
        }
    }

    /// Serve a chainsync request from a peer.
    async fn handle_chainsync_event(&self, event: ChainSyncEvent) {
        match event {
            ChainSyncEvent::ProvideBlocksRequest {
                target_block,
                local_tip,
                latest_immutable_block,
                additional_blocks,
                reply_sender,
            } => {
                let known_blocks = vec![local_tip, latest_immutable_block]
                    .into_iter()
                    .chain(additional_blocks)
                    .collect::<HashSet<_>>();

                self.sync_blocks_provider
                    .send_blocks(
                        &self.cryptarchia.consensus,
                        target_block,
                        &known_blocks,
                        reply_sender,
                    )
                    .await;
            }
            ChainSyncEvent::ProvideTipRequest { reply_sender } => {
                let tip = self.cryptarchia.tip_branch();
                let response = ProviderResponse::Available(GetTipResponse::Tip {
                    tip: tip.id(),
                    slot: tip.slot(),
                    height: tip.length(),
                });

                trace!(target: LOG_TARGET, ?response, "sending tip response");
                if let Err(err) = reply_sender.send(response).await {
                    error!(target: LOG_TARGET, %err, "failed to send tip header");
                }
            }
        }
    }
}
