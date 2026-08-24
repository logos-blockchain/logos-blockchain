use core::{
    fmt::{Debug, Display},
    marker::PhantomData,
};

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

use super::PayloadDispatcher;
use crate::message::BlendPayload;

const LOG_TARGET: &str = blend::service::CORE;

type MempoolRelay<Item, Key> = OutboundRelay<MempoolMsg<HeaderId, Item, Item, Key>>;

/// A payload dispatcher for a node whose network service uses the libp2p
/// backend.
pub struct Libp2pPayloadDispatcher<MempoolNetAdapter, Mempool, RuntimeServiceId>
where
    Mempool: RecoverableMempool<BlockId = HeaderId>,
{
    network_relay:
        OutboundRelay<<NetworkService<Libp2p, RuntimeServiceId> as ServiceData>::Message>,
    mempool_relay: MempoolRelay<Mempool::Item, Mempool::Key>,
    settings: Libp2pBroadcastSettings,
    _phantom: PhantomData<(MempoolNetAdapter, RuntimeServiceId)>,
}

/// Settings used to broadcast messages to the network service that uses libp2p
/// backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Libp2pBroadcastSettings {
    pub topic: String,
}

impl<MempoolNetAdapter, Mempool, RuntimeServiceId>
    Libp2pPayloadDispatcher<MempoolNetAdapter, Mempool, RuntimeServiceId>
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

impl<MempoolNetAdapter, Mempool, RuntimeServiceId>
    Libp2pPayloadDispatcher<MempoolNetAdapter, Mempool, RuntimeServiceId>
where
    Mempool: RecoverableMempool<BlockId = HeaderId>,
    Mempool::Item: Hashable<Hash = Mempool::Key> + serde::de::DeserializeOwned + Send + 'static,
    Mempool::Key: PrefixedKey<Prefix: Send + Sync> + Send + 'static,
    MempoolNetAdapter: Sync,
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
impl<MempoolNetAdapter, Mempool, RuntimeServiceId> PayloadDispatcher<RuntimeServiceId>
    for Libp2pPayloadDispatcher<MempoolNetAdapter, Mempool, RuntimeServiceId>
where
    Mempool: RecoverableMempool<BlockId = HeaderId, RecoveryState: 'static> + Send + Sync + 'static,
    Mempool::Item: Hashable<Hash = Mempool::Key> + serde::de::DeserializeOwned + Send + 'static,
    Mempool::Key: PrefixedKey<Prefix: Send + Sync> + Send + 'static,
    Mempool::Settings: Clone + Send + Sync + 'static,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync + 'static,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync
        + 'static,
    MempoolNetAdapter::Settings: Clone + Send + Sync + 'static,
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
    type MempoolService =
        TxMempoolService<MempoolNetAdapter, Mempool, Mempool::Storage, RuntimeServiceId>;
    type Settings = Libp2pBroadcastSettings;

    fn new(
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
        mempool_relay: OutboundRelay<<Self::MempoolService as ServiceData>::Message>,
        settings: Self::Settings,
    ) -> Self {
        Self {
            network_relay,
            mempool_relay,
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
}
