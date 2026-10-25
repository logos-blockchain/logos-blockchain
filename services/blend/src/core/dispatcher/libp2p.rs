use core::{
    fmt::{Debug, Display},
    future::ready,
    marker::PhantomData,
    mem,
};

use futures::{Stream, StreamExt as _, stream, stream::BoxStream};
use lb_binary_codec::bincode::DeserializeOp;
use lb_chain_network_service::Message as ChainNetworkMsg;
use lb_core::{
    header::HeaderId,
    mantle::{traits::Hashable, transactions::hash::PrefixedKey},
};
use lb_cryptarchia_engine::{Era, EraSchedule};
use lb_log_targets::blend;
use lb_network_service::{
    backends::libp2p::{Command, Libp2p, Message as PubSubMessage, PubSubCommand},
    message::{BackendNetworkMsg, ChainSyncEvent, NetworkMsg},
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
use crate::message::{DataPayload, MAX_PAYLOAD_BODY_SIZE};

const LOG_TARGET: &str = blend::service::CORE;

type NetworkRelay = OutboundRelay<NetworkMsg<Command, PubSubMessage, ChainSyncEvent>>;
type MempoolRelay<Item, Key> = OutboundRelay<MempoolMsg<HeaderId, Item, Item, Key>>;
type ChainNetworkRelay<Item> = OutboundRelay<ChainNetworkMsg<Item>>;

/// A payload dispatcher for a node whose network service uses the libp2p
/// backend.
pub struct Libp2pPayloadDispatcher<
    MempoolNetAdapter,
    Mempool,
    ChainNetwork,
    TimeBackend,
    RuntimeServiceId,
> where
    Mempool: RecoverableMempool<BlockId = HeaderId>,
{
    network_relay: NetworkRelay,
    mempool_relay: MempoolRelay<Mempool::Item, Mempool::Key>,
    chain_network_relay: ChainNetworkRelay<Mempool::Item>,
    current_era: Era,
    previous_era: Option<Era>,
    era_schedule: EraSchedule<Libp2pBroadcastSettings>,
    #[expect(
        clippy::type_complexity,
        reason = "Phantom data stuff to not require Send/Sync on type parameters that are only used as tags"
    )]
    _phantom: PhantomData<
        fn() -> (
            MempoolNetAdapter,
            ChainNetwork,
            TimeBackend,
            RuntimeServiceId,
        ),
    >,
}

/// Settings used to broadcast messages to the network service that uses libp2p
/// backend, in an era.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Libp2pBroadcastSettings {
    pub topic: String,
}

fn proposal_topic_for_era(era_schedule: &EraSchedule<Libp2pBroadcastSettings>, era: Era) -> String {
    era_schedule
        .get(era)
        .expect("The eras of the dispatcher are scheduled.")
        .parameters
        .topic
        .clone()
}

/// Broadcast an unencrypted block proposal to the network by publishing it
/// under the configured gossipsub topic.
async fn broadcast_block_proposal(network_relay: &NetworkRelay, topic: String, proposal: Vec<u8>) {
    if proposal.len() > MAX_PAYLOAD_BODY_SIZE {
        tracing::error!(
            target: LOG_TARGET,
            size = proposal.len(),
            maximum = MAX_PAYLOAD_BODY_SIZE,
            "Refusing to broadcast an oversized block proposal"
        );
        return;
    }

    if let Err(error) = network_relay
        .send(NetworkMsg::Process(Command::PubSub(
            PubSubCommand::Broadcast {
                topic,
                message: proposal.into_boxed_slice(),
            },
        )))
        .await
    {
        tracing::error!(target: LOG_TARGET, "error broadcasting block proposal: {error}");
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

/// The observations of `subscription`, carried on past any lag.
///
/// A missed observation is a delivery this node cannot see, and a delivery it
/// cannot see is a payload it broadcasts in the clear once its deadline passes.
/// That costs the payloads the lag skipped, whereas ending the stream would
/// cost every payload sent from then on its fallback, so a lag is logged and
/// the stream goes on from what the channel still holds.
fn observe_items_ignoring_lags<Observed>(
    subscription: BroadcastStream<Observed>,
    observed_type: StreamType,
) -> impl Stream<Item = Observed> + Send
where
    Observed: Clone + Send + 'static,
{
    subscription.filter_map(move |subscribed| {
        ready(match subscribed {
            Ok(observed) => Some(observed),
            Err(BroadcastStreamRecvError::Lagged(missed)) => {
                tracing::warn!(target: LOG_TARGET, missed, "Missed {missed} {observed_type}; any of this node's own payloads among them is broadcast in the clear at its delivery deadline.", observed_type = observed_type.as_ref());
                None
            }
        })
    })
}

async fn observe_block_proposals<Tx>(
    chain_network_relay: ChainNetworkRelay<Tx>,
) -> BoxStream<'static, DataPayload>
where
    Tx: Send + 'static,
{
    let (result_sender, receiver) = oneshot::channel();
    if let Err(error) = chain_network_relay
        .send(ChainNetworkMsg::SubscribeToProposals { result_sender })
        .await
    {
        tracing::error!(target: LOG_TARGET, "Failed to ask the chain network for the proposals it receives: {error}");
        return stream::empty().boxed();
    }
    let Ok(received) = receiver.await else {
        tracing::error!(target: LOG_TARGET, "The chain network dropped the received-proposal subscription.");
        return stream::empty().boxed();
    };

    observe_items_ignoring_lags(BroadcastStream::new(received), StreamType::Proposals)
        .filter_map(|proposal| {
            // Encoded the way it was handed to Blend, so that the two are the same
            // bytes and comparing them is all the sender has to do. One that does
            // not fit a payload is one Blend cannot have carried, so it is not a
            // delivery this node is waiting on.
            ready(DataPayload::try_from_proposal(&proposal).ok())
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
    if let Err(error) = mempool_relay
        .send(MempoolMsg::Add {
            key: transaction.hash(),
            payload: transaction,
            reply_channel,
        })
        .await
    {
        tracing::error!(target: LOG_TARGET, "Error submitting a blended transaction to the mempool: {error}");
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
) -> BoxStream<'static, DataPayload>
where
    Item: Serialize + Clone + Send + 'static,
    Key: PrefixedKey<Prefix: Send> + Send + 'static,
{
    let (reply_channel, receiver) = oneshot::channel();
    if let Err(error) = mempool_relay
        .send(MempoolMsg::SubscribeToAccepted { reply_channel })
        .await
    {
        tracing::error!(target: LOG_TARGET, "Failed to ask the mempool for the transactions it accepts: {error}");
        return stream::empty().boxed();
    }
    let Ok(accepted) = receiver.await else {
        tracing::error!(target: LOG_TARGET, "The mempool dropped the accepted-transaction subscription.");
        return stream::empty().boxed();
    };

    observe_items_ignoring_lags(BroadcastStream::new(accepted), StreamType::Transactions)
        .filter_map(|transaction| {
            // Encoded the way it was handed to Blend, so that the two are the same
            // bytes and comparing them is all the sender has to do. If it cannot fit into
            // the maximum size Blend allows, it means the tx was not sent with Blend in the
            // first place.
            ready(DataPayload::try_from_transaction(&transaction).ok())
        })
        .boxed()
}

#[async_trait::async_trait]
impl<MempoolNetAdapter, Mempool, ChainNetwork, TimeBackend, RuntimeServiceId>
    PayloadDispatcher<RuntimeServiceId>
    for Libp2pPayloadDispatcher<
        MempoolNetAdapter,
        Mempool,
        ChainNetwork,
        TimeBackend,
        RuntimeServiceId,
    >
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
    TimeBackend: 'static,
    RuntimeServiceId: Clone
        + Debug
        + Display
        + Send
        + Sync
        + 'static
        + AsServiceId<StorageService<RuntimeServiceId>>,
{
    type Backend = Libp2p;
    type ChainNetworkService = ChainNetwork;
    type MempoolService = TxMempoolService<
        MempoolNetAdapter,
        Mempool,
        Mempool::Storage,
        TimeBackend,
        RuntimeServiceId,
    >;
    type Settings = EraSchedule<Libp2pBroadcastSettings>;

    fn new(
        network_relay: OutboundRelay<BackendNetworkMsg<Self::Backend, RuntimeServiceId>>,
        mempool_relay: OutboundRelay<<Self::MempoolService as ServiceData>::Message>,
        chain_network_relay: OutboundRelay<<Self::ChainNetworkService as ServiceData>::Message>,
        era_schedule: Self::Settings,
        current_era: Era,
    ) -> Self {
        Self {
            network_relay,
            mempool_relay,
            chain_network_relay,
            current_era,
            previous_era: None,
            era_schedule,
            _phantom: PhantomData,
        }
    }

    async fn rotate_era(&mut self, era: Era) {
        let Some(era_settings) = self.era_schedule.get(era) else {
            tracing::error!(
                target: LOG_TARGET,
                "Asked to enter era {}, which is not scheduled",
                era.into_inner()
            );
            return;
        };
        if era == self.current_era {
            return;
        }
        // An earlier era still held has had its transition period end
        // already: only the report of its end went missing. Entering the new
        // era below replaces it.
        if let Some(lingering_era) = self.previous_era {
            tracing::warn!(
                target: LOG_TARGET,
                "The end of the previous era's transition period was never reported: block proposals are no longer broadcast in era {}",
                lingering_era.into_inner()
            );
        }
        tracing::info!(
            target: LOG_TARGET,
            "Entered era {}: block proposals are broadcast on {}",
            era.into_inner(),
            era_settings.parameters.topic
        );
        self.previous_era = Some(mem::replace(&mut self.current_era, era));
    }

    async fn complete_era_transition(&mut self, ended_era: Era) {
        if self.previous_era != Some(ended_era) {
            return;
        }
        self.previous_era = None;
        tracing::info!(
            target: LOG_TARGET,
            "Transition period for era {} is over.",
            ended_era.into_inner(),
        );
    }

    async fn dispatch(&self, payload: DataPayload, era: Era) {
        match payload {
            DataPayload::BlockProposal(proposal) => {
                // This node serves the current era, and the previous one during
                // its transition period.
                if era != self.current_era && self.previous_era != Some(era) {
                    tracing::error!(
                        target: LOG_TARGET,
                        "Refusing to broadcast a block proposal in era {}, which this node no longer serves",
                        era.into_inner()
                    );
                    return;
                }
                let topic = proposal_topic_for_era(&self.era_schedule, era);
                broadcast_block_proposal(&self.network_relay, topic, proposal).await;
            }
            DataPayload::Transaction(transaction) => {
                submit_transaction(&self.mempool_relay, transaction).await;
            }
        }
    }

    async fn observe_broadcasts(&self) -> BoxStream<'static, DataPayload> {
        let proposals_stream =
            stream::once(observe_block_proposals(self.chain_network_relay.clone())).flatten();
        let transactions_stream =
            stream::once(observe_transactions(self.mempool_relay.clone())).flatten();

        stream::select(proposals_stream, transactions_stream).boxed()
    }
}

#[cfg(test)]
mod tests {
    use core::pin::pin;

    use futures::StreamExt as _;
    use tokio::sync::broadcast;
    use tokio_stream::wrappers::BroadcastStream;

    use super::{StreamType, observe_items_ignoring_lags};

    /// A lag costs the observations it skipped and nothing after them: the
    /// stream goes on with what the channel still holds and what it carries
    /// next.
    #[tokio::test]
    async fn observing_carries_on_past_a_lag() {
        let (channel, subscription) = broadcast::channel(1);
        let mut observed = pin!(observe_items_ignoring_lags(
            BroadcastStream::new(subscription),
            StreamType::Transactions
        ));
        // One more than the channel holds, so the first is skipped.
        channel.send(1).expect("the stream is subscribed");
        channel.send(2).expect("the stream is subscribed");

        assert_eq!(observed.next().await, Some(2));
        channel.send(3).expect("the stream is subscribed");
        assert_eq!(observed.next().await, Some(3));
    }
}
