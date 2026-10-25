use std::{
    collections::HashSet, fmt::Debug, hash::Hash, iter, marker::PhantomData, mem, time::Instant,
};

use futures::{FutureExt as _, TryStreamExt as _, future::select_ok, stream};
use lb_binary_codec::canonical::BinaryDecodeExt as _;
use lb_core::{
    block::{Block, Proposal},
    header::HeaderId,
    mantle::{
        ledger::verification_mode::StandardMode,
        traits::{SignedMantleTx, StorageSize},
        transactions::states::Preverified,
    },
};
use lb_cryptarchia_engine::{Era, EraSchedule};
use lb_cryptarchia_sync::GetTipResponse;
use lb_log_targets::chain;
use lb_network_service::{
    backends::libp2p::{
        ChainSyncCommand, Command, DiscoveryCommand, Libp2p, NetworkCommand, PeerId,
        PubSubCommand::{Subscribe, Unsubscribe},
        TopicHash,
    },
    message::{BackendNetworkMsg, ChainSyncEvent, NetworkMsg},
};
use overwatch::{DynError, services::relay::OutboundRelay};
use rand::{seq::IteratorRandom as _, thread_rng};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;
use tokio_stream::{StreamExt as _, wrappers::errors::BroadcastStreamRecvError};

use crate::{
    metrics,
    network::{BoxedStream, NetworkAdapter},
};

type Relay<T, RuntimeServiceId> = OutboundRelay<BackendNetworkMsg<T, RuntimeServiceId>>;
type BlockStreamItem<Tx> = Result<(HeaderId, Block<Tx>), DynError>;
type FirstBlockResponse<Tx> = Result<Option<BlockStreamItem<Tx>>, DynError>;
type BlockDownloadStream<Tx> = BoxedStream<BlockStreamItem<Tx>>;

const LOG_TARGET: &str = chain::network::LIBP2P;

#[derive(Clone)]
pub struct LibP2pAdapter<Tx, RuntimeServiceId>
where
    Tx: Clone + Eq,
{
    network_relay: OutboundRelay<BackendNetworkMsg<Libp2p, RuntimeServiceId>>,
    settings: LibP2pAdapterSettings,
    /// The era the adapter is in.
    current_era: Era,
    /// The topic of the era the adapter is in.
    current_topic: String,
    /// The topic of the era before, which the adapter stays subscribed to
    /// until the end of its transition period.
    previous_topic: Option<String>,
    _phantom_tx: PhantomData<Tx>,
}

/// The settings of the libp2p adapter: those it keeps for the whole run, and
/// those that change from era to era.
#[derive(Debug, Clone)]
pub struct LibP2pAdapterSettings {
    /// The maximum number of connected peers to attempt downloads from
    /// for each target block.
    pub max_connected_peers_to_try_download: usize,
    /// The maximum number of discovered peers to attempt downloads from
    /// for each target block.
    pub max_discovered_peers_to_try_download: usize,
    pub era_schedule: EraSchedule<LibP2pAdapterEraSettings>,
}

/// What the libp2p adapter runs with in an era.
#[derive(Debug, Clone)]
pub struct LibP2pAdapterEraSettings {
    /// The topic block proposals are gossiped on.
    pub topic: String,
}

impl<Tx, RuntimeServiceId> LibP2pAdapter<Tx, RuntimeServiceId>
where
    Tx: Clone + Eq + Serialize,
{
    // Requests a blocks stream from a single peer and validates the first item
    // before this peer is considered a successful candidate by `select_ok`.
    //
    // Behavior:
    // - If the first item is any error, returns `Err` so this peer is excluded from
    //   winner selection.
    // - Otherwise, returns a reconstructed stream where the first item is put back
    //   (`iter([first_item]).chain(stream)`) so downstream consumers see the full
    //   original stream.
    // - If the stream is immediately exhausted (`None`), returns it unchanged.
    fn check_first_block_response_ready(
        first_item: Option<BlockStreamItem<Tx>>,
    ) -> FirstBlockResponse<Tx> {
        match first_item {
            Some(Err(err)) => Err(err),
            Some(first_item) => Ok(Some(first_item)),
            None => Ok(None),
        }
    }

    async fn request_available_blocks_stream_from_peer(
        &self,
        peer: PeerId,
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: HashSet<HeaderId>,
    ) -> Result<BlockDownloadStream<Tx>, DynError>
    where
        Tx: SignedMantleTx<Preverified, StandardMode>
            + StorageSize
            + Serialize
            + DeserializeOwned
            + Clone
            + Eq
            + Send
            + Sync
            + 'static,
    {
        let mut stream = self
            .request_blocks_from_peer(
                peer,
                target_block,
                local_tip,
                latest_immutable_block,
                additional_blocks,
            )
            .await?;

        match Self::check_first_block_response_ready(stream.next().await)? {
            Some(first_item) => {
                let rebuilt = tokio_stream::iter([first_item]).chain(stream);
                Ok(Box::new(rebuilt))
            }
            None => Ok(stream),
        }
    }

    async fn subscribe(relay: &Relay<Libp2p, RuntimeServiceId>, topic: &str) {
        if let Err(error) = relay
            .send(NetworkMsg::Process(Command::PubSub(Subscribe(
                topic.into(),
            ))))
            .await
        {
            tracing::error!(target: LOG_TARGET, "error subscribing to {topic}: {error}");
        }
    }

    async fn unsubscribe(relay: &Relay<Libp2p, RuntimeServiceId>, topic: &str) {
        if let Err(error) = relay
            .send(NetworkMsg::Process(Command::PubSub(Unsubscribe(
                topic.into(),
            ))))
            .await
        {
            tracing::error!(target: LOG_TARGET, "error unsubscribing from {topic}: {error}");
        }
    }

    async fn get_connected_peers(
        relay: &Relay<Libp2p, RuntimeServiceId>,
    ) -> Result<HashSet<PeerId>, DynError> {
        let (reply_sender, receiver) = oneshot::channel();
        if let Err(error) = relay
            .send(NetworkMsg::Process(Command::Network(
                NetworkCommand::ConnectedPeers {
                    reply: reply_sender,
                },
            )))
            .await
        {
            return Err(Box::new(error));
        }

        let connected_peers = receiver.await.map_err(|e| Box::new(e) as DynError)?;
        Ok(connected_peers)
    }

    async fn get_chainsync_eligible_peers(
        relay: &Relay<Libp2p, RuntimeServiceId>,
    ) -> Result<HashSet<PeerId>, DynError> {
        let (reply_sender, receiver) = oneshot::channel();
        if let Err(error) = relay
            .send(NetworkMsg::Process(Command::ChainSync(
                ChainSyncCommand::EligiblePeers { reply_sender },
            )))
            .await
        {
            return Err(Box::new(error));
        }

        receiver.await.map_err(|e| Box::new(e) as DynError)
    }

    async fn get_discovered_peers(
        relay: &Relay<Libp2p, RuntimeServiceId>,
    ) -> Result<HashSet<PeerId>, DynError> {
        let (reply_sender, receiver) = oneshot::channel();
        if let Err(error) = relay
            .send(NetworkMsg::Process(Command::Discovery(
                DiscoveryCommand::GetDiscoveredPeers {
                    reply: reply_sender,
                },
            )))
            .await
        {
            return Err(Box::new(error));
        }

        let discovered_peers = receiver.await.map_err(|e| Box::new(e) as DynError)?;

        Ok(discovered_peers)
    }
}

#[async_trait::async_trait]
impl<Tx, RuntimeServiceId> NetworkAdapter<RuntimeServiceId> for LibP2pAdapter<Tx, RuntimeServiceId>
where
    Tx: SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Serialize
        + DeserializeOwned
        + Clone
        + Eq
        + Send
        + Sync
        + 'static,
{
    type Backend = Libp2p;
    type Settings = LibP2pAdapterSettings;
    type PeerId = PeerId;
    type Block = Block<Tx>;
    type Proposal = Proposal;

    async fn new(
        settings: Self::Settings,
        current_era: Era,
        network_relay: Relay<Libp2p, RuntimeServiceId>,
    ) -> Self {
        let topic = settings
            .era_schedule
            .get(current_era)
            .expect("The era the adapter starts in should be scheduled")
            .parameters
            .topic
            .clone();
        let relay = network_relay.clone();
        tracing::debug!(
            target: LOG_TARGET,
            "Subscribing chain-network adapter to pubsub topic {topic}"
        );
        Self::subscribe(&relay, topic.as_str()).await;
        tracing::trace!(target: LOG_TARGET, "Starting up...");
        // this wait seems to be helpful in some cases since we give the time
        // to the network to establish connections before we start sending messages
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        Self {
            network_relay,
            settings,
            current_era,
            current_topic: topic,
            previous_topic: None,
            _phantom_tx: PhantomData,
        }
    }

    async fn rotate_era(&mut self, era: Era) {
        let Some(era_settings) = self.settings.era_schedule.get(era) else {
            tracing::error!(
                target: LOG_TARGET,
                "Asked to enter era {}, which is not scheduled",
                era.into_inner()
            );
            return;
        };
        self.current_era = era;
        let new_era_topic = &era_settings.parameters.topic;
        if new_era_topic == &self.current_topic {
            return;
        }
        tracing::info!(
            target: LOG_TARGET,
            "Entered era {}: block proposals are gossiped on {new_era_topic}",
            era.into_inner()
        );
        // A topic of an earlier era still held has had its transition period
        // end already: only the report of its end went missing.
        if let Some(lingering_topic) = self.previous_topic.take() {
            tracing::warn!(
                target: LOG_TARGET,
                "The end of the previous era's transition period was never reported: block proposals are no longer received on {lingering_topic}"
            );
            Self::unsubscribe(&self.network_relay, &lingering_topic).await;
        }
        let previous_topic = mem::replace(&mut self.current_topic, new_era_topic.clone());
        Self::subscribe(&self.network_relay, &self.current_topic).await;
        // Proposals broadcast by peers that have not entered the new era yet
        // still arrive on the previous era's topic, so the adapter stays
        // subscribed to it until its transition period ends.
        self.previous_topic = Some(previous_topic);
    }

    async fn complete_era_transition(&mut self, ended_era: Era) {
        if self.current_era.checked_sub(1) != Some(ended_era) {
            return;
        }
        let Some(previous_topic) = self.previous_topic.take() else {
            return;
        };
        tracing::info!(
            target: LOG_TARGET,
            "Transition period over: block proposals are no longer received on {previous_topic}"
        );
        Self::unsubscribe(&self.network_relay, &previous_topic).await;
    }

    async fn proposals_stream(&self) -> Result<BoxedStream<Self::Proposal>, DynError> {
        let (sender, receiver) = oneshot::channel();
        if let Err(error) = self
            .network_relay
            .send(NetworkMsg::SubscribeToPubSub { sender })
            .await
        {
            return Err(Box::new(error));
        }
        // Gossipsub only hands over the messages of the topics the node is
        // subscribed to: the topic of the adapter's current era, and the topic
        // of the era before during its transition period. So the stream keeps
        // the messages of every era's topic, and follows the adapter from era
        // to era.
        let topic_hashes: HashSet<TopicHash> = self
            .settings
            .era_schedule
            .iter()
            .map(|era| TopicHash::from_raw(era.parameters.topic.clone()))
            .collect();
        let stream = receiver.await.map_err(Box::new)?;
        Ok(Box::new(stream.filter_map(move |message| match message {
            Ok(message) if topic_hashes.contains(&message.topic) => {
                match Proposal::decode_all(&message.data) {
                    Ok(proposal) => Some(proposal),
                    Err(e) => {
                        tracing::debug!(target: LOG_TARGET, "unrecognized gossipsub message: {e}");
                        None
                    }
                }
            }
            Ok(_) => None,
            Err(BroadcastStreamRecvError::Lagged(n)) => {
                tracing::error!(target: LOG_TARGET, "lagged messages: {n}");
                None
            }
        })))
    }

    async fn chainsync_events_stream(&self) -> Result<BoxedStream<ChainSyncEvent>, DynError> {
        let (sender, receiver) = oneshot::channel();

        if let Err(error) = self
            .network_relay
            .send(NetworkMsg::SubscribeToChainSync { sender })
            .await
        {
            return Err(Box::new(error));
        }

        let stream = receiver.await.map_err(Box::new)?;
        Ok(Box::new(stream.filter_map(|event| {
            event
                .map_err(|e| tracing::error!(target: LOG_TARGET, "lagged messages: {e}"))
                .ok()
        })))
    }

    async fn request_tip(&self, peer: Self::PeerId) -> Result<GetTipResponse, DynError> {
        let started_at = Instant::now();
        tracing::debug!(target: LOG_TARGET, "Requesting chain tip from peer {peer:?}");
        let (reply_sender, receiver) = oneshot::channel();
        if let Err(error) = self
            .network_relay
            .send(NetworkMsg::Process(Command::ChainSync(
                ChainSyncCommand::RequestTip { peer, reply_sender },
            )))
            .await
        {
            return Err(Box::new(error));
        }

        let response = receiver
            .await
            .map_err(Into::into)
            .and_then(|response| response.map_err(Into::into));

        metrics::chainsync_observe_request_tip(started_at.elapsed(), response)
    }

    async fn sample_tips(&self, max_peers: usize) -> BoxedStream<GetTipResponse> {
        use futures::stream::StreamExt as FuturesStreamExt;
        let connected_peers = match Self::get_connected_peers(&self.network_relay).await {
            Ok(peers) => peers,
            Err(e) => {
                tracing::warn!(target: LOG_TARGET, "tip poll: failed to fetch connected peers: {e}");
                return Box::new(stream::empty::<GetTipResponse>());
            }
        };
        let chainsync_peers = match Self::get_chainsync_eligible_peers(&self.network_relay).await {
            Ok(peers) => peers,
            Err(e) => {
                tracing::warn!(target: LOG_TARGET, "tip poll: failed to fetch chainsync-eligible peers: {e}");
                return Box::new(stream::empty::<GetTipResponse>());
            }
        };
        let connected_peers = connected_peers
            .intersection(&chainsync_peers)
            .copied()
            .collect::<HashSet<_>>();

        let sampled: Vec<PeerId> = connected_peers
            .into_iter()
            .choose_multiple(&mut thread_rng(), max_peers);

        if sampled.is_empty() {
            tracing::debug!(target: LOG_TARGET, "tip poll: no connected peers to sample");
            return Box::new(stream::empty::<GetTipResponse>());
        }
        let result_stream = FuturesStreamExt::filter_map(
            stream::iter(
                sampled
                    .into_iter()
                    .zip(iter::repeat(self.network_relay.clone())),
            ),
            async |(peer, relay)| {
                let (reply_sender, receiver) = oneshot::channel();
                if let Err(error) = relay
                    .send(NetworkMsg::Process(Command::ChainSync(
                        ChainSyncCommand::RequestTip { peer, reply_sender },
                    )))
                    .await
                {
                    tracing::debug!(target: LOG_TARGET, "tip poll: failed to send GetTip to peer {peer:?}: {error}");
                    None
                } else {
                    match receiver.await.ok() {
                        None => None,
                        Some(Err(e)) => {
                            tracing::debug!(
                                target: LOG_TARGET,
                                "tip poll: GetTip request to peer {peer:?} failed: {e}"
                            );
                            None
                        }
                        Some(Ok(tip)) => Some(tip),
                    }
                }
            },
        );
        Box::new(Box::pin(result_stream))
    }

    async fn request_blocks_from_peer(
        &self,
        peer: Self::PeerId,
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: HashSet<HeaderId>,
    ) -> Result<BoxedStream<Result<(HeaderId, Self::Block), DynError>>, DynError> {
        let additional_blocks_len = additional_blocks.len();
        tracing::debug!(
            target: LOG_TARGET,
            "Requesting blocks from peer {peer:?} for target block {target_block:?} from local tip {local_tip:?} with immutable block {latest_immutable_block:?} and {additional_blocks_len} additional blocks"
        );
        let (reply_sender, receiver) = oneshot::channel();
        if let Err(error) = self
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
            return Err(Box::new(error));
        }

        let stream = receiver.await?;
        let stream = stream.map_err(|e| Box::new(e) as DynError).map(|result| {
            let block = result?;
            let block: Self::Block = Block::try_from(block).map_err(|e| Box::new(e) as DynError)?;
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

        let chainsync_peers = Self::get_chainsync_eligible_peers(&self.network_relay).await?;

        // All peers we know about, including those that are not connected.
        let discovered_peers = Self::get_discovered_peers(&self.network_relay).await?;

        // Discovery membership and protocol capability are independent.
        let peers_to_request = choose_eligible_peers_to_request_download(
            &connected_peers,
            &discovered_peers,
            &chainsync_peers,
            self.settings.max_connected_peers_to_try_download,
            self.settings.max_discovered_peers_to_try_download,
        );
        tracing::debug!(
            target: LOG_TARGET,
            "Selecting peers for target block {target_block:?} from local tip {local_tip:?} with \
            immutable block {latest_immutable_block:?}; selected_peers={peers_to_request:?}, \
            connected={}, discovered={}, eligible_connected={}, eligible_discovered={}, \
            additional_blocks={}",
            connected_peers.len(),
            discovered_peers.len(),
            connected_peers.intersection(&chainsync_peers).count(),
            discovered_peers.intersection(&chainsync_peers).count(),
            additional_blocks.len()
        );

        if peers_to_request.is_empty() {
            return Err(format!(
                "no candidate peers available to download orphan ancestors for target block \
                {target_block:?} (connected={}, discovered={}, eligible_connected={}, \
                eligible_discovered={})",
                connected_peers.len(),
                discovered_peers.len(),
                connected_peers.intersection(&chainsync_peers).count(),
                discovered_peers.intersection(&chainsync_peers).count()
            )
            .into());
        }

        let requests = peers_to_request
            .into_iter()
            .map(|peer| {
                let additional_blocks = additional_blocks.clone();
                async move {
                    let stream = self
                        .request_available_blocks_stream_from_peer(
                            peer,
                            target_block,
                            local_tip,
                            latest_immutable_block,
                            additional_blocks,
                        )
                        .await?;

                    tracing::debug!(target: LOG_TARGET, "received a stream of orphan parents from peer: {peer}");

                    Ok(stream)
                }
                .boxed()
            })
            .collect::<Vec<_>>();

        // First peer with a validated first response wins
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

fn choose_eligible_peers_to_request_download<PeerId>(
    connected_peers: &HashSet<PeerId>,
    discovered_peers: &HashSet<PeerId>,
    eligible_peers: &HashSet<PeerId>,
    max_connected_peers: usize,
    max_discovered_peers: usize,
) -> Vec<PeerId>
where
    PeerId: Eq + Hash + Copy,
{
    let connected_peers = connected_peers
        .intersection(eligible_peers)
        .copied()
        .collect::<HashSet<_>>();
    let discovered_peers = discovered_peers
        .intersection(eligible_peers)
        .copied()
        .collect::<HashSet<_>>();

    choose_peers_to_request_download(
        &connected_peers,
        max_connected_peers,
        &discovered_peers,
        max_discovered_peers,
    )
    .collect()
}

#[cfg(test)]
mod tests {
    use lb_cryptarchia_sync::{BlocksUnavailableReason, ChainSyncError, ChainSyncErrorKind};

    use super::*;

    #[test]
    fn validate_first_block_response_rejects_block_not_found() {
        let block_not_found = ChainSyncError::new(
            PeerId::random(),
            ChainSyncErrorKind::BlockProviderUnavailable(BlocksUnavailableReason::BlockNotFound(
                HeaderId::from([9u8; 32]),
            )),
        );
        let first_item: Result<(HeaderId, Block<()>), DynError> = Err(Box::new(block_not_found));

        assert!(
            LibP2pAdapter::<(), ()>::check_first_block_response_ready(Some(first_item)).is_err()
        );
    }

    #[test]
    fn validate_first_block_response_rejects_other_provider_errors() {
        let unknown = ChainSyncError::new(
            PeerId::random(),
            ChainSyncErrorKind::BlockProviderUnavailable(BlocksUnavailableReason::Unknown),
        );
        let first_item: Result<(HeaderId, Block<()>), DynError> = Err(Box::new(unknown));

        assert!(
            LibP2pAdapter::<(), ()>::check_first_block_response_ready(Some(first_item)).is_err()
        );
    }

    #[test]
    fn validate_first_block_response_accepts_empty_stream() {
        assert!(
            LibP2pAdapter::<(), ()>::check_first_block_response_ready(None)
                .unwrap()
                .is_none()
        );
    }

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
            .filter(|&id| id != &[4; 32] && id != &[5; 32])
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
        // all selected peers must be from the discovered-but-not-connected set
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
            choose_peers_to_request_download(&connected, 0, &discovered, 3).collect::<Vec<_>>();

        // all discovered peers except `3` must be returned
        assert_eq!(result.len(), 2);
        assert!(result.contains(&[4; 32]));
        assert!(result.contains(&[5; 32]));
    }

    #[test]
    fn chainsync_filter_is_applied_before_candidate_limits() {
        let connected: HashSet<[i32; 32]> = HashSet::from_iter(vec![[1; 32], [2; 32]]);
        let discovered: HashSet<[i32; 32]> = HashSet::from_iter(vec![[3; 32], [4; 32]]);
        let eligible: HashSet<[i32; 32]> = HashSet::from_iter(vec![[2; 32], [4; 32]]);

        let result =
            choose_eligible_peers_to_request_download(&connected, &discovered, &eligible, 1, 1);

        assert_eq!(result, vec![[4; 32], [2; 32]]);
    }
}
