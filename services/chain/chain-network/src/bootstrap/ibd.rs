use std::{collections::HashSet, fmt::Debug, hash::Hash, marker::PhantomData, time::Duration};

use backon::{ExponentialBuilder, Retryable as _};
use futures::{
    StreamExt as _,
    future::{join_all, try_join_all},
};
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
use tracing::{debug, error, info, trace, warn};

use crate::{
    Error as ChainError, IbdConfig, OrphanConfig, mempool::adapter::MempoolAdapter,
    network::NetworkAdapter, sync::orphan_handler::OrphanBlocksDownloader,
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
        Ok(self.cryptarchia.info().await?.cryptarchia_info)
    }

    async fn process_block(&mut self, block: Block<Cryptarchia::Tx>) -> Result<(), Error> {
        crate::apply_block_and_reconcile_mempool::<_, Mempool, _>(
            block,
            &self.cryptarchia,
            &self.mempool_adapter,
        )
        .await
        .map_err(Error::from)
    }

    async fn has_processed_block(&self, block_id: HeaderId) -> Result<bool, Error> {
        Ok(self.cryptarchia.get_ledger_state(block_id).await?.is_some())
    }
}

/// Initial Block Download.
///
/// Each round, IBD fetches the chain tip from every configured peer in
/// parallel, enqueues every fetched tip that is not yet in the local tree as
/// an orphan into the [`OrphanBlocksDownloader`], drains the downloader, then
/// re-fetches. IBD completes once every fetched tip is in the local tree.
///
/// The orphan downloader fans out across connected and discovered peers,
/// so block downloads themselves are not restricted to the configured IBD
/// peer set.
pub struct InitialBlockDownload<NetAdapter, BlockProcessor, RuntimeServiceId>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId>,
    NetAdapter::PeerId: Clone + Eq + Hash,
    BlockProcessor: IbdBlockProcessor<NetAdapter::Block>,
{
    block_processor: BlockProcessor,
    network: NetAdapter,
    _phantom: PhantomData<RuntimeServiceId>,
}

impl<NetAdapter, BlockProcessor, RuntimeServiceId>
    InitialBlockDownload<NetAdapter, BlockProcessor, RuntimeServiceId>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId>,
    NetAdapter::PeerId: Clone + Eq + Hash,
    BlockProcessor: IbdBlockProcessor<NetAdapter::Block>,
{
    pub const fn new(block_processor: BlockProcessor, network: NetAdapter) -> Self {
        Self {
            block_processor,
            network,
            _phantom: PhantomData,
        }
    }
}

impl<NetAdapter, BlockProcessor, RuntimeServiceId>
    InitialBlockDownload<NetAdapter, BlockProcessor, RuntimeServiceId>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId> + Clone + Send + Sync + 'static,
    NetAdapter::PeerId: Copy + Clone + Eq + Hash + Debug + Send + Sync + Unpin + 'static,
    NetAdapter::Block: Clone + Debug + Send + Sync + Unpin + 'static,
    BlockProcessor: IbdBlockProcessor<NetAdapter::Block> + Send + Sync,
    RuntimeServiceId: Send + Sync + 'static,
{
    /// Runs IBD against the configured peers.
    ///
    /// # Returns
    /// - [`BlockProcessor`] if IBD caught up to every reachable IBD peer's tip.
    /// - [`Error::AllPeersFailed`] if no IBD peer returned a tip.
    pub async fn run(
        mut self,
        config: IbdConfig<NetAdapter::PeerId>,
        orphan_config: &OrphanConfig,
    ) -> Result<BlockProcessor, Error> {
        if config.peers.is_empty() {
            warn!("Skipping IBD as no peers are configured");
            return Ok(self.block_processor);
        }

        info!(
            "Starting Initial Block Download with {} peers",
            config.peers.len()
        );

        self.download_blocks(config, orphan_config).await?;
        Ok(self.block_processor)
    }

    /// Start downloading blocks:
    /// 1. Fetch tips from configured peers
    /// 2. Enqueue tips into orphan downloader
    /// 3. Drain orphan downloader, downloading/applying blocks.
    /// 4. Repeat 1~3 until all tips are present in the local tree
    ///
    /// Between each round, sleep for `config.round_delay` to not overload the
    /// configured peers with tip requests if the block downloads have been
    /// rejected from all peers immediately. Also, this delay gives the node
    /// time to discover new peers.
    async fn download_blocks(
        &mut self,
        config: IbdConfig<NetAdapter::PeerId>,
        orphan_config: &OrphanConfig,
    ) -> Result<(), Error> {
        let mut downloader = OrphanBlocksDownloader::new(
            self.network.clone(),
            config
                .peers
                .len()
                .try_into()
                .expect("IBD peer set shouldn't be empty in this function"),
            orphan_config.max_rejected_cache_size,
        );

        loop {
            let unsynced_tips = self.collect_unsynced_tips(&config).await?;
            if unsynced_tips.is_empty() {
                info!("IBD complete: all configured peer tips are present in the local tree");
                return Ok(());
            }
            let info = self.block_processor.info().await?;
            enqueue_tips(&mut downloader, unsynced_tips, &info);

            self.drain_downloader(&mut downloader).await;

            tokio::time::sleep(config.round_delay).await;
        }
    }

    /// Pulls every block from the orphan downloader, applying each to the
    /// block processor. Returns once the downloader has nothing more to yield.
    #[expect(clippy::cognitive_complexity, reason = "for readability")]
    async fn drain_downloader(
        &mut self,
        downloader: &mut OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>,
    ) {
        debug!("draining downloads");

        // Use `timeout` because `downloader.next()` can stall forever if a
        // download fails and leaves the queue empty.
        // The timeout lets us re-check `should_poll` and exit cleanly in that case.
        // Also, the timeout is useful to give the node time for peer discovery
        // before starting the next IBD round.
        // TODO: improve `OrphanBlocksDownloader` to simplify this.
        while downloader.should_poll() {
            match tokio::time::timeout(Duration::from_secs(1), downloader.next()).await {
                Ok(Some(block)) => match self.block_processor.process_block(block).await {
                    Ok(()) => {}
                    Err(Error::BlockProcessing(ChainError::Cryptarchia(
                        lb_chain_service::api::ApiError::AlreadyApplied(header_id),
                    ))) => {
                        debug!(?header_id, "block already applied; continuing");
                    }
                    Err(err) => {
                        warn!(?err, "failed to process block; cancelling the download");
                        downloader.cancel_active_download();
                    }
                },
                Ok(None) => {
                    debug!("orphan downloader returned None; re-checking should_poll");
                }
                Err(_) => {
                    trace!("drain timed out; re-checking should_poll");
                }
            }
        }
    }

    /// Fetches tips from the configured peers and returns those not yet in the
    /// local tree. Returns [`Error::AllPeersFailed`] if no peer responded
    /// across every retry attempt.
    async fn collect_unsynced_tips(
        &self,
        config: &IbdConfig<NetAdapter::PeerId>,
    ) -> Result<HashSet<HeaderId>, Error> {
        debug!("collecting unsynced tips from {} peers", config.peers.len());

        let tips = fetch_tips_with_retry(&self.network, config)
            .await
            .inspect_err(|_| error!("no configured peer returned a tip this round"))?;

        Ok(try_join_all(tips.into_iter().map(async |tip| {
            self.block_processor
                .has_processed_block(tip)
                .await
                .map(|processed| (!processed).then_some(tip))
        }))
        .await?
        .into_iter()
        .flatten()
        .collect())
    }
}

/// Calls [`fetch_tips`] with exponential backoff to not overload the configured
/// peers.
///
/// Returns `Ok` with the first non-empty batch, or [`AllPeersFailed`] once
/// every retry attempt produced nothing.
///
/// Retry lives at this batch level (not per-fetch), so a slow peer's backoff
/// doesn't block IBD from moving on to fetch next tips from the fast peers.
async fn fetch_tips_with_retry<NetAdapter, RuntimeServiceId>(
    network: &NetAdapter,
    config: &IbdConfig<NetAdapter::PeerId>,
) -> Result<HashSet<HeaderId>, AllPeersFailed>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId> + Sync,
    NetAdapter::PeerId: Copy + Clone + Eq + Hash + Debug + Send + Sync,
    RuntimeServiceId: Sync,
{
    // A closure is needed for `backon::Retryable.retry()` that spawns a fresh
    // future per attempt.
    (|| fetch_tips(network, config.peers.iter().copied()))
        .retry(
            ExponentialBuilder::default()
                .with_min_delay(config.tips_fetch_min_delay)
                .with_max_delay(config.tips_fetch_max_delay)
                .with_max_times(config.tips_fetch_max_attempts)
                .with_jitter(),
        )
        .notify(|_, delay| debug!("tip fetch returned no tips; retrying in {delay:?}"))
        .await
}

/// Concurrently asks every peer for its current chain tip. Returns `Ok` with
/// the tips of the peers that responded, or [`AllPeersFailed`] if none did.
/// Per-peer failures are warn-logged and dropped.
async fn fetch_tips<NetAdapter, RuntimeServiceId, I>(
    network: &NetAdapter,
    peers: I,
) -> Result<HashSet<HeaderId>, AllPeersFailed>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId> + Sync,
    NetAdapter::PeerId: Copy + Debug + Send + Sync,
    RuntimeServiceId: Sync,
    I: IntoIterator<Item = NetAdapter::PeerId>,
{
    let tips: HashSet<HeaderId> = join_all(peers.into_iter().map(async |peer| {
        let result: Result<HeaderId, DynError> = match network.request_tip(peer).await {
            Ok(GetTipResponse::Tip { tip, .. }) => Ok(tip),
            Ok(GetTipResponse::Failure(reason)) => Err(DynError::from(reason)),
            Err(e) => Err(e),
        };
        result
            .inspect_err(|e| warn!("failed to fetch tip from {peer:?}: {e}"))
            .ok()
    }))
    .await
    .into_iter()
    .flatten()
    .collect();
    if tips.is_empty() {
        Err(AllPeersFailed)
    } else {
        Ok(tips)
    }
}

fn enqueue_tips<NetAdapter, RuntimeServiceId>(
    downloader: &mut OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>,
    tips: HashSet<HeaderId>,
    info: &CryptarchiaInfo,
) where
    NetAdapter: NetworkAdapter<RuntimeServiceId> + Clone + Send + Sync + 'static,
    NetAdapter::Block: Clone + Send + Sync + 'static,
    RuntimeServiceId: Send + Sync + 'static,
{
    for tip in tips {
        if let Err(e) = downloader.enqueue_orphan(tip, None, info.tip, info.lib) {
            debug!("failed to enqueue tip {tip:?}: {e}");
        } else {
            debug!("enqueued tip {tip:?} for download");
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Cryptarchia(#[from] lb_chain_service::api::ApiError),
    #[error(transparent)]
    AllPeersFailed(#[from] AllPeersFailed),
    #[error("Block processing failed: {0}")]
    BlockProcessing(#[from] ChainError),
}

#[derive(Debug, thiserror::Error)]
#[error("All peers failed")]
pub struct AllPeersFailed;

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        iter::empty,
        num::{NonZero, NonZeroU64},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use futures::stream;
    use lb_core::{
        block::Proposal,
        sdp::{MinStake, ServiceParameters, ServiceType},
    };
    use lb_cryptarchia_engine::{EpochConfig, Slot};
    use lb_ledger::{
        LedgerState,
        mantle::sdp::{ServiceRewardsParameters, rewards},
    };
    use lb_network_service::{NetworkService, backends::NetworkBackend, message::ChainSyncEvent};
    use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
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
            MockNetworkAdapter::<()>::new(Vec::new()),
        )
        .run(config(HashSet::new()), &orphan_config())
        .await
        .unwrap();

        let cryptarchia = block_processor.cryptarchia;

        // The Cryptarchia remains unchanged.
        assert_eq!(cryptarchia.lib(), [GENESIS_ID; 32].into());
        assert_eq!(cryptarchia.tip(), [GENESIS_ID; 32].into());
    }

    #[tokio::test]
    async fn single_peer() {
        let peer = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                Block::new(2, 1, 2, 2),
            ],
            Ok(Block::new(2, 1, 2, 2)),
        );
        let block_processor = run_ibd(
            HashMap::from([(NodeId(0), peer.clone())]),
            [NodeId(0)].into(),
        )
        .await
        .unwrap();

        let cryptarchia = block_processor.cryptarchia;

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
        );
        let peer1 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(3, GENESIS_ID, 3, 1),
                Block::new(4, 3, 4, 2),
                Block::new(5, 4, 5, 3),
            ],
            Ok(Block::new(5, 4, 5, 3)),
        );
        let block_processor = run_ibd(
            HashMap::from([(NodeId(0), peer0.clone()), (NodeId(1), peer1.clone())]),
            [NodeId(0), NodeId(1)].into(),
        )
        .await
        .unwrap();

        let cryptarchia = block_processor.cryptarchia;

        assert!(peer0.chain.iter().all(|b| cryptarchia.has_block(&b.id)));
        assert!(peer1.chain.iter().all(|b| cryptarchia.has_block(&b.id)));
    }

    /// If all configured peers fail to return a tip, [`Error::AllPeersFailed`]
    /// is returned.
    #[tokio::test]
    async fn all_peers_fail_tip() {
        let peer0 = BlockProvider::new(vec![Block::genesis()], Err(()));
        let peer1 = BlockProvider::new(vec![Block::genesis()], Err(()));
        let result = run_ibd(
            HashMap::from([(NodeId(0), peer0), (NodeId(1), peer1)]),
            [NodeId(0), NodeId(1)].into(),
        )
        .await;

        assert!(matches!(result, Err(Error::AllPeersFailed(_))));
    }

    /// If only some peers return a tip, IBD proceeds with the ones that did.
    #[tokio::test]
    async fn one_peer_fails_tip() {
        let peer0 = BlockProvider::new(vec![Block::genesis()], Err(()));
        let peer1 = BlockProvider::new(
            vec![
                Block::genesis(),
                Block::new(1, GENESIS_ID, 1, 1),
                Block::new(2, 1, 2, 2),
            ],
            Ok(Block::new(2, 1, 2, 2)),
        );
        let block_processor = run_ibd(
            HashMap::from([(NodeId(0), peer0), (NodeId(1), peer1.clone())]),
            [NodeId(0), NodeId(1)].into(),
        )
        .await
        .unwrap();

        let cryptarchia = block_processor.cryptarchia;
        assert!(peer1.chain.iter().all(|b| cryptarchia.has_block(&b.id)));
    }

    /// If every peer's tip is already in the local tree, IBD finishes
    /// immediately without enqueueing anything.
    #[tokio::test]
    async fn all_tips_already_local() {
        let peer = BlockProvider::new(vec![Block::genesis()], Ok(Block::genesis()));
        let block_processor = run_ibd(HashMap::from([(NodeId(0), peer)]), [NodeId(0)].into())
            .await
            .unwrap();

        let cryptarchia = block_processor.cryptarchia;
        assert_eq!(cryptarchia.tip(), [GENESIS_ID; 32].into());
    }

    /// A peer that streams a block whose parent is unknown to the consensus
    /// triggers `process_block` to fail, which must in turn call
    /// `cancel_active_download` on the orphan downloader. The IBD then keeps
    /// retrying that same tip (no per-tip retry limit yet), so the test bounds
    /// the run with a timeout and asserts the failure counter advanced.
    #[tokio::test]
    async fn block_apply_error_triggers_cancel() {
        let invalid_chain = vec![
            Block::genesis(),
            Block::new(1, GENESIS_ID, 1, 1),
            // Parent (id=100) is not in the consensus, so process_block errors.
            Block::new(2, 100, 2, 2),
        ];
        let peer = BlockProvider::new(invalid_chain, Ok(Block::new(2, 100, 2, 2)));

        let processor = MockBlockProcessor::new();
        let failures = Arc::clone(&processor.process_block_failures);
        let ibd = InitialBlockDownload::new(
            processor,
            MockNetworkAdapter::<()>::new(vec![(NodeId(0), peer)]),
        );

        let _result = tokio::time::timeout(
            Duration::from_millis(200),
            ibd.run(config([NodeId(0)].into()), &orphan_config()),
        )
        .await;

        assert!(
            failures.load(Ordering::SeqCst) > 0,
            "process_block must have failed at least once",
        );
    }

    /// Multi-round flow: round 1 syncs the peer's first reported tip, round 2
    /// picks up the peer's advanced tip, round 3 sees the tip in local tree
    /// and completes IBD.
    #[tokio::test]
    async fn tip_advances_between_rounds() {
        let chain = vec![
            Block::genesis(),
            Block::new(1, GENESIS_ID, 1, 1),
            Block::new(2, 1, 2, 2),
        ];
        let peer = BlockProvider::with_tips(
            chain.clone(),
            vec![
                Ok(Block::new(1, GENESIS_ID, 1, 1)),
                Ok(Block::new(2, 1, 2, 2)),
            ],
        );
        let block_processor = run_ibd(HashMap::from([(NodeId(0), peer)]), [NodeId(0)].into())
            .await
            .unwrap();

        let cryptarchia = block_processor.cryptarchia;
        assert!(chain.iter().all(|b| cryptarchia.has_block(&b.id)));
    }

    /// The peer streams a chain whose prefix is shared with the local ledger
    /// (up to a fork point) and whose suffix is the peer's own fork:
    ///
    /// ```text
    ///   G--A--B--C--D          (local tip)
    ///         \
    ///          E--F--H         (remote tip)
    ///   |<--->|
    ///   overlapped
    /// ```
    ///
    /// Since `local_tip=D` is not on the peer's chain, the peer streams from
    /// `A`, so `A` and `B` come back as `AlreadyApplied`.
    /// IBD must skip those and keep the stream flowing so the remote fork
    /// `[E, F, H]` gets applied instead of cancelling on the first duplicate.
    #[tokio::test]
    async fn already_applied_prefix_does_not_cancel_download() {
        let local_a = Block::new(1, GENESIS_ID, 1, 1);
        let local_b = Block::new(2, 1, 2, 2);
        let local_c = Block::new(3, 2, 3, 3);
        let local_d = Block::new(4, 3, 4, 4);
        let remote_e = Block::new(5, 2, 3, 3);
        let remote_f = Block::new(6, 5, 4, 4);
        let remote_h = Block::new(7, 6, 5, 5);
        let remote_chain = vec![
            Block::genesis(),
            local_a.clone(),
            local_b.clone(),
            remote_e.clone(),
            remote_f.clone(),
            remote_h.clone(),
        ];

        let mut processor = MockBlockProcessor::new();
        for block in [&local_a, &local_b, &local_c, &local_d] {
            processor.process_block(block.clone()).await.unwrap();
        }

        let peer = BlockProvider::new(remote_chain, Ok(remote_h.clone()));
        let ibd = InitialBlockDownload::new(
            processor,
            MockNetworkAdapter::<()>::new(vec![(NodeId(0), peer)]),
        );
        let block_processor = tokio::time::timeout(
            Duration::from_secs(5),
            ibd.run(config([NodeId(0)].into()), &orphan_config()),
        )
        .await
        .expect("IBD test timed out")
        .expect("IBD should complete");

        let cryptarchia = block_processor.cryptarchia;
        assert!(
            [&remote_e, &remote_f, &remote_h]
                .iter()
                .all(|b| cryptarchia.has_block(&b.id)),
        );
    }

    /// First round succeeds (peer returns a tip we sync), second round the
    /// peer goes dark. Since the synced tip is already local, the only
    /// remaining peer fails tip fetch -> `AllPeersFailed`.
    #[tokio::test]
    async fn all_peers_go_dark_after_first_round() {
        let chain = vec![Block::genesis(), Block::new(1, GENESIS_ID, 1, 1)];
        let peer =
            BlockProvider::with_tips(chain, vec![Ok(Block::new(1, GENESIS_ID, 1, 1)), Err(())]);
        let result = run_ibd(HashMap::from([(NodeId(0), peer)]), [NodeId(0)].into()).await;
        assert!(matches!(result, Err(Error::AllPeersFailed(_))));
    }

    async fn run_ibd(
        providers: HashMap<NodeId, BlockProvider>,
        peers: HashSet<NodeId>,
    ) -> Result<MockBlockProcessor, Error> {
        let provider_list = providers.into_iter().collect();
        let ibd = InitialBlockDownload::new(
            MockBlockProcessor::new(),
            MockNetworkAdapter::<()>::new(provider_list),
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            ibd.run(config(peers), &orphan_config()),
        )
        .await
        .expect("IBD test timed out")
    }

    struct MockBlockProcessor {
        cryptarchia: lb_chain_service::Cryptarchia,
        process_block_failures: Arc<AtomicUsize>,
    }

    impl MockBlockProcessor {
        fn new() -> Self {
            Self {
                cryptarchia: new_cryptarchia(),
                process_block_failures: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl IbdBlockProcessor<Block> for MockBlockProcessor {
        async fn info(&self) -> Result<CryptarchiaInfo, Error> {
            Ok(self.cryptarchia.info())
        }

        async fn process_block(&mut self, block: Block) -> Result<(), Error> {
            if self.cryptarchia.has_block(&block.id) {
                return Err(Error::BlockProcessing(ChainError::Cryptarchia(
                    lb_chain_service::api::ApiError::AlreadyApplied(block.id),
                )));
            }

            self.cryptarchia
                .consensus
                .receive_block(block.id, block.parent, block.slot)
                .map_err(|e| {
                    self.process_block_failures.fetch_add(1, Ordering::SeqCst);
                    Error::BlockProcessing(ChainError::InvalidBlock(format!(
                        "Consensus error: {e:?}"
                    )))
                })?;
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
            tips_fetch_max_attempts: 3,
            tips_fetch_min_delay: Duration::from_millis(250),
            tips_fetch_max_delay: Duration::from_secs(1),
            round_delay: Duration::from_secs(1),
        }
    }

    fn orphan_config() -> OrphanConfig {
        OrphanConfig {
            max_orphan_cache_size: 10.try_into().unwrap(),
            max_rejected_cache_size: 0,
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

    /// Mock peer that owns a fixed chain and a sequence of tip responses.
    ///
    /// `request_tip` consumes the sequence one element per call (saturating at
    /// the last), letting a test simulate a peer whose tip advances or goes
    /// dark between IBD rounds.
    #[derive(Clone)]
    struct BlockProvider {
        chain: Vec<Block>,
        tips: Vec<Result<Block, ()>>,
        tip_call_count: Arc<AtomicUsize>,
    }

    impl BlockProvider {
        fn new(chain: Vec<Block>, tip: Result<Block, ()>) -> Self {
            Self::with_tips(chain, vec![tip])
        }

        fn with_tips(chain: Vec<Block>, tips: Vec<Result<Block, ()>>) -> Self {
            assert!(!tips.is_empty(), "tip sequence must be non-empty");
            Self {
                chain,
                tips,
                tip_call_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn next_tip(&self) -> Result<Block, ()> {
            let idx = self.tip_call_count.fetch_add(1, Ordering::SeqCst);
            let pos = idx.min(self.tips.len() - 1);
            self.tips[pos].clone()
        }

        fn stream_to_target(
            &self,
            target: HeaderId,
            known_blocks: &HashSet<HeaderId>,
        ) -> Option<Vec<Block>> {
            let target_pos = self.chain.iter().position(|b| b.id == target)?;
            let start_pos = self.chain[..=target_pos]
                .iter()
                .rposition(|block| known_blocks.contains(&block.id))
                .map_or(0, |pos| pos + 1);
            Some(self.chain[start_pos..=target_pos].to_vec())
        }
    }

    /// Mock adapter that holds an ordered list of [`BlockProvider`]s. The
    /// order is deterministic so that `request_blocks_from_peers` picks the
    /// first provider that owns the target block in the same way across runs.
    #[derive(Clone)]
    struct MockNetworkAdapter<RuntimeServiceId> {
        providers: Arc<[(NodeId, BlockProvider)]>,
        _phantom: PhantomData<RuntimeServiceId>,
    }

    impl<RuntimeServiceId> MockNetworkAdapter<RuntimeServiceId> {
        fn new(providers: Vec<(NodeId, BlockProvider)>) -> Self {
            Self {
                providers: providers.into(),
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
            let tip = self
                .providers
                .iter()
                .find_map(|(id, p)| (*id == peer).then(|| p.next_tip()))
                .expect("test setup: peer must exist in providers");
            match tip {
                Ok(tip) => Ok(GetTipResponse::Tip {
                    tip: tip.id,
                    slot: tip.slot,
                    height: tip.height,
                }),
                Err(()) => Err(DynError::from("cannot provide tip")),
            }
        }

        async fn sample_tips(&self, _max_peers: usize) -> BoxedStream<GetTipResponse> {
            Box::new(stream::empty())
        }

        async fn request_blocks_from_peer(
            &self,
            _peer: Self::PeerId,
            _target_block: HeaderId,
            _local_tip: HeaderId,
            _latest_immutable_block: HeaderId,
            _additional_blocks: HashSet<HeaderId>,
        ) -> Result<BoxedStream<Result<(HeaderId, Self::Block), DynError>>, DynError> {
            unimplemented!()
        }

        async fn request_blocks_from_peers(
            &self,
            target_block: HeaderId,
            local_tip: HeaderId,
            latest_immutable_block: HeaderId,
            additional_blocks: HashSet<HeaderId>,
        ) -> Result<BoxedStream<Result<(HeaderId, Self::Block), DynError>>, DynError> {
            let mut known_blocks = additional_blocks;
            known_blocks.insert(local_tip);
            known_blocks.insert(latest_immutable_block);

            let blocks = self
                .providers
                .iter()
                .find_map(|(_, p)| p.stream_to_target(target_block, &known_blocks))
                .ok_or_else(|| DynError::from(format!("no peer has target {target_block:?}")))?;

            let items = blocks
                .into_iter()
                .map(|b| Ok((b.id, b)))
                .collect::<Vec<_>>();
            Ok(Box::new(tokio_stream::iter(items)))
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
        let epoch_config = EpochConfig {
            epoch_stake_distribution_stabilization: NonZero::new(1).unwrap(),
            epoch_period_nonce_buffer: NonZero::new(1).unwrap(),
            epoch_period_nonce_stabilization: NonZero::new(1).unwrap(),
        };
        let consensus_config = lb_cryptarchia_engine::Config::new(
            NonZero::new(1).unwrap(),
            NonNegativeRatio::new(1, 10.try_into().unwrap()),
            1f64.try_into().expect("1 > 0"),
        );
        let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

        lb_ledger::Config {
            epoch_config,
            consensus_config,
            sdp_config: lb_ledger::mantle::sdp::Config {
                service_params: Arc::new(
                    [(
                        ServiceType::BlendNetwork,
                        ServiceParameters {
                            inactivity_period: 20.try_into().unwrap(),
                            epoch: 0.into(),
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: rewards::blend::RewardsParameters {
                        rounds_per_epoch: epoch_length.try_into().unwrap(),
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
            faucet_pk: None,
        }
    }
}
