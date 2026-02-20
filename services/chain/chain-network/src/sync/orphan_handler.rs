use std::{
    collections::{HashMap, HashSet},
    num::NonZeroUsize,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use futures::{Stream, StreamExt as _};
use lb_core::header::HeaderId;
use overwatch::DynError;
use tracing::{debug, error, info, warn};

use crate::{
    network::{BoxedStream, NetworkAdapter},
    sync::LOG_TARGET,
};

type PendingNetworkRequest<Block> =
    Pin<Box<dyn Future<Output = Result<ActiveDownload<Block>, DynError>> + Send>>;

/// State of the orphan blocks downloader
pub enum DownloaderState<Block> {
    /// No active download, no pending request
    Idle,
    /// Starting a new download (pending network request)
    Requesting(PendingNetworkRequest<Block>),
    /// Actively streaming blocks from the network
    Downloading(ActiveDownload<Block>),
}

/// Fetches blocks for orphan blocks by requesting their ancestors from the
/// network.
pub struct OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId>,
{
    /// Queue of blocks waiting to be fetched
    pending_orphans_queue: HashMap<HeaderId, OrphanInfo>,
    /// Network interface
    network_adapter: NetAdapter,
    /// Current state of the downloader with associated data
    state: DownloaderState<NetAdapter::Block>,
    /// Maximum number of orphans to queue
    max_pending_orphans: NonZeroUsize,
    /// Waker to notify when new work is available
    waker: Option<Waker>,
    _phantom: std::marker::PhantomData<RuntimeServiceId>,
}

/// Information about an orphan block that needs to be downloaded
#[derive(Clone, Debug)]
struct OrphanInfo {
    /// The orphan block ID we're fetching ancestors for
    orphan_id: HeaderId,
    /// Local tip
    tip: HeaderId,
    /// The latest immutable block
    lib: HeaderId,
}

impl OrphanInfo {
    const fn new(block_id: HeaderId, local_tip: HeaderId, lib: HeaderId) -> Self {
        Self {
            orphan_id: block_id,
            tip: local_tip,
            lib,
        }
    }
}

pub struct ActiveDownload<Block> {
    /// Original orphan information
    orphan_info: OrphanInfo,
    /// Last block ID received (used to continue fetching)
    last_block_id: Option<HeaderId>,
    /// Active stream of blocks from the network
    block_stream: Option<BoxedStream<Result<(HeaderId, Block), DynError>>>,
    /// Total number of blocks received for this orphan
    total_blocks_received: usize,
}

impl<Block> ActiveDownload<Block> {
    fn new(
        orphan_info: OrphanInfo,
        block_stream: BoxedStream<Result<(HeaderId, Block), DynError>>,
    ) -> Self {
        Self {
            orphan_info,
            last_block_id: None,
            block_stream: Some(block_stream),
            total_blocks_received: 0,
        }
    }

    const fn orphan_block_id(&self) -> HeaderId {
        self.orphan_info.orphan_id
    }
}

impl<NetAdapter, RuntimeServiceId> OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId> + Send + Sync + Clone + 'static,
    NetAdapter::Block: Clone + Send + Sync + 'static,
    RuntimeServiceId: Send + Sync + 'static,
{
    pub fn new(network_adapter: NetAdapter, max_pending_orphans: NonZeroUsize) -> Self {
        Self {
            pending_orphans_queue: HashMap::new(),
            network_adapter,
            state: DownloaderState::Idle,
            max_pending_orphans,
            waker: None,
            _phantom: std::marker::PhantomData,
        }
    }

    pub fn enqueue_orphan(&mut self, block_id: HeaderId, current_tip: HeaderId, lib: HeaderId) {
        if self.pending_orphans_queue.len() >= self.max_pending_orphans.get() {
            warn!(
                target: LOG_TARGET,
                queue_size_limit = self.max_pending_orphans.get(),
                cur_queue_size = self.pending_orphans_queue.len(),
                ?block_id,
                "Orphan block ignored due to queue size limit"
            );
            return;
        }

        if let DownloaderState::Downloading(download) = &self.state
            && download.orphan_block_id() == block_id
        {
            debug!(target: LOG_TARGET, ?block_id, "Orphan block is already being downloaded, skipping enqueue");
            return;
        }

        if self.pending_orphans_queue.contains_key(&block_id) {
            debug!(target: LOG_TARGET, ?block_id, "Orphan block is already in the queue, skipping enqueue");
            return;
        }

        self.pending_orphans_queue
            .insert(block_id, OrphanInfo::new(block_id, current_tip, lib));
        info!(target: LOG_TARGET, ?block_id, ?current_tip, ?lib, "Orphan block enqueued for sync");

        if let Some(waker) = &self.waker {
            waker.wake_by_ref();
        }
    }

    fn dequeue_next_orphan(&mut self) -> Option<OrphanInfo> {
        let key = self.pending_orphans_queue.keys().next().copied()?;
        let orphan_info = self.pending_orphans_queue.remove(&key);
        debug!(target: LOG_TARGET, ?orphan_info, "Orphan block dequeued");
        orphan_info
    }

    async fn request_blocks_stream(
        network: NetAdapter,
        orphan_info: OrphanInfo,
        known_blocks: HashSet<HeaderId>,
    ) -> Result<ActiveDownload<NetAdapter::Block>, DynError> {
        network
            .request_blocks_from_peers(
                orphan_info.orphan_id,
                orphan_info.tip,
                orphan_info.lib,
                known_blocks.clone(),
            )
            .await
            .map(|stream| ActiveDownload::new(orphan_info, stream))
    }

    pub fn remove_orphan(&mut self, block_id: &HeaderId) {
        if let Some(orphan_info) = self.pending_orphans_queue.remove(block_id) {
            debug!(target: LOG_TARGET, ?orphan_info, "Orphan block removed from queue");
        }
    }

    pub fn cancel_active_download(&mut self) {
        if let DownloaderState::Downloading(download) = &mut self.state {
            let orphan_id = download.orphan_block_id();
            self.pending_orphans_queue.remove(&orphan_id);
            self.state = DownloaderState::Idle;
        }

        if let Some(waker) = &self.waker {
            waker.wake_by_ref();
        }
    }

    pub fn should_poll(&self) -> bool {
        !self.pending_orphans_queue.is_empty() || !matches!(self.state, DownloaderState::Idle)
    }

    fn get_next_stream_input(&mut self) -> Option<(OrphanInfo, HashSet<HeaderId>)> {
        if let DownloaderState::Downloading(ActiveDownload {
            orphan_info,
            last_block_id: Some(last_block_id),
            ..
        }) = &mut self.state
        {
            let orphan_info = orphan_info.clone();
            let last_block_id = *last_block_id;
            return Some((orphan_info, HashSet::from([last_block_id])));
        }

        self.dequeue_next_orphan()
            .map(|orphan_info| (orphan_info, HashSet::new()))
    }
}

impl<NetAdapter, RuntimeServiceId> Stream for OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId> + Send + Sync + Clone + 'static,
    NetAdapter::Block: Clone + Send + Sync + 'static,
    RuntimeServiceId: Send + Sync + 'static,
{
    type Item = NetAdapter::Block;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.waker = Some(cx.waker().clone());

        match &mut self.state {
            DownloaderState::Idle => {
                let Some((orphan_info, known_blocks)) = self.get_next_stream_input() else {
                    return Poll::Pending;
                };

                debug!(target: LOG_TARGET, ?orphan_info, ?known_blocks, "Starting new orphan block download");
                let request_blocks_stream_fut = Self::request_blocks_stream(
                    self.network_adapter.clone(),
                    orphan_info,
                    known_blocks,
                );

                self.state = DownloaderState::Requesting(Box::pin(request_blocks_stream_fut));

                cx.waker().wake_by_ref();
                Poll::Pending
            }
            DownloaderState::Requesting(request) => match request.as_mut().poll(cx) {
                Poll::Ready(Ok(download)) => {
                    self.state = DownloaderState::Downloading(download);

                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Poll::Ready(Err(e)) => {
                    error!(target: LOG_TARGET, "Error while starting download: {e}");
                    self.state = DownloaderState::Idle;

                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Poll::Pending => Poll::Pending,
            },
            DownloaderState::Downloading(download) => {
                let Some(block_stream) = download.block_stream.as_mut() else {
                    self.state = DownloaderState::Idle;
                    return Poll::Pending;
                };

                match block_stream.poll_next_unpin(cx) {
                    Poll::Ready(Some(Ok((block_id, block)))) => {
                        download.last_block_id = Some(block_id);
                        download.total_blocks_received = download
                            .total_blocks_received
                            .checked_add(1)
                            .expect("Block count overflow");

                        if download.orphan_info.orphan_id == block_id {
                            info!(
                                target: LOG_TARGET,
                                ?block_id,
                                total_blocks_received = download.total_blocks_received,
                                "Sync for orphan block completed"
                            );
                            self.state = DownloaderState::Idle;
                        }

                        self.pending_orphans_queue.remove(&block_id);

                        cx.waker().wake_by_ref();
                        Poll::Ready(Some(block))
                    }
                    Poll::Ready(Some(Err(e))) => {
                        error!(target: LOG_TARGET, "Error while fetching blocks: {e}");

                        self.state = DownloaderState::Idle;

                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                    Poll::Ready(None) => {
                        download.block_stream = None;

                        if let Some(last_block_id) = download.last_block_id {
                            let orphan_info = download.orphan_info.clone();
                            let known_blocks = HashSet::from([last_block_id]);

                            debug!(
                                target: LOG_TARGET, ?orphan_info, ?known_blocks,
                                "Previous stream ended, requesting next stream for remaining blocks"
                            );

                            let request_blocks_stream_fut = Self::request_blocks_stream(
                                self.network_adapter.clone(),
                                orphan_info,
                                known_blocks,
                            );

                            self.state =
                                DownloaderState::Requesting(Box::pin(request_blocks_stream_fut));

                            cx.waker().wake_by_ref();
                        } else {
                            debug!(
                                target: LOG_TARGET,
                                orphan_info = ?download.orphan_info,
                                "No blocks received for this orphan, ending sync"
                            );
                            let orphan_id = download.orphan_block_id();
                            self.pending_orphans_queue.remove(&orphan_id);

                            self.state = DownloaderState::Idle;
                        }

                        Poll::Pending
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

impl<NetAdapter, RuntimeServiceId> Unpin for OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>
where
    NetAdapter: NetworkAdapter<RuntimeServiceId>,
    NetAdapter::Block: Clone + Send + Sync + 'static,
    RuntimeServiceId: Send + Sync + 'static,
{
}

#[cfg(test)]
mod tests {
    use std::{
        pin,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use futures::stream;
    use lb_cryptarchia_sync::GetTipResponse;
    use lb_network_service::{NetworkService, backends::mock::Mock, message::ChainSyncEvent};
    use overwatch::services::{ServiceData, relay::OutboundRelay};
    use tokio::time::timeout;

    use super::*;

    type TestBlock = HeaderId;
    type OrphanResults = Vec<Result<TestBlock, String>>;

    fn create_downloader() -> OrphanBlocksDownloader<MockNetworkAdapter, usize> {
        let network = MockNetworkAdapter::new();
        OrphanBlocksDownloader::new(network, ORPHAN_CACHE_SIZE)
    }

    fn create_downloader_with_responses<T>(
        chain: &[TestBlock],
        orphan_configs: Vec<(usize, Vec<T>)>,
    ) -> OrphanBlocksDownloader<MockNetworkAdapter, usize>
    where
        T: IntoBlockResult + Clone,
    {
        let mut network = MockNetworkAdapter::new();

        for (orphan_index, response_sequences) in orphan_configs {
            let orphan_id = chain[orphan_index];
            let mut last_block_id = None;

            for (i, responses) in response_sequences.iter().enumerate() {
                let block_results = responses.clone().into_block_result(chain);
                let known_block = if i == 0 { None } else { last_block_id };

                let next_last_block_id = block_results
                    .last()
                    .and_then(|result| result.as_ref().map_or(None, |block| Some(*block)));

                network = network.with_stream(orphan_id, known_block, block_results);

                last_block_id = next_last_block_id;
            }
        }

        OrphanBlocksDownloader::new(network, ORPHAN_CACHE_SIZE)
    }

    trait IntoBlockResult {
        fn into_block_result(self, chain: &[TestBlock]) -> Vec<Result<TestBlock, String>>;
    }

    impl IntoBlockResult for Vec<usize> {
        fn into_block_result(self, chain: &[TestBlock]) -> Vec<Result<TestBlock, String>> {
            self.iter().map(|&i| Ok(chain[i])).collect()
        }
    }

    impl IntoBlockResult for Vec<Result<TestBlock, String>> {
        fn into_block_result(self, _chain: &[TestBlock]) -> Vec<Result<TestBlock, String>> {
            self
        }
    }

    impl IntoBlockResult for &[usize] {
        fn into_block_result(self, chain: &[TestBlock]) -> Vec<Result<TestBlock, String>> {
            self.iter().map(|&i| Ok(chain[i])).collect()
        }
    }

    async fn receive_blocks<S>(stream: &mut Pin<&mut S>, expected_count: usize) -> Vec<TestBlock>
    where
        S: Stream<Item = TestBlock> + Unpin,
    {
        let mut blocks = Vec::new();

        match timeout(Duration::from_millis(100), async {
            for _ in 0..expected_count {
                match stream.next().await {
                    Some(block) => blocks.push(block),
                    None => break,
                }
            }
        })
        .await
        {
            Err(_) | Ok(()) => blocks,
        }
    }

    fn create_chain(size: usize) -> Vec<TestBlock> {
        (0..size).map(|i| HeaderId::from([i as u8; 32])).collect()
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Request {
        target: HeaderId,
        local_tip: HeaderId,
        lib: HeaderId,
        additional_blocks: HashSet<HeaderId>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct ResponseKey {
        orphan_id: HeaderId,
        known_block: Option<HeaderId>,
    }

    #[derive(Clone)]
    struct MockNetworkAdapter {
        requests: Arc<Mutex<Vec<Request>>>,
        responses: Arc<Mutex<HashMap<ResponseKey, OrphanResults>>>,
    }

    impl Unpin for MockNetworkAdapter {}

    impl MockNetworkAdapter {
        fn new() -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(HashMap::new())),
            }
        }

        fn with_stream(
            self,
            orphan_id: HeaderId,
            known_block: Option<HeaderId>,
            responses: Vec<Result<TestBlock, String>>,
        ) -> Self {
            let key = ResponseKey {
                orphan_id,
                known_block,
            };
            self.responses.lock().unwrap().insert(key, responses);
            self
        }
    }

    #[async_trait::async_trait]
    impl<RuntimeServiceId: Send + Sync> NetworkAdapter<RuntimeServiceId> for MockNetworkAdapter {
        type Backend = Mock;
        type Settings = ();
        type PeerId = ();
        type Block = TestBlock;
        type Proposal = ();

        async fn new(
            _settings: Self::Settings,
            _network_relay: OutboundRelay<
                <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
            >,
        ) -> Self {
            Self::new()
        }

        async fn proposals_stream(&self) -> Result<BoxedStream<Self::Proposal>, DynError> {
            unimplemented!()
        }

        async fn chainsync_events_stream(&self) -> Result<BoxedStream<ChainSyncEvent>, DynError> {
            unimplemented!()
        }

        async fn request_tip(&self, _peer: Self::PeerId) -> Result<GetTipResponse, DynError> {
            unimplemented!()
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
            let request = Request {
                target: target_block,
                local_tip,
                lib: latest_immutable_block,
                additional_blocks: additional_blocks.clone(),
            };

            self.requests.lock().unwrap().push(request);

            let key = ResponseKey {
                orphan_id: target_block,
                known_block: additional_blocks.iter().next().copied(),
            };

            self.responses.lock().unwrap().remove(&key).map_or_else(
                || Err("No response for this request".into()),
                |blocks| {
                    let stream = stream::iter(blocks)
                        .map(|result| result.map(|block| (block, block)).map_err(Into::into))
                        .boxed();
                    Ok(Box::new(stream) as BoxedStream<Result<(HeaderId, Self::Block), DynError>>)
                },
            )
        }
    }

    const ORPHAN_CACHE_SIZE: NonZeroUsize =
        NonZeroUsize::new(5).expect("ORPHAN_CACHE_SIZE must be non-zero");

    const TEST_TIP: [u8; 32] = [10u8; 32];
    const TEST_LIB: [u8; 32] = [11u8; 32];

    #[tokio::test]
    async fn test_orphan_cache_limit() {
        let mut downloader = create_downloader();

        let mut added_orphans = Vec::new();
        for i in 0..downloader.max_pending_orphans.get() {
            let orphan = [i as u8; 32].into();
            downloader.enqueue_orphan(orphan, TEST_TIP.into(), TEST_LIB.into());
            added_orphans.push(orphan);
        }

        for orphan in &added_orphans {
            assert!(
                downloader
                    .pending_orphans_queue
                    .iter()
                    .any(|(key, _)| *key == *orphan)
            );
        }

        let extra_orphan = [255u8; 32].into();
        downloader.enqueue_orphan(extra_orphan, TEST_TIP.into(), TEST_LIB.into());

        assert_eq!(
            downloader.pending_orphans_queue.len(),
            downloader.max_pending_orphans.get()
        );

        assert!(
            !downloader
                .pending_orphans_queue
                .iter()
                .any(|(key, _)| *key == extra_orphan)
        );
    }

    #[tokio::test]
    async fn test_orphans_with_middle_errors() {
        let chain = create_chain(8);

        let mut downloader = create_downloader_with_responses(
            &chain,
            vec![
                (
                    2,
                    vec![vec![
                        Ok(chain[0]),
                        Ok(chain[1]),
                        Err("Network error".to_owned()),
                        Ok(chain[2]),
                    ]],
                ),
                (
                    6,
                    vec![vec![
                        Ok(chain[5]),
                        Ok(chain[6]),
                        Err("Network error".to_owned()),
                        Ok(chain[7]),
                    ]],
                ),
            ],
        );
        downloader.enqueue_orphan(chain[2], TEST_TIP.into(), TEST_LIB.into());
        downloader.enqueue_orphan(chain[6], TEST_TIP.into(), TEST_LIB.into());

        let mut downloader = pin::pin!(downloader);
        let received_blocks = receive_blocks(&mut downloader, 6).await;

        let first_block = received_blocks.first().unwrap();
        let is_orphan_2_first = *first_block == chain[0];

        let expected = if is_orphan_2_first {
            &[chain[0..=1].to_vec(), chain[5..=6].to_vec()].concat()
        } else {
            &[chain[5..=6].to_vec(), chain[0..=1].to_vec()].concat()
        };
        assert_eq!(&received_blocks, expected);
    }

    #[tokio::test]
    async fn test_multiple_orphans() {
        let chain = create_chain(10);

        let mut downloader = create_downloader_with_responses(
            &chain,
            vec![(2, vec![vec![0, 1, 2]]), (7, vec![vec![5, 6, 7]])],
        );

        downloader.enqueue_orphan(chain[2], TEST_TIP.into(), TEST_LIB.into());
        downloader.enqueue_orphan(chain[7], TEST_TIP.into(), TEST_LIB.into());

        let mut downloader = pin::pin!(downloader);
        let received_blocks = receive_blocks(&mut downloader, 6).await;

        let first_block = received_blocks.first().unwrap();
        let is_orphan_2_first = *first_block == chain[0];

        let expected = if is_orphan_2_first {
            &[chain[0..=2].to_vec(), chain[5..=7].to_vec()].concat()
        } else {
            &[chain[5..=7].to_vec(), chain[0..=2].to_vec()].concat()
        };
        assert_eq!(&received_blocks, expected);
    }

    #[tokio::test]
    async fn test_following_request() {
        let chain = create_chain(5);

        let mut downloader =
            create_downloader_with_responses(&chain, vec![(4, vec![vec![0, 1, 2], vec![3, 4]])]);

        downloader.enqueue_orphan(chain[4], TEST_TIP.into(), TEST_LIB.into());

        let mut downloader = pin::pin!(downloader);
        let received_blocks = receive_blocks(&mut downloader, 5).await;

        assert_eq!(&received_blocks, &chain);
    }

    #[tokio::test]
    async fn test_multiple_streams() {
        let chain = create_chain(10);

        let mut downloader = create_downloader_with_responses(
            &chain,
            vec![(9, vec![vec![0, 1, 2], vec![3, 4, 5], Vec::<usize>::new()])],
        );

        downloader.enqueue_orphan(chain[9], TEST_TIP.into(), TEST_LIB.into());

        let mut downloader = pin::pin!(downloader);

        let received_blocks = receive_blocks(&mut downloader, 10).await;

        let received_ids: Vec<_> = received_blocks.iter().collect();
        let expected_ids: Vec<_> = chain[0..6].iter().collect();
        assert_eq!(received_ids, expected_ids);

        let requests = downloader
            .as_mut()
            .get_mut()
            .network_adapter
            .requests
            .lock()
            .unwrap();
        assert_eq!(requests.len(), 3);

        drop(requests);
    }

    #[tokio::test]
    async fn test_cancel_active_stream_while_processing() {
        let chain = create_chain(10);

        let mut downloader = create_downloader_with_responses(
            &chain,
            vec![(2, vec![vec![0, 1, 2]]), (7, vec![vec![5, 6, 7]])],
        );

        downloader.enqueue_orphan(chain[2], TEST_TIP.into(), TEST_LIB.into());
        downloader.enqueue_orphan(chain[7], TEST_TIP.into(), TEST_LIB.into());

        let mut received_blocks = Vec::new();
        let mut downloader = pin::pin!(downloader);

        if let Some(block) = downloader.next().await {
            received_blocks.push(block);
        }

        let downloader_mut = downloader.as_mut().get_mut();
        downloader_mut.cancel_active_download();

        let additional_blocks = receive_blocks(&mut downloader, 3).await;
        received_blocks.extend(additional_blocks);

        assert_eq!(received_blocks.len(), 4);

        let is_orphan_1_first = received_blocks[0] == chain[0];

        if is_orphan_1_first {
            assert_eq!(received_blocks[0], chain[0]);
            assert_eq!(received_blocks[1], chain[5]);
            assert_eq!(received_blocks[2], chain[6]);
            assert_eq!(received_blocks[3], chain[7]);
        } else {
            assert_eq!(received_blocks[0], chain[5]);
            assert_eq!(received_blocks[1], chain[0]);
            assert_eq!(received_blocks[2], chain[1]);
            assert_eq!(received_blocks[3], chain[2]);
        }
    }

    #[tokio::test]
    async fn test_add_duplicate_orphan_while_streaming() {
        let chain = create_chain(5);

        let mut downloader =
            create_downloader_with_responses(&chain, vec![(4, vec![vec![0, 1, 2, 3, 4]])]);

        downloader.enqueue_orphan(chain[4], TEST_TIP.into(), TEST_LIB.into());

        let mut downloader = pin::pin!(downloader);

        if downloader.next().await.is_some() {
            downloader.as_mut().get_mut().enqueue_orphan(
                chain[4],
                [20u8; 32].into(),
                [21u8; 32].into(),
            );

            assert!(
                !downloader
                    .as_mut()
                    .get_mut()
                    .pending_orphans_queue
                    .contains_key(&chain[4])
            );

            assert!(matches!(
                downloader.as_mut().get_mut().state,
                DownloaderState::Downloading(_)
            ));
            if let DownloaderState::Downloading(download) = &downloader.as_mut().get_mut().state {
                assert_eq!(download.orphan_block_id(), chain[4]);
            }
        }
    }

    #[tokio::test]
    async fn test_cancel_orphan_during_streaming() {
        let chain = create_chain(8);

        let mut downloader = create_downloader_with_responses(
            &chain,
            vec![(3, vec![vec![0, 1, 2, 3]]), (7, vec![vec![4, 5, 6, 7]])],
        );

        downloader.enqueue_orphan(chain[3], TEST_TIP.into(), TEST_LIB.into());
        downloader.enqueue_orphan(chain[7], TEST_TIP.into(), TEST_LIB.into());

        let mut received_blocks = Vec::new();
        let mut downloader = pin::pin!(downloader);

        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        if let Poll::Ready(Some(block)) = downloader.as_mut().poll_next(&mut cx) {
            received_blocks.push(block);

            downloader.as_mut().get_mut().cancel_active_download();
        }

        let additional_blocks = receive_blocks(&mut downloader, 4).await;
        received_blocks.extend(additional_blocks);

        let is_from_orphan1 = received_blocks[0] == chain[0];
        let expected_blocks = if is_from_orphan1 {
            &chain[0..=3]
        } else {
            &chain[4..=7]
        };

        assert_eq!(&received_blocks, expected_blocks);
    }

    #[tokio::test]
    async fn test_add_orphan_after_idle() {
        let chain = create_chain(6);

        let mut downloader = create_downloader_with_responses(
            &chain,
            vec![(2, vec![vec![0, 1, 2]]), (5, vec![vec![3, 4, 5]])],
        );

        downloader.enqueue_orphan(chain[2], TEST_TIP.into(), TEST_LIB.into());

        let mut downloader = pin::pin!(downloader);

        let first_batch = receive_blocks(&mut downloader, 3).await;
        let expected = &chain[0..=2];
        assert_eq!(&first_batch, expected);

        downloader
            .as_mut()
            .get_mut()
            .enqueue_orphan(chain[5], TEST_TIP.into(), TEST_LIB.into());

        let second_batch = receive_blocks(&mut downloader, 3).await;
        let expected = &chain[3..=5];
        assert_eq!(&second_batch, expected);

        let all_blocks: Vec<_> = first_batch.into_iter().chain(second_batch).collect();
        assert_eq!(&all_blocks, &chain);
    }

    #[tokio::test]
    async fn test_stop_at_orphan_block() {
        let chain = create_chain(5);
        let mut downloader =
            create_downloader_with_responses(&chain, vec![(2, vec![vec![0, 1, 2, 3, 4]])]);

        downloader.enqueue_orphan(chain[2], TEST_TIP.into(), TEST_LIB.into());

        let mut downloader = pin::pin!(downloader);
        let received_blocks = receive_blocks(&mut downloader, 3).await;

        assert_eq!(&received_blocks, &chain[0..=2]);

        assert!(matches!(
            downloader.as_mut().get_mut().state,
            DownloaderState::Idle
        ));
    }
}
