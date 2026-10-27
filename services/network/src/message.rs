use std::{collections::HashSet, fmt::Debug};

use educe::Educe;
use lb_core::header::HeaderId;
use lb_cryptarchia_sync::{BlocksProviderResponse, TipProviderResponse};
use tokio::sync::{mpsc::Sender, oneshot};
use tokio_stream::wrappers::BroadcastStream;

use crate::backends::NetworkBackend;

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

#[derive(Clone, Educe)]
#[educe(Debug)]
pub enum ChainSyncEvent {
    BlocksRequest {
        /// Return blocks up to `target_block`.
        target_block: HeaderId,
        /// The requester's canonical chain tip.
        requester_tip: HeaderId,
        /// The latest immutable block.
        latest_immutable_block: HeaderId,
        /// The list of additional blocks that the requester has.
        additional_blocks: HashSet<HeaderId>,
        /// Channel to send blocks to network.
        #[educe(Debug(ignore))]
        reply_sender: Sender<BlocksProviderResponse>,
    },
    TipRequest {
        /// Channel to send the latest tip to the network.
        #[educe(Debug(ignore))]
        reply_sender: Sender<TipProviderResponse>,
    },
}
