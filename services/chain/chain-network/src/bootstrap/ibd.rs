use std::{collections::HashSet, fmt::Debug, hash::Hash, marker::PhantomData};

use futures::StreamExt as _;
use lb_chain_service::{
    CryptarchiaInfo,
    api::{CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_core::{
    block::Block,
    header::HeaderId,
    mantle::{AuthenticatedMantleTx, TxHash},
};
use lb_cryptarchia_sync::GetTipResponse;
use lb_tx_service::backend::RecoverableMempool;
use overwatch::DynError;
use tracing::{debug, error, warn};

use crate::{
    Error as ChainError, IbdConfig,
    bootstrap::download::{Delay, Download, Downloads, DownloadsOutput},
    mempool::adapter::MempoolAdapter,
    network::NetworkAdapter,
};

pub trait IbdBlockProcessor<B> {
    async fn info(&self) -> Result<CryptarchiaInfo, Error>;
    async fn process_block(&mut self, block: B) -> Result<(), Error>;
    async fn has_processed_block(&self, header: HeaderId) -> Result<bool, Error>;
}

pub struct ChainNetworkIbdBlockProcessor<Cryptarchia, Mempool, RuntimeServiceId>
where
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx: AuthenticatedMantleTx + Debug + Clone + Send + Sync,
    Mempool:
        RecoverableMempool<BlockId = HeaderId, Key = TxHash, Item = Cryptarchia::Tx> + Send + Sync,
    RuntimeServiceId: Send + Sync,
{
    pub cryptarchia: CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    pub mempool_adapter: MempoolAdapter<Mempool::Item>,
}

impl<Cryptarchia, Mempool, RuntimeServiceId> IbdBlockProcessor<Block<Cryptarchia::Tx>>
    for ChainNetworkIbdBlockProcessor<Cryptarchia, Mempool, RuntimeServiceId>
where
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx: AuthenticatedMantleTx + Debug + Clone + Send + Sync,
    Mempool:
        RecoverableMempool<BlockId = HeaderId, Key = TxHash, Item = Cryptarchia::Tx> + Send + Sync,
    RuntimeServiceId: Send + Sync,
{
    async fn info(&self) -> Result<CryptarchiaInfo, Error> {
        Ok(self.cryptarchia.info().await?)
    }

    async fn process_block(&mut self, block: Block<Cryptarchia::Tx>) -> Result<(), Error> {
        crate::apply_block_and_reconcile_mempool::<_, Mempool, _>(
            block,
            &self.cryptarchia,
            &self.mempool_adapter,
        )
        .await
        .map_err(|e| {
            error!("Error processing block during IBD: {:?}", e);
            Error::from(e)
        })
    }

    async fn has_processed_block(&self, block_id: HeaderId) -> Result<bool, Error> {
        Ok(self.cryptarchia.get_ledger_state(block_id).await?.is_some())
    }
}

pub struct InitialBlockDownload<NetAdapter, BlockProcessor, RuntimeServiceId>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId>,
    NetAdapter::PeerId: Clone + Eq + Hash,
    BlockProcessor: IbdBlockProcessor<NetAdapter::Block>,
{
    block_processor: BlockProcessor,
    network: NetAdapter,
    synced_peers: HashSet<NetAdapter::PeerId>,
    _phantom: PhantomData<RuntimeServiceId>,
}

impl<NetAdapter, BlockProcessor, RuntimeServiceId>
    InitialBlockDownload<NetAdapter, BlockProcessor, RuntimeServiceId>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId>,
    NetAdapter::PeerId: Clone + Eq + Hash,
    BlockProcessor: IbdBlockProcessor<NetAdapter::Block>,
{
    pub fn new(block_processor: BlockProcessor, network: NetAdapter) -> Self {
        Self {
            block_processor,
            network,
            synced_peers: HashSet::new(),
            _phantom: PhantomData,
        }
    }
}

impl<NetAdapter, BlockProcessor, RuntimeServiceId>
    InitialBlockDownload<NetAdapter, BlockProcessor, RuntimeServiceId>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId> + Send + Sync,
    NetAdapter::PeerId: Copy + Clone + Eq + Hash + Debug + Send + Sync + Unpin,
    NetAdapter::Block: Debug + Unpin,
    BlockProcessor: IbdBlockProcessor<NetAdapter::Block> + Sync,
    RuntimeServiceId: Sync,
{
    /// Runs IBD with the configured peers.
    ///
    /// It downloads blocks from the peers, and applies them to the
    /// [`Cryptarchia`].
    ///
    /// An updated [`Cryptarchia`] is returned after all downloads
    /// have completed from all peers except the failed ones.
    ///
    /// An error is returned if downloads fail from all peers.
    pub async fn run(
        mut self,
        config: IbdConfig<NetAdapter::PeerId>,
    ) -> Result<BlockProcessor, Error> {
        if config.peers.is_empty() {
            warn!("Skipping IBD as no peers are configured");
            return Ok(self.block_processor);
        }

        let downloads = self.initiate_downloads(config).await?;
        self.proceed_downloads(downloads).await
    }

    /// Initiates [`Downloads`] from the configured peers.
    async fn initiate_downloads<'a>(
        &mut self,
        config: IbdConfig<NetAdapter::PeerId>,
    ) -> Result<Downloads<'a, NetAdapter::PeerId, NetAdapter::Block>, Error>
    where
        NetAdapter::PeerId: 'a,
        NetAdapter::Block: 'a,
    {
        let mut downloads = Downloads::new(config.delay_before_new_download);
        for peer in &config.peers {
            match self.initiate_download(*peer, None).await {
                Ok(Some(download)) => {
                    self.start_download(download, &mut downloads);
                }
                Ok(None) => {
                    debug!("No download needed for {peer:?}. Delaying the peer");
                    downloads.add_delay(Delay::new(*peer, None));
                }
                Err(e) => {
                    error!("Failed to initiate download for {peer:?}: {e}");
                }
            }
        }

        if downloads.is_empty() {
            Err(Error::AllPeersFailed)
        } else {
            Ok(downloads)
        }
    }

    /// Initiates a [`Download`] from a specific peer.
    ///
    /// It gets the peer's tip, and requests a block stream to reach the tip.
    ///
    /// If the peer's tip already exists in local, or if there is any duplicate
    /// download for the tip, no download is initiated and [`None`] is returned.
    ///
    /// If communication fails, an [`Error`] is returned.
    async fn initiate_download(
        &mut self,
        peer: NetAdapter::PeerId,
        latest_downloaded_block: Option<HeaderId>,
    ) -> Result<Option<Download<NetAdapter::PeerId, NetAdapter::Block>>, Error> {
        // Get the most recent peer's tip.
        let tip_response = self
            .network
            .request_tip(peer)
            .await
            .map_err(Error::BlockProvider)?;

        // Use the peer's tip as the target for the download.
        let target = match tip_response {
            GetTipResponse::Tip { tip, .. } => tip,
            GetTipResponse::Failure(reason) => {
                return Err(Error::BlockProvider(DynError::from(reason)));
            }
        };

        if self.block_processor.has_processed_block(target).await? {
            debug!(
                "No download needed for {peer:?} as target block already exists locally: {target:?}"
            );
            self.synced_peers.insert(peer);
            return Ok(None);
        }

        let initial_cryptarchia_info = self.block_processor.info().await?;

        // Request a block stream.
        let stream = self
            .network
            .request_blocks_from_peer(
                peer,
                target,
                initial_cryptarchia_info.tip,
                initial_cryptarchia_info.lib,
                latest_downloaded_block.map_or_else(HashSet::new, |id| HashSet::from([id])),
            )
            .await
            .map_err(Error::BlockProvider)?;

        Ok(Some(Download::new(peer, target, stream)))
    }

    /// Proceeds [`Downloads`] by reading/processing blocks.
    ///
    /// It returns the updated [`Cryptarchia`] if all downloads have
    /// completed from all peers except the failed ones.
    ///
    /// For peers that complete earlier, delays for the peers are scheduled,
    /// so that new downloads can be initiated after the delays,
    /// as long as there are other peers still in progress.
    ///
    /// An error is return if downloads fail from all peers.
    async fn proceed_downloads<'a>(
        mut self,
        mut downloads: Downloads<'a, NetAdapter::PeerId, NetAdapter::Block>,
    ) -> Result<BlockProcessor, Error>
    where
        NetAdapter::PeerId: 'a,
        NetAdapter::Block: 'a,
    {
        // Repeat until there is no download remaining,
        // even if there are delays in progress.
        while let Some(output) = downloads.next().await {
            if let Err(e) = self.handle_downloads_output(output, &mut downloads).await {
                error!("A peer was dropped from IBD due to error: {e:?}");
            }
        }

        if self.synced_peers.is_empty() {
            error!("No peers synced successfully during IBD");
            Err(Error::AllPeersFailed)
        } else {
            Ok(self.block_processor)
        }
    }

    /// Handles a [`DownloadsOutput`].
    ///
    /// In case of failure, the [`Download`] for the failed peer is dropped
    /// from the [`Downloads`] and the error is returned.
    async fn handle_downloads_output<'a>(
        &mut self,
        output: DownloadsOutput<NetAdapter::PeerId, NetAdapter::Block>,
        downloads: &mut Downloads<'a, NetAdapter::PeerId, NetAdapter::Block>,
    ) -> Result<(), Error>
    where
        NetAdapter::PeerId: 'a,
        NetAdapter::Block: 'a,
    {
        match output {
            DownloadsOutput::DelayCompleted(delay) => {
                self.handle_delay_completed(delay, downloads).await
            }
            DownloadsOutput::BlockReceived { block, download } => {
                self.handle_block_received(block, download, downloads).await
            }
            DownloadsOutput::DownloadCompleted(download) => {
                self.handle_download_completed(download, downloads).await
            }
            DownloadsOutput::Error { error, download } => {
                error!("Download failed from {:?}: {}", download.peer(), error);
                Err(Error::BlockProvider(error))
            }
        }
    }

    /// Handles a [`DownloadsOutput::BlockReceived`] by processing the block.
    async fn handle_block_received<'a>(
        &mut self,
        block: NetAdapter::Block,
        download: Download<NetAdapter::PeerId, NetAdapter::Block>,
        downloads: &mut Downloads<'a, NetAdapter::PeerId, NetAdapter::Block>,
    ) -> Result<(), Error>
    where
        NetAdapter::PeerId: 'a,
        NetAdapter::Block: 'a,
    {
        debug!(
            "Handling a block received from {:?}: {:?}",
            download.peer(),
            block
        );

        self.block_processor
            .process_block(block)
            .await
            .inspect_err(|e| {
                error!(
                    "Failed to process block from peer {:?}: {e:?}",
                    download.peer()
                );
            })?;
        self.start_download(download, downloads);
        Ok(())
    }

    /// Handles a [`DownloadsOutput::DownloadCompleted`] by trying to
    /// initiate a new download for the same peer.
    async fn handle_download_completed<'a>(
        &mut self,
        download: Download<NetAdapter::PeerId, NetAdapter::Block>,
        downloads: &mut Downloads<'a, NetAdapter::PeerId, NetAdapter::Block>,
    ) -> Result<(), Error>
    where
        NetAdapter::PeerId: 'a,
        NetAdapter::Block: 'a,
    {
        debug!(
            "A download completed for {:?}. Try a new download",
            download.peer()
        );
        self.try_initiate_download(*download.peer(), download.last(), downloads)
            .await
    }

    /// Handles a [`DownloadsOutput::DelayCompleted`] by trying to
    /// initiate a new download for the same peer.
    async fn handle_delay_completed<'a>(
        &mut self,
        delay: Delay<NetAdapter::PeerId>,
        downloads: &mut Downloads<'a, NetAdapter::PeerId, NetAdapter::Block>,
    ) -> Result<(), Error>
    where
        NetAdapter::PeerId: 'a,
        NetAdapter::Block: 'a,
    {
        debug!(
            "A delay completed for {:?}. Try a new download",
            delay.peer()
        );
        self.try_initiate_download(*delay.peer(), delay.latest_downloaded_block(), downloads)
            .await
    }

    /// Tries to initiate a download for a peer.
    ///
    /// If there is no download needed at the moment, a delay is scheduled,
    /// so that a new download can be attempted later.
    async fn try_initiate_download<'a>(
        &mut self,
        peer: NetAdapter::PeerId,
        latest_downloaded_block: Option<HeaderId>,
        downloads: &mut Downloads<'a, NetAdapter::PeerId, NetAdapter::Block>,
    ) -> Result<(), Error>
    where
        NetAdapter::PeerId: 'a,
        NetAdapter::Block: 'a,
    {
        match self
            .initiate_download(peer, latest_downloaded_block)
            .await
            .inspect_err(|e| {
                error!("Failed to initiate next download for {peer:?}: {e}");
            })? {
            Some(download) => {
                self.start_download(download, downloads);
            }
            None => {
                downloads.add_delay(Delay::new(peer, latest_downloaded_block));
            }
        }
        Ok(())
    }

    fn start_download<'a>(
        &mut self,
        download: Download<NetAdapter::PeerId, NetAdapter::Block>,
        downloads: &mut Downloads<'a, NetAdapter::PeerId, NetAdapter::Block>,
    ) where
        NetAdapter::PeerId: 'a,
        NetAdapter::Block: 'a,
    {
        self.synced_peers.remove(download.peer());
        downloads.add_download(download);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Cryptarchia(#[from] lb_chain_service::api::ApiError),
    #[error("Block provider error: {0}")]
    BlockProvider(DynError),
    #[error("All peers failed")]
    AllPeersFailed,
    #[error("Block processing failed: {0}")]
    BlockProcessing(#[from] ChainError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        iter::empty,
        num::{NonZero, NonZeroU64},
        sync::Arc,
    };

    use lb_core::{
        block::Proposal,
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::{EpochConfig, Slot, UpdatedCryptarchia};
    use lb_ledger::{
        LedgerState,
        mantle::sdp::{ServiceRewardsParameters, rewards},
    };
    use lb_network_service::{NetworkService, backends::NetworkBackend, message::ChainSyncEvent};
    use lb_utils::math::NonNegativeF64;
    use overwatch::{
        overwatch::OverwatchHandle,
        services::{ServiceData, relay::OutboundRelay},
    };
    use tokio_stream::wrappers::BroadcastStream;

    use super::*;
    use crate::network::BoxedStream;

    #[tokio::test]
    async fn no_peers_configured() {
        let block_processor = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(HashMap::new()),
        )
        .run(config(HashSet::new()))
        .await
        .unwrap();

        let cryptarchia = block_processor.cryptarchia;

        // The Cryptarchia remains unchanged.
        assert_eq!(cryptarchia.lib(), [GENESIS_ID; 32].into());
        assert_eq!(cryptarchia.tip(), [GENESIS_ID; 32].into());
    }

    #[tokio::test]
    async fn single_download() {
        let peer = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                Block::new(2, 1, 2, 2),
            ],
            Ok(Block::new(2, 1, 2, 2)),
            2,
            false,
        );
        let block_processor = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(HashMap::from([(NodeId(0), peer.clone())])),
        )
        .run(config([NodeId(0)].into()))
        .await
        .unwrap();

        let cryptarchia = block_processor.cryptarchia;

        // All blocks from the peer should be in the local chain.
        assert!(peer.chain.iter().all(|b| cryptarchia.has_block(&b.id)));
    }

    #[tokio::test]
    async fn repeat_downloads() {
        let peer = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                Block::new(2, 1, 2, 2),
                Block::new(3, 2, 3, 3),
            ],
            Ok(Block::new(3, 2, 3, 3)),
            2,
            false,
        );
        let block_processor = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(HashMap::from([(NodeId(0), peer.clone())])),
        )
        .run(config([NodeId(0)].into()))
        .await
        .unwrap();

        let cryptarchia = block_processor.cryptarchia;

        // All blocks from the peer should be in the local chain.
        assert!(peer.chain.iter().all(|b| cryptarchia.has_block(&b.id)));
    }

    #[tokio::test]
    async fn multiple_peers() {
        let peer0 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                Block::new(2, 1, 2, 2),
            ],
            Ok(Block::new(2, 1, 2, 2)),
            2,
            false,
        );
        let peer1 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(3, GENESIS_ID, 3, 1),
                Block::new(4, 3, 4, 2),
                Block::new(5, 4, 5, 3),
            ],
            Ok(Block::new(5, 4, 5, 3)),
            2,
            false,
        );
        let block_processor = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(HashMap::from([
                (NodeId(0), peer0.clone()),
                (NodeId(1), peer1.clone()),
            ])),
        )
        .run(config([NodeId(0), NodeId(1)].into()))
        .await
        .unwrap();

        let cryptarchia = block_processor.cryptarchia;

        // All blocks from both peers should be in the local chain.
        assert!(peer0.chain.iter().all(|b| cryptarchia.has_block(&b.id)));
        assert!(peer1.chain.iter().all(|b| cryptarchia.has_block(&b.id)));
    }

    /// If one peer returns an error while streaming blocks,
    /// the peer should be ignored, and IBD should continue
    /// with the remaining peers.
    #[tokio::test]
    async fn stream_err_from_one_peer_while_downloading() {
        let peer0 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                Block::new(2, 1, 2, 2),
            ],
            Ok(Block::new(2, 1, 2, 2)),
            2,
            true, // Return error while streaming blocks
        );
        let peer1 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(3, GENESIS_ID, 3, 1),
                Block::new(4, 3, 4, 2),
                Block::new(5, 4, 5, 3),
            ],
            Ok(Block::new(5, 4, 5, 3)),
            2,
            false,
        );
        let block_processor = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(HashMap::from([
                (NodeId(0), peer0.clone()),
                (NodeId(1), peer1.clone()),
            ])),
        )
        .run(config([NodeId(0), NodeId(1)].into()))
        .await
        .unwrap();

        let cryptarchia = block_processor.cryptarchia;

        // All blocks from peer1 that doesn't return an error
        // should be added to the local chain.
        assert!(peer1.chain.iter().all(|b| cryptarchia.has_block(&b.id)));
    }

    /// If all peers return an error while streaming blocks,
    /// [`Error::AllPeersFailed`] should be returned.
    #[tokio::test]
    async fn stream_err_from_all_peers_while_downloading() {
        let peer0 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                Block::new(2, 1, 2, 2),
            ],
            Ok(Block::new(2, 1, 2, 2)),
            2,
            true, // Return error while streaming blocks
        );
        let peer1 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(3, GENESIS_ID, 3, 1),
                Block::new(4, 3, 4, 2),
                Block::new(5, 4, 5, 3),
            ],
            Ok(Block::new(5, 4, 5, 3)),
            2,
            true, // Return error while streaming blocks
        );
        let result = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(HashMap::from([
                (NodeId(0), peer0.clone()),
                (NodeId(1), peer1.clone()),
            ])),
        )
        .run(config([NodeId(0), NodeId(1)].into()))
        .await;

        assert!(matches!(result, Err(Error::AllPeersFailed)));
    }

    /// If one peer returns an error while initiating download,
    /// the peer should be ignored, and IBD should continue
    /// with the remaining peers.
    #[tokio::test]
    async fn stream_err_from_one_peer_while_initiating() {
        let peer0 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                Block::new(2, 1, 2, 2),
            ],
            Err(()), // Return error while initiating download
            2,
            true,
        );
        let peer1 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(3, GENESIS_ID, 3, 1),
                Block::new(4, 3, 4, 2),
                Block::new(5, 4, 5, 3),
            ],
            Ok(Block::new(5, 4, 5, 3)),
            2,
            false,
        );
        let block_processor = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(HashMap::from([
                (NodeId(0), peer0.clone()),
                (NodeId(1), peer1.clone()),
            ])),
        )
        .run(config([NodeId(0), NodeId(1)].into()))
        .await
        .unwrap();

        let cryptarchia = block_processor.cryptarchia;
        // All blocks from peer1 that doesn't return an error
        // should be added to the local chain.
        assert!(peer1.chain.iter().all(|b| cryptarchia.has_block(&b.id)));
    }

    /// If all peers return an error while initiating download,
    /// [`Error::AllPeersFailed`] should be returned.
    #[tokio::test]
    async fn stream_err_from_all_peers_while_initiating() {
        let peer0 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                Block::new(2, 1, 2, 2),
            ],
            Err(()), // Return error while initiating download
            2,
            true,
        );
        let peer1 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(3, GENESIS_ID, 3, 1),
                Block::new(4, 3, 4, 2),
                Block::new(5, 4, 5, 3),
            ],
            Err(()), // Return error while initiating download
            2,
            true,
        );
        let result = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(HashMap::from([
                (NodeId(0), peer0.clone()),
                (NodeId(1), peer1.clone()),
            ])),
        )
        .run(config([NodeId(0), NodeId(1)].into()))
        .await;

        assert!(matches!(result, Err(Error::AllPeersFailed)));
    }

    /// If a block received from a peer cannot be processed,
    /// the peer should be ignored, and IBD should continue
    /// with the remaining peers.
    #[tokio::test]
    async fn block_err_from_one_peer() {
        let peer0 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                // Invalid block (parent doesn't exist)
                Block::new(2, 100, 2, 2),
                Block::new(3, 2, 3, 3),
            ],
            Ok(Block::new(3, 2, 3, 3)),
            2,
            false,
        );
        let peer1 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(4, GENESIS_ID, 1, 1),
                Block::new(5, 4, 2, 2),
                Block::new(6, 5, 3, 3),
            ],
            Ok(Block::new(6, 5, 3, 3)),
            2,
            false,
        );
        let block_processor = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(HashMap::from([
                (NodeId(0), peer0.clone()),
                (NodeId(1), peer1.clone()),
            ])),
        )
        .run(config([NodeId(0), NodeId(1)].into()))
        .await
        .unwrap();

        let cryptarchia = block_processor.cryptarchia;

        // All blocks from peer1 that provided valid blocks
        // should be added to the local chain.
        assert!(peer1.chain.iter().all(|b| cryptarchia.has_block(&b.id)));
        // The local tip should be the same as peer1's tip.
        assert_eq!(cryptarchia.tip(), peer1.tip.unwrap().id);

        // Blocks from peer0 remain in the local chain only until
        // right before the failure.
        assert!(
            peer0.chain[..2]
                .iter()
                .all(|b| cryptarchia.has_block(&b.id))
        );
        assert!(
            peer0.chain[2..]
                .iter()
                .all(|b| !cryptarchia.has_block(&b.id))
        );
    }

    /// If all peers provided invalid blocks,
    /// [`Error::AllPeersFailed`] should be returned.
    #[tokio::test]
    async fn block_err_from_all_peers() {
        let peer0 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                // Invalid block (parent doesn't exist)
                Block::new(2, 100, 2, 2),
                Block::new(3, 2, 3, 3),
            ],
            Ok(Block::new(3, 2, 3, 3)),
            2,
            false,
        );
        let peer1 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(4, GENESIS_ID, 1, 1),
                // Invalid block (parent doesn't exist)
                Block::new(5, 100, 2, 2),
                Block::new(6, 5, 3, 3),
            ],
            Ok(Block::new(6, 5, 3, 3)),
            2,
            false,
        );
        let result = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(HashMap::from([
                (NodeId(0), peer0.clone()),
                (NodeId(1), peer1.clone()),
            ])),
        )
        .run(config([NodeId(0), NodeId(1)].into()))
        .await;

        // Expect an error
        assert!(matches!(result, Err(Error::AllPeersFailed)));
    }

    #[tokio::test]
    async fn block_err_from_all_peers_with_same_tip() {
        let peer0 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                // Invalid block (parent doesn't exist)
                Block::new(2, 100, 2, 2),
                Block::new(3, 2, 3, 3),
            ],
            Ok(Block::new(3, 2, 3, 3)),
            2,
            false,
        );
        let peer1 = peer0.clone();

        let result = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(HashMap::from([
                (NodeId(0), peer0.clone()),
                (NodeId(1), peer1.clone()),
            ])),
        )
        .run(config([NodeId(0), NodeId(1)].into()))
        .await;

        // Expect an error
        assert!(matches!(result, Err(Error::AllPeersFailed)));
    }

    struct MockBlockProcessor {
        cryptarchia: lb_chain_service::Cryptarchia,
    }

    impl MockBlockProcessor {
        fn new() -> Self {
            Self {
                cryptarchia: new_cryptarchia(),
            }
        }
    }

    impl IbdBlockProcessor<Block> for MockBlockProcessor {
        async fn info(&self) -> Result<CryptarchiaInfo, Error> {
            Ok(self.cryptarchia.info())
        }

        async fn process_block(&mut self, block: Block) -> Result<(), Error> {
            // Add the block only to the consensus, not to the ledger state
            // because the mocked block doesn't have a proof.
            // It's enough because the tests doesn't check the ledger state.
            let UpdatedCryptarchia {
                cryptarchia: consensus,
                ..
            } = self
                .cryptarchia
                .consensus
                .receive_block(block.id, block.parent, block.slot)
                .map_err(|e| {
                    Error::BlockProcessing(ChainError::InvalidBlock(format!(
                        "Consensus error: {e:?}"
                    )))
                })?;

            self.cryptarchia.consensus = consensus;
            Ok(())
        }

        async fn has_processed_block(&self, header: HeaderId) -> Result<bool, Error> {
            Ok(self.cryptarchia.has_block(&header))
        }
    }

    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    struct NodeId(usize);

    fn config(peers: HashSet<NodeId>) -> IbdConfig<NodeId> {
        IbdConfig {
            peers,
            delay_before_new_download: std::time::Duration::from_millis(1),
        }
    }

    const GENESIS_ID: u8 = 0;

    #[derive(Clone, Debug, PartialEq)]
    struct Block {
        id: HeaderId,
        parent: HeaderId,
        slot: Slot,
        height: u64,
    }

    impl Block {
        fn new(id: u8, parent: u8, slot: u64, height: u64) -> Self {
            Self {
                id: [id; 32].into(),
                parent: [parent; 32].into(),
                slot: slot.into(),
                height,
            }
        }

        fn genesis() -> Self {
            Self {
                id: [GENESIS_ID; 32].into(),
                parent: [GENESIS_ID; 32].into(),
                slot: Slot::genesis(),
                height: 0,
            }
        }
    }

    /// A mock block provider that returns the fixed sets of block streams.
    #[derive(Clone)]
    struct BlockProvider {
        chain: Vec<Block>,
        tip: Result<Block, ()>,
        stream_limit: usize,
        stream_err: bool,
    }

    impl BlockProvider {
        fn new(
            chain: Vec<Block>,
            tip: Result<Block, ()>,
            stream_limit: usize,
            stream_err: bool,
        ) -> Self {
            Self {
                chain,
                tip,
                stream_limit,
                stream_err,
            }
        }

        fn stream(&self, known_blocks: &HashSet<HeaderId>) -> Vec<Result<Block, DynError>> {
            if self.stream_err {
                return vec![Err(DynError::from("Stream error"))];
            }

            let start_pos = self
                .chain
                .iter()
                .rposition(|block| known_blocks.contains(&block.id))
                .map_or(0, |pos| pos + 1);
            if start_pos >= self.chain.len() {
                vec![]
            } else {
                self.chain[start_pos..]
                    .iter()
                    .take(self.stream_limit)
                    .cloned()
                    .map(Ok)
                    .collect()
            }
        }
    }

    /// A mock network adapter that returns a static set of blocks.
    struct MockNetworkAdapter<RuntimeServiceId> {
        providers: HashMap<NodeId, BlockProvider>,
        _phantom: PhantomData<RuntimeServiceId>,
    }

    impl<RuntimeServiceId> MockNetworkAdapter<RuntimeServiceId> {
        pub fn new(providers: HashMap<NodeId, BlockProvider>) -> Self {
            Self {
                providers,
                _phantom: PhantomData,
            }
        }
    }

    #[async_trait::async_trait]
    impl<RuntimeServiceId> NetworkAdapter<RuntimeServiceId> for MockNetworkAdapter<RuntimeServiceId>
    where
        RuntimeServiceId: Send + Sync + 'static,
    {
        type Backend = MockNetworkBackend<RuntimeServiceId>;
        type Settings = ();
        type PeerId = NodeId;
        type Block = Block;
        type Proposal = Proposal;

        async fn new(
            _settings: Self::Settings,
            _network_relay: OutboundRelay<
                <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
            >,
        ) -> Self {
            unimplemented!()
        }

        async fn proposals_stream(&self) -> Result<BoxedStream<Self::Proposal>, DynError> {
            unimplemented!()
        }

        async fn chainsync_events_stream(&self) -> Result<BoxedStream<ChainSyncEvent>, DynError> {
            unimplemented!()
        }

        async fn request_tip(&self, peer: Self::PeerId) -> Result<GetTipResponse, DynError> {
            let provider = self.providers.get(&peer).unwrap();
            match provider.tip.clone() {
                Ok(tip) => Ok(GetTipResponse::Tip {
                    tip: tip.id,
                    slot: tip.slot,
                    height: tip.height,
                }),
                Err(()) => Err(DynError::from("Cannot provide tip")),
            }
        }

        async fn request_blocks_from_peer(
            &self,
            peer: Self::PeerId,
            _target_block: HeaderId,
            local_tip: HeaderId,
            latest_immutable_block: HeaderId,
            additional_blocks: HashSet<HeaderId>,
        ) -> Result<BoxedStream<Result<(HeaderId, Self::Block), DynError>>, DynError> {
            let provider = self.providers.get(&peer).unwrap();

            let mut known_blocks = additional_blocks;
            known_blocks.insert(local_tip);
            known_blocks.insert(latest_immutable_block);

            let stream = provider.stream(&known_blocks);
            Ok(Box::new(tokio_stream::iter(stream.into_iter().map(
                |result| match result {
                    Ok(block) => Ok((block.id, block)),
                    Err(e) => Err(e),
                },
            ))))
        }

        async fn request_blocks_from_peers(
            &self,
            _target_block: HeaderId,
            _local_tip: HeaderId,
            _latest_immutable_block: HeaderId,
            _additional_blocks: HashSet<HeaderId>,
        ) -> Result<BoxedStream<Result<(HeaderId, Self::Block), DynError>>, DynError> {
            unimplemented!()
        }
    }

    /// A mock network backend that does nothing.
    struct MockNetworkBackend<RuntimeServiceId> {
        _phantom: PhantomData<RuntimeServiceId>,
    }

    #[async_trait::async_trait]
    impl<RuntimeServiceId> NetworkBackend<RuntimeServiceId> for MockNetworkBackend<RuntimeServiceId>
    where
        RuntimeServiceId: Send + Sync + 'static,
    {
        type Settings = ();
        type Message = ();
        type PubSubEvent = ();
        type ChainSyncEvent = ();

        fn new(
            _config: Self::Settings,
            _overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        ) -> Self {
            unimplemented!()
        }

        async fn process(&self, _msg: Self::Message) {
            unimplemented!()
        }

        async fn subscribe_to_pubsub(&mut self) -> BroadcastStream<Self::PubSubEvent> {
            unimplemented!()
        }

        async fn subscribe_to_chainsync(&mut self) -> BroadcastStream<Self::ChainSyncEvent> {
            unimplemented!()
        }
    }

    fn new_cryptarchia() -> lb_chain_service::Cryptarchia {
        let ledger_config = ledger_config();
        lb_chain_service::Cryptarchia::from_lib(
            [GENESIS_ID; 32].into(),
            LedgerState::from_utxos(empty(), &ledger_config),
            [GENESIS_ID; 32].into(),
            ledger_config,
            lb_cryptarchia_engine::State::Bootstrapping,
            0.into(),
            0,
        )
    }

    #[must_use]
    fn ledger_config() -> lb_ledger::Config {
        lb_ledger::Config {
            epoch_config: EpochConfig {
                epoch_stake_distribution_stabilization: NonZero::new(1).unwrap(),
                epoch_period_nonce_buffer: NonZero::new(1).unwrap(),
                epoch_period_nonce_stabilization: NonZero::new(1).unwrap(),
            },
            consensus_config: lb_cryptarchia_engine::Config::new(
                NonZero::new(1).unwrap(),
                1.0,
                1f64.try_into().expect("1 > 0"),
            ),
            sdp_config: lb_ledger::mantle::sdp::Config {
                service_params: Arc::new(
                    [(
                        ServiceType::BlendNetwork,
                        ServiceParameters {
                            lock_period: 10,
                            inactivity_period: 20,
                            retention_period: 100,
                            timestamp: 0,
                            session_duration: 10,
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: rewards::blend::RewardsParameters {
                        rounds_per_session: NonZeroU64::new(10).unwrap(),
                        message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
                        num_blend_layers: NonZeroU64::new(3).unwrap(),
                        minimum_network_size: NonZeroU64::new(1).unwrap(),
                        data_replication_factor: 0,
                        activity_threshold_sensitivity: 1,
                    },
                },
                min_stake: MinStake {
                    threshold: 1,
                    timestamp: 0,
                },
            },
        }
    }
}
