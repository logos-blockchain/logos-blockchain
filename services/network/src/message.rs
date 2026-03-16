use std::{collections::HashSet, fmt::Debug};

use lb_core::header::HeaderId;
use lb_cryptarchia_sync::{BlocksResponse, TipResponse};
use tokio::sync::{mpsc::Sender, oneshot};
use tokio_stream::wrappers::BroadcastStream;

use crate::backends::{NetworkBackend, libp2p::PeerId};

#[derive(Debug)]
pub enum NetworkMsg<Payload, PubSubEvent, ChainSyncEvent> {
    Process(Payload),
    SubscribeToPubSub {
        sender: oneshot::Sender<BroadcastStream<PubSubEvent>>,
    },
    SubscribeToChainSync {
        sender: oneshot::Sender<BroadcastStream<ChainSyncEvent>>,
    },
}

pub type BackendNetworkMsg<Backend, RuntimeServiceId> = NetworkMsg<
    <Backend as NetworkBackend<RuntimeServiceId>>::Message,
    <Backend as NetworkBackend<RuntimeServiceId>>::PubSubEvent,
    <Backend as NetworkBackend<RuntimeServiceId>>::ChainSyncEvent,
>;

#[derive(Debug, Clone)]
pub enum ChainSyncEvent {
    ProvideBlocksRequest {
        /// Return blocks up to `target_block`.
        target_block: HeaderId,
        /// The local canonical chain latest block.
        local_tip: HeaderId,
        /// The latest immutable block.
        latest_immutable_block: HeaderId,
        /// The list of additional blocks that the requester has.
        additional_blocks: HashSet<HeaderId>,
        /// Channel to send blocks to network.
        reply_sender: Sender<BlocksResponse>,
        /// The peer ID of the requester.
        peer_id: PeerId,
    },
    ProvideTipRequest {
        /// Channel to send the latest tip to the network.
        reply_sender: Sender<TipResponse>,
        /// The peer ID of the requester.
        peer_id: PeerId,
    },
}
