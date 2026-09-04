use core::{
    fmt::{Debug, Display},
    future::ready,
    marker::PhantomData,
};

use futures::{Stream, StreamExt as _, stream, stream::BoxStream};
use lb_chain_network_service::Message as ChainNetworkMsg;
use lb_core::{
    codec::DeserializeOp,
    header::HeaderId,
    mantle::{traits::Hashable, transactions::hash::PrefixedKey},
};
use lb_log_targets::blend;
use lb_network_service::{
    NetworkService,
    backends::libp2p::{Command, Libp2p, Message as PubSubMessage, PubSubCommand},
    message::{ChainSyncEvent, NetworkMsg},
};
use lb_storage_service::StorageService;
use lb_tx_service::{
    MempoolMsg, TxMempoolService, backend::RecoverableMempool,
    network::NetworkAdapter as MempoolNetworkAdapter, storage::MempoolStorageAdapter,
};
use overwatch::services::{AsServiceId, ServiceData, relay::OutboundRelay};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};

use super::PayloadDispatcher;
use crate::message::BlendPayload;

const LOG_TARGET: &str = blend::service::CORE;

type NetworkRelay = OutboundRelay<NetworkMsg<Command, PubSubMessage, ChainSyncEvent>>;
type MempoolRelay<Item, Key> = OutboundRelay<MempoolMsg<HeaderId, Item, Item, Key>>;
type ChainNetworkRelay<Item> = OutboundRelay<ChainNetworkMsg<Item>>;

/// A payload dispatcher for a node whose network service uses the libp2p
/// backend.
pub struct Libp2pPayloadDispatcher<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId>
where
    Mempool: RecoverableMempool<BlockId = HeaderId>,
{
    network_relay: NetworkRelay,
    mempool_relay: MempoolRelay<Mempool::Item, Mempool::Key>,
    chain_network_relay: ChainNetworkRelay<Mempool::Item>,
    settings: Libp2pBroadcastSettings,
    #[expect(
        clippy::type_complexity,
        reason = "Phantom data stuff to not require Send/Sync on type parameters that are only used as tags"
    )]
    _phantom: PhantomData<fn() -> (MempoolNetAdapter, ChainNetwork, RuntimeServiceId)>,
}

/// Settings used to broadcast messages to the network service that uses libp2p
/// backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Libp2pBroadcastSettings {
    pub topic: String,
}

/// Broadcast an unencrypted block proposal to the network by publishing it
/// under the configured gossipsub topic.
async fn broadcast_block_proposal(network_relay: &NetworkRelay, topic: String, proposal: Vec<u8>) {
    if let Err((e, _)) = network_relay
        .send(NetworkMsg::Process(Command::PubSub(
            PubSubCommand::Broadcast {
                topic,
                message: proposal.into_boxed_slice(),
            },
        )))
        .await
    {
        tracing::error!(target: LOG_TARGET, "error broadcasting block proposal: {e}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamType {
    Proposals,
    Transactions,
}

impl AsRef<str> for StreamType {
    fn as_ref(&self) -> &str {
        match self {
            Self::Proposals => "block proposals",
            Self::Transactions => "transactions",
        }
    }
}

/// The observations of `subscription`, up to the moment it lags.
///
/// A missed observation is a delivery this node cannot see, and a delivery it
/// cannot see is a payload it reveals. Carrying on past a lag would hand an
/// adversary a way to force those reveals by flooding the node into one — the
/// cheaper of the two to flood being the transactions — so the stream ends
/// instead, and the detection that reads it stops with it.
fn stop_observing_on_lag<Observed>(
    subscription: BroadcastStream<Observed>,
    observed_type: StreamType,
) -> impl Stream<Item = Observed> + Send
where
    Observed: Clone + Send + 'static,
{
    subscription
        .take_while(move |subscribed| {
            ready(match subscribed {
                Ok(_) => true,
                Err(BroadcastStreamRecvError::Lagged(missed)) => {
                    tracing::error!(target: LOG_TARGET, "Missed {missed} {observed_type}; a delivery can no longer be told from a loss, so the direct broadcast is disabled for the rest of this run.", observed_type = observed_type.as_ref());
                    false
                }
            })
        })
        .filter_map(|subscribed| ready(subscribed.ok()))
}

async fn observe_block_proposals<Tx>(
    chain_network_relay: ChainNetworkRelay<Tx>,
) -> BoxStream<'static, BlendPayload>
where
    Tx: Send + 'static,
{
    let (result_sender, receiver) = oneshot::channel();
    if let Err((e, _)) = chain_network_relay
        .send(ChainNetworkMsg::SubscribeToProposals { result_sender })
        .await
    {
        tracing::error!(target: LOG_TARGET, "Failed to ask the chain network for the proposals it receives: {e}");
        return stream::empty().boxed();
    }
    let Ok(received) = receiver.await else {
        tracing::error!(target: LOG_TARGET, "The chain network dropped the received-proposal subscription.");
        return stream::empty().boxed();
    };

    stop_observing_on_lag(BroadcastStream::new(received), StreamType::Proposals)
        .filter_map(|proposal| {
            // Encoded the way it was handed to Blend, so that the two are the same
            // bytes and comparing them is all the sender has to do. One that does
            // not fit a payload is one Blend cannot have carried, so it is not a
            // delivery this node is waiting on.
            ready(BlendPayload::try_from_proposal(&proposal).ok())
        })
        .boxed()
}

/// Submit a decapsulated transaction to the local mempool after validating its
/// structure.
async fn submit_transaction<Item, Key>(
    mempool_relay: &MempoolRelay<Item, Key>,
    transaction: Vec<u8>,
) where
    Item: Hashable<Hash = Key> + DeserializeOp + Send,
    Key: PrefixedKey<Prefix: Send> + Send,
{
    let Ok(transaction) = Item::from_bytes(&transaction).inspect_err(|e| {
        tracing::error!(
            target: LOG_TARGET,
            "Discarding a decapsulated payload that does not decode as a transaction: {e}"
        );
    }) else {
        return;
    };

    let (reply_channel, receiver) = oneshot::channel();
    if let Err((e, _)) = mempool_relay
        .send(MempoolMsg::Add {
            key: transaction.hash(),
            payload: transaction,
            reply_channel,
        })
        .await
    {
        tracing::error!(target: LOG_TARGET, "Error submitting a blended transaction to the mempool: {e}");
        return;
    }

    let outcome = receiver
        .await
        .map_err(|e| format!("the mempool dropped the reply: {e}"))
        .and_then(|added| added.map_err(|e| format!("the mempool refused it: {e}")));
    if let Err(reason) = outcome {
        tracing::debug!(target: LOG_TARGET, "Blended transaction was not added to the mempool: {reason}");
    }
}

async fn observe_transactions<Item, Key>(
    mempool_relay: MempoolRelay<Item, Key>,
) -> BoxStream<'static, BlendPayload>
where
    Item: Serialize + Clone + Send + 'static,
    Key: PrefixedKey<Prefix: Send> + Send + 'static,
{
    let (reply_channel, receiver) = oneshot::channel();
    if let Err((e, _)) = mempool_relay
        .send(MempoolMsg::SubscribeToAccepted { reply_channel })
        .await
    {
        tracing::error!(target: LOG_TARGET, "Failed to ask the mempool for the transactions it accepts: {e}");
        return stream::empty().boxed();
    }
    let Ok(accepted) = receiver.await else {
        tracing::error!(target: LOG_TARGET, "The mempool dropped the accepted-transaction subscription.");
        return stream::empty().boxed();
    };

    stop_observing_on_lag(BroadcastStream::new(accepted), StreamType::Transactions)
        .filter_map(|transaction| {
            // Encoded the way it was handed to Blend, so that the two are the same
            // bytes and comparing them is all the sender has to do. If it cannot fit into
            // the maximum size Blend allows, it means the tx was not sent with Blend in the
            // first place.
            ready(BlendPayload::try_from_transaction(&transaction).ok())
        })
        .boxed()
}

#[async_trait::async_trait]
impl<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId> PayloadDispatcher<RuntimeServiceId>
    for Libp2pPayloadDispatcher<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId>
where
    Mempool: RecoverableMempool<BlockId = HeaderId, RecoveryState: 'static> + Send + Sync + 'static,
    Mempool::Item:
        Hashable<Hash = Mempool::Key> + Clone + Serialize + DeserializeOp + Send + 'static,
    Mempool::Key: PrefixedKey<Prefix: Send> + Send + 'static,
    Mempool::Settings: Clone + Send + Sync,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync + 'static,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync
        + 'static,
    MempoolNetAdapter::Settings: Clone + Send + Sync,
    ChainNetwork: ServiceData<Message = ChainNetworkMsg<Mempool::Item>> + Send + Sync + 'static,
    RuntimeServiceId: Clone
        + Debug
        + Display
        + Send
        + Sync
        + 'static
        + AsServiceId<
            StorageService<
                <Mempool::Storage as MempoolStorageAdapter<RuntimeServiceId>>::Backend,
                RuntimeServiceId,
            >,
        >,
{
    type Backend = Libp2p;
    type ChainNetworkService = ChainNetwork;
    type MempoolService =
        TxMempoolService<MempoolNetAdapter, Mempool, Mempool::Storage, RuntimeServiceId>;
    type Settings = Libp2pBroadcastSettings;

    fn new(
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
        mempool_relay: OutboundRelay<<Self::MempoolService as ServiceData>::Message>,
        chain_network_relay: OutboundRelay<<Self::ChainNetworkService as ServiceData>::Message>,
        settings: Self::Settings,
    ) -> Self {
        Self {
            network_relay,
            mempool_relay,
            chain_network_relay,
            settings,
            _phantom: PhantomData,
        }
    }

    async fn dispatch(&self, payload: BlendPayload) {
        match payload {
            BlendPayload::BlockProposal(proposal) => {
                broadcast_block_proposal(
                    &self.network_relay,
                    self.settings.topic.clone(),
                    proposal,
                )
                .await;
            }
            BlendPayload::Transaction(transaction) => {
                submit_transaction(&self.mempool_relay, transaction).await;
            }
        }
    }

    async fn observe_broadcasts(&self) -> BoxStream<'static, BlendPayload> {
        let proposals_stream =
            stream::once(observe_block_proposals(self.chain_network_relay.clone())).flatten();
        let transactions_stream =
            stream::once(observe_transactions(self.mempool_relay.clone())).flatten();

        // Each half is followed by a `None` marking its end, so that whichever ends
        // first stops the merge rather than merely dropping out of it.
        stream::select(
            proposals_stream.map(Some).chain(stream::once(ready(None))),
            transactions_stream
                .map(Some)
                .chain(stream::once(ready(None))),
        )
        .take_while(|observed| ready(observed.is_some()))
        .map(|observed| observed.expect("`take_while` stops at the first end marker."))
        .boxed()
    }
}
