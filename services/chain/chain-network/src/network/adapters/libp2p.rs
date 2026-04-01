use std::{collections::HashSet, fmt::Debug, hash::Hash, marker::PhantomData, time::Instant};

use futures::{FutureExt as _, TryStreamExt as _, future::select_ok};
use lb_chain_service_common::NetworkMessage;
use lb_core::{
    block::{Block, Proposal},
    codec::DeserializeOp as _,
    header::HeaderId,
    mantle::AuthenticatedMantleTx,
};
use lb_cryptarchia_sync::GetTipResponse;
use lb_network_service::{
    NetworkService,
    backends::libp2p::{
        ChainSyncCommand, Command, DiscoveryCommand, Libp2p, NetworkCommand, PeerId,
        PubSubCommand::Subscribe,
    },
    message::{ChainSyncEvent, NetworkMsg},
};
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};
use rand::{seq::IteratorRandom as _, thread_rng};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;
use tokio_stream::{StreamExt as _, wrappers::errors::BroadcastStreamRecvError};
use tracing::debug;

use crate::{
    metrics,
    network::{BoxedStream, NetworkAdapter},
};

type Relay<T, RuntimeServiceId> =
    OutboundRelay<<NetworkService<T, RuntimeServiceId> as ServiceData>::Message>;

#[derive(Clone)]
pub struct LibP2pAdapter<Tx, RuntimeServiceId>
where
    Tx: Clone + Eq,
{
    network_relay:
        OutboundRelay<<NetworkService<Libp2p, RuntimeServiceId> as ServiceData>::Message>,
    settings: LibP2pAdapterSettings,
    _phantom_tx: PhantomData<Tx>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibP2pAdapterSettings {
    pub topic: String,
    /// The maximum number of connected peers to attempt downloads from
    /// for each target block.
    pub max_connected_peers_to_try_download: usize,
    /// The maximum number of discovered peers to attempt downloads from
    /// for each target block.
    pub max_discovered_peers_to_try_download: usize,
}

impl<Tx, RuntimeServiceId> LibP2pAdapter<Tx, RuntimeServiceId>
where
    Tx: Clone + Eq + Serialize,
{
    async fn subscribe(relay: &Relay<Libp2p, RuntimeServiceId>, topic: &str) {
        if let Err((e, _)) = relay
            .send(NetworkMsg::Process(Command::PubSub(Subscribe(
                topic.into(),
            ))))
            .await
        {
            tracing::error!("error subscribing to {topic}: {e}");
        }
    }

    async fn get_connected_peers(
        relay: &Relay<Libp2p, RuntimeServiceId>,
    ) -> Result<HashSet<PeerId>, DynError> {
        let (reply_sender, receiver) = oneshot::channel();
        if let Err((e, _)) = relay
            .send(NetworkMsg::Process(Command::Network(
                NetworkCommand::ConnectedPeers {
                    reply: reply_sender,
                },
            )))
            .await
        {
            return Err(Box::new(e));
        }

        let connected_peers = receiver.await.map_err(|e| Box::new(e) as DynError)?;
        Ok(connected_peers)
    }

    async fn get_discovered_peers(
        relay: &Relay<Libp2p, RuntimeServiceId>,
    ) -> Result<HashSet<PeerId>, DynError> {
        let (reply_sender, receiver) = oneshot::channel();
        if let Err((e, _)) = relay
            .send(NetworkMsg::Process(Command::Discovery(
                DiscoveryCommand::GetDiscoveredPeers {
                    reply: reply_sender,
                },
            )))
            .await
        {
            return Err(Box::new(e));
        }

        let discovered_peers = receiver.await.map_err(|e| Box::new(e) as DynError)?;

        Ok(discovered_peers)
    }
}

#[async_trait::async_trait]
impl<Tx, RuntimeServiceId> NetworkAdapter<RuntimeServiceId> for LibP2pAdapter<Tx, RuntimeServiceId>
where
    Tx: AuthenticatedMantleTx + Serialize + DeserializeOwned + Clone + Eq + Send + Sync + 'static,
{
    type Backend = Libp2p;
    type Settings = LibP2pAdapterSettings;
    type PeerId = PeerId;
    type Block = Block<Tx>;
    type Proposal = Proposal;

    async fn new(settings: Self::Settings, network_relay: Relay<Libp2p, RuntimeServiceId>) -> Self {
        let relay = network_relay.clone();
        Self::subscribe(&relay, settings.topic.as_str()).await;
        tracing::trace!("Starting up...");
        // this wait seems to be helpful in some cases since we give the time
        // to the network to establish connections before we start sending messages
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        Self {
            network_relay,
            settings,
            _phantom_tx: PhantomData,
        }
    }

    async fn proposals_stream(&self) -> Result<BoxedStream<Self::Proposal>, DynError> {
        let (sender, receiver) = oneshot::channel();
        if let Err((e, _)) = self
            .network_relay
            .send(NetworkMsg::SubscribeToPubSub { sender })
            .await
        {
            return Err(Box::new(e));
        }
        let stream = receiver.await.map_err(Box::new)?;
        Ok(Box::new(stream.filter_map(|message| match message {
            Ok(message) => NetworkMessage::from_bytes(&message.data).map_or_else(
                |_| {
                    tracing::trace!("unrecognized gossipsub message");
                    None
                },
                |msg| match msg {
                    NetworkMessage::Proposal(proposal) => Some(proposal),
                },
            ),
            Err(BroadcastStreamRecvError::Lagged(n)) => {
                tracing::error!("lagged messages: {n}");
                None
            }
        })))
    }

    async fn chainsync_events_stream(&self) -> Result<BoxedStream<ChainSyncEvent>, DynError> {
        let (sender, receiver) = oneshot::channel();

        if let Err((e, _)) = self
            .network_relay
            .send(NetworkMsg::SubscribeToChainSync { sender })
            .await
        {
            return Err(Box::new(e));
        }

        let stream = receiver.await.map_err(Box::new)?;
        Ok(Box::new(stream.filter_map(|event| {
            event
                .map_err(|e| tracing::error!("lagged messages: {e}"))
                .ok()
        })))
    }

    async fn request_tip(&self, peer: Self::PeerId) -> Result<GetTipResponse, DynError> {
        let started_at = Instant::now();
        let (reply_sender, receiver) = oneshot::channel();
        if let Err((e, _)) = self
            .network_relay
            .send(NetworkMsg::Process(Command::ChainSync(
                ChainSyncCommand::RequestTip { peer, reply_sender },
            )))
            .await
        {
            return Err(Box::new(e));
        }

        let response = receiver
            .await
            .map_err(Into::into)
            .and_then(|response| response.map_err(Into::into));

        metrics::chainsync_observe_request_tip(started_at.elapsed(), response)
    }

    async fn request_blocks_from_peer(
        &self,
        peer: Self::PeerId,
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: HashSet<HeaderId>,
    ) -> Result<BoxedStream<Result<(HeaderId, Self::Block), DynError>>, DynError> {
        let (reply_sender, receiver) = oneshot::channel();
        if let Err((e, _)) = self
            .network_relay
            .send(NetworkMsg::Process(Command::ChainSync(
                ChainSyncCommand::DownloadBlocks {
                    peer,
                    target_block,
                    local_tip,
                    latest_immutable_block,
                    additional_blocks,
                    reply_sender,
                },
            )))
            .await
        {
            return Err(Box::new(e));
        }

        let stream = receiver.await?;
        let stream = stream.map_err(|e| Box::new(e) as DynError).map(|result| {
            let block = result?;
            let block: Self::Block =
                Block::from_bytes(&block).map_err(|e| Box::new(e) as DynError)?;
            Ok((block.header().id(), block))
        });

        Ok(Box::new(stream))
    }

    /// Attempts to open a stream of blocks from a locally known block to the
    /// `target_block` block.
    async fn request_blocks_from_peers(
        &self,
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: HashSet<HeaderId>,
    ) -> Result<BoxedStream<Result<(HeaderId, Self::Block), DynError>>, DynError> {
        let connected_peers = Self::get_connected_peers(&self.network_relay).await?;

        // All peers we know about, including those that are not connected.
        let discovered_peers = Self::get_discovered_peers(&self.network_relay).await?;

        let peers_to_request = choose_peers_to_request_download(
            &connected_peers,
            self.settings.max_connected_peers_to_try_download,
            &discovered_peers,
            self.settings.max_discovered_peers_to_try_download,
        );

        let requests = peers_to_request
            .into_iter()
            .map(|peer| {
                let additional_blocks = additional_blocks.clone();
                async move {
                    let stream = self
                        .request_blocks_from_peer(
                            peer,
                            target_block,
                            local_tip,
                            latest_immutable_block,
                            additional_blocks,
                        )
                        .await?;

                    debug!("Requested orphan parents from peer: {peer}");

                    Ok(stream)
                }
                .boxed()
            })
            .collect::<Vec<_>>();

        select_ok(requests).await.map(|(stream, _)| stream)
    }
}

/// Selects peers to attempt downloads from.
///
/// Returns at most `max_connected_peers + max_discovered_peers` peers in total:
/// - at most `max_connected_peers` from the `connected_peers` set
/// - at most `max_discovered_peers` from the `discovered_peers -
///   connected_peers` set
fn choose_peers_to_request_download<PeerId>(
    connected_peers: &HashSet<PeerId>,
    max_connected_peers: usize,
    discovered_peers: &HashSet<PeerId>,
    max_discovered_peers: usize,
) -> impl Iterator<Item = PeerId>
where
    PeerId: Eq + Hash + Copy,
{
    let mut rng = thread_rng();

    // select from discovered-but-not-connected peers
    let discovered_selected = discovered_peers
        .difference(connected_peers)
        .copied()
        .choose_multiple(&mut rng, max_discovered_peers);

    // select from connected peers
    let connected_selected = connected_peers
        .iter()
        .copied()
        .choose_multiple(&mut rng, max_connected_peers);

    discovered_selected.into_iter().chain(connected_selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_peers() {
        // `3` is in both connected and discovered sets
        let connected = HashSet::from_iter(vec![[1; 32], [2; 32], [3; 32]]);
        let discovered = HashSet::from_iter(vec![[3; 32], [4; 32], [5; 32]]);

        let result =
            choose_peers_to_request_download(&connected, 2, &discovered, 2).collect::<Vec<_>>();

        assert_eq!(result.len(), 4);
        // all discovered peers except `3` must be returned
        assert!(result.contains(&[4; 32]));
        assert!(result.contains(&[5; 32]));
        // other selected peers must be from the connected set
        result
            .iter()
            .filter(|&id| ![[4; 32], [5; 32]].contains(id))
            .for_each(|id| {
                assert!(
                    connected.contains(id),
                    "must be selected from connected peers: id={id:?}"
                );
            });
    }

    #[test]
    fn choose_peers_zero_max() {
        // `3` is in both connected and discovered sets
        let connected = HashSet::from_iter(vec![[1; 32], [2; 32], [3; 32]]);
        let discovered = HashSet::from_iter(vec![[3; 32], [4; 32], [5; 32]]);

        // set max=0 for connected peers
        let result =
            choose_peers_to_request_download(&connected, 0, &discovered, 2).collect::<Vec<_>>();

        assert_eq!(result.len(), 2);
        // all discovered peers except `3` must be returned
        assert!(result.contains(&[4; 32]));
        assert!(result.contains(&[5; 32]));

        // set max=0 for discovered peers
        let result =
            choose_peers_to_request_download(&connected, 2, &discovered, 0).collect::<Vec<_>>();

        assert_eq!(result.len(), 2);
        // all selected peers must be from the connected set
        for id in &result {
            assert!(
                connected.contains(id),
                "must be selected from connected peers: id={id:?}"
            );
        }
    }

    #[test]
    fn choose_peers_less_than_max() {
        // `3` is in both connected and discovered sets
        let connected = HashSet::from_iter(vec![[1; 32], [2; 32], [3; 32]]);
        let discovered = HashSet::from_iter(vec![[3; 32], [4; 32], [5; 32]]);

        // set max=4 larger than # of connected peers
        let result =
            choose_peers_to_request_download(&connected, 4, &discovered, 0).collect::<Vec<_>>();

        // all connected peers must be returned
        assert_eq!(result.len(), connected.len());
        assert!(result.contains(&[1; 32]));
        assert!(result.contains(&[2; 32]));
        assert!(result.contains(&[3; 32]));

        // set max=3 larger than # of `discovered - connected` peers
        let result =
            choose_peers_to_request_download(&connected, 0, &discovered, 2).collect::<Vec<_>>();

        // all discovered peers except `3` must be returned
        assert_eq!(result.len(), 2);
        assert!(result.contains(&[4; 32]));
        assert!(result.contains(&[5; 32]));
    }
}
