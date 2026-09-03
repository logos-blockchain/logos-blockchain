use core::{
    fmt::{Debug, Display},
    future::ready,
    marker::PhantomData,
};

use futures::{StreamExt as _, stream, stream::BoxStream};
use lb_chain_network_service::Message as ChainNetworkMsg;
use lb_codec::BinaryEncode as _;
use lb_core::{
    codec::DeserializeOp as _,
    header::HeaderId,
    mantle::{traits::Hashable, transactions::hash::PrefixedKey},
};
use lb_log_targets::blend;
use lb_network_service::{
    NetworkService,
    backends::libp2p::{Command, Libp2p, PubSubCommand},
    message::NetworkMsg,
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

type MempoolRelay<Item, Key> = OutboundRelay<MempoolMsg<HeaderId, Item, Item, Key>>;
type ChainNetworkRelay<Item> = OutboundRelay<ChainNetworkMsg<Item>>;

/// A payload dispatcher for a node whose network service uses the libp2p
/// backend.
pub struct Libp2pPayloadDispatcher<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId>
where
    Mempool: RecoverableMempool<BlockId = HeaderId>,
{
    network_relay:
        OutboundRelay<<NetworkService<Libp2p, RuntimeServiceId> as ServiceData>::Message>,
    mempool_relay: MempoolRelay<Mempool::Item, Mempool::Key>,
    chain_network_relay: ChainNetworkRelay<Mempool::Item>,
    settings: Libp2pBroadcastSettings,
    _phantom: PhantomData<(MempoolNetAdapter, ChainNetwork, RuntimeServiceId)>,
}

/// Settings used to broadcast messages to the network service that uses libp2p
/// backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Libp2pBroadcastSettings {
    pub topic: String,
}

impl<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId>
    Libp2pPayloadDispatcher<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId>
where
    Mempool: RecoverableMempool<BlockId = HeaderId>,
    MempoolNetAdapter: Sync,
    RuntimeServiceId: Sync,
{
    /// Broadcast an unencrypted message to the network by publishing the
    /// message under the configured gossipsub topic.
    async fn broadcast_block_proposal(&self, proposal: Vec<u8>) {
        if let Err((e, _)) = self
            .network_relay
            .send(NetworkMsg::Process(Command::PubSub(
                PubSubCommand::Broadcast {
                    topic: self.settings.topic.clone(),
                    message: proposal.into_boxed_slice(),
                },
            )))
            .await
        {
            tracing::error!(target: LOG_TARGET, "error broadcasting block proposal: {e}");
        }
    }
}

impl<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId>
    Libp2pPayloadDispatcher<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId>
where
    Mempool: RecoverableMempool<BlockId = HeaderId> + Sync,
    Mempool::Item: Clone + Serialize + Send + Sync + 'static,
    Mempool::Key: PrefixedKey<Prefix: Send + Sync> + Send + 'static,
    MempoolNetAdapter: Sync,
    ChainNetwork: Sync,
    RuntimeServiceId: Sync,
{
    async fn observe_block_proposals(&self) -> BoxStream<'static, BlendPayload> {
        let chain_network_relay = self.chain_network_relay.clone();
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

        BroadcastStream::new(received)
            .filter_map(|proposal| {
                ready(match proposal {
                    // Encoded the way it was handed to Blend, so that the two are the
                    // same bytes and comparing them is all the sender has to do. Failure to encode as Blend requires means the payload was most likely sent outside of Blend and hence does not need to be returned in the stream.
                    Ok(proposal) => BlendPayload::try_from_proposal(&proposal).ok(),
                    // A lagging observer can only miss a delivery, never invent one, so the
                    // worst it costs is a payload broadcast in the clear that need not have
                    // been.
                    Err(BroadcastStreamRecvError::Lagged(missed)) => {
                        tracing::warn!(target: LOG_TARGET, "Missed {missed} received proposals; a delivered proposal may be broadcast directly anyway.");
                        None
                    }
                })
            })
            .boxed()
    }

    async fn observe_transactions(&self) -> BoxStream<'static, BlendPayload> {
        let mempool_relay = self.mempool_relay.clone();
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

        BroadcastStream::new(accepted)
            .filter_map(|transaction| {
                ready(match transaction {
                    // Encoded the way it was handed to Blend, so that the two are the
                    // same bytes and comparing them is all the sender has to do.
                    Ok(transaction) => BlendPayload::try_from_transaction(&transaction)
                        .inspect_err(|e| {
                            tracing::debug!(target: LOG_TARGET, "A transaction the mempool accepted is not one Blend could have carried: {e}");
                        })
                        .ok(),
                    Err(BroadcastStreamRecvError::Lagged(missed)) => {
                        tracing::warn!(target: LOG_TARGET, "Missed {missed} accepted transactions; a delivered transaction may be broadcast directly anyway.");
                        None
                    }
                })
            })
            .boxed()
    }
}

impl<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId>
    Libp2pPayloadDispatcher<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId>
where
    Mempool: RecoverableMempool<BlockId = HeaderId> + Sync,
    Mempool::Item:
        Hashable<Hash = Mempool::Key> + serde::de::DeserializeOwned + Send + Sync + 'static,
    Mempool::Key: PrefixedKey<Prefix: Send + Sync> + Send + Sync + 'static,
    MempoolNetAdapter: Sync,
    ChainNetwork: Sync,
    RuntimeServiceId: Sync,
{
    /// Submit a decapsulated transaction to the local mempool after validating
    /// its structure.
    async fn submit_transaction(&self, transaction: Vec<u8>) {
        let Ok(transaction) = Mempool::Item::from_bytes(&transaction).inspect_err(|e| {
            tracing::error!(
                target: LOG_TARGET,
                "Discarding a decapsulated payload that does not decode as a transaction: {e}"
            );
        }) else {
            return;
        };

        let (reply_channel, receiver) = oneshot::channel();
        if let Err((e, _)) = self
            .mempool_relay
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
}

#[async_trait::async_trait]
impl<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId> PayloadDispatcher<RuntimeServiceId>
    for Libp2pPayloadDispatcher<MempoolNetAdapter, Mempool, ChainNetwork, RuntimeServiceId>
where
    Mempool: RecoverableMempool<BlockId = HeaderId, RecoveryState: 'static> + Send + Sync + 'static,
    Mempool::Item: Hashable<Hash = Mempool::Key>
        + Clone
        + Serialize
        + serde::de::DeserializeOwned
        + Send
        + Sync
        + 'static,
    Mempool::Key: PrefixedKey<Prefix: Send + Sync> + Send + Sync + 'static,
    Mempool::Settings: Clone + Send + Sync + 'static,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync + 'static,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync
        + 'static,
    MempoolNetAdapter::Settings: Clone + Send + Sync + 'static,
    ChainNetwork: ServiceData<Message = ChainNetworkMsg<Mempool::Item>> + Send + Sync + 'static,
    RuntimeServiceId: Clone
        + Debug
        + Display
        + Sync
        + Send
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
            BlendPayload::BlockProposal(proposal) => self.broadcast_block_proposal(proposal).await,
            BlendPayload::Transaction(transaction) => self.submit_transaction(transaction).await,
        }
    }

    async fn observe_broadcasts(&self) -> BoxStream<'static, BlendPayload> {
        // `once(..).flatten()` is what defers each subscription to the first poll,
        // so that neither has to be answered before the caller's loop can start.
        stream::select(
            stream::once(self.observe_block_proposals()).flatten(),
            stream::once(self.observe_transactions()).flatten(),
        )
        .boxed()
    }
}
