use std::{
    collections::HashSet,
    fmt::{Debug, Formatter},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use cryptarchia_sync::HeaderId;
use futures::{Stream, StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use overwatch::DynError;
use tracing::{debug, info};

use crate::network::BoxedStream;

/// Manages on-going [`Download`]s and [`Delay`]s.
///
/// This implements [`futures::Stream`] that yields [`DownloadsOutput`]s
/// either a [`DownloadResult`] that contains a block downloaded from a peer,
/// or a [`Delay`] that has completed.
pub struct Downloads<'a, NodeId, Block> {
    /// [`Future`]s that read a single block from a [`Download`].
    downloads: FuturesUnordered<BoxFuture<'a, DownloadResult<NodeId, Block>>>,
    /// A set of blocks that are being targeted by [`Self::downloads`].
    targets: HashSet<HeaderId>,
    /// [`Delay`] for peers that have no download needed at the moment.
    delays: FuturesUnordered<BoxFuture<'a, Delay<NodeId>>>,
    /// The duration of a delay.
    delay_duration: Duration,
}

type DownloadResult<NodeId, Block> = Result<
    (Option<(HeaderId, Block)>, Download<NodeId, Block>),
    (DynError, Download<NodeId, Block>),
>;

impl<NodeId, Block> Stream for Downloads<'_, NodeId, Block>
where
    NodeId: Debug,
{
    type Item = DownloadsOutput<NodeId, Block>;

    /// Returns a [`DownloadsOutput::DelayCompleted`] if there is a [`Delay`]
    /// that has completed.
    ///
    /// Otherwise, it returns a [`DownloadsOutput::BlockReceived`] if there is
    /// a block downloaded from one of the registered [`Download`]s.
    ///
    /// If there is a [`Download`] that has completed,
    /// a [`DownloadsOutput::DownloadCompleted`] is returned.
    ///
    /// A [`DownloadsOutput::Error`] is returned if an error occurs while
    /// processing a [`Download`].
    ///
    /// The [`Stream`] ends once there is no [`Download`]s in progress,
    /// even if there are [`Delay`]s in progress.
    ///
    /// A [`Download`] is always returned as part of [`DownloadsOutput`],
    /// so that the caller can re-add it to the [`Downloads`] if needed.
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Check if there is any delay that has completed.
        if let Poll::Ready(Some(delay)) = self.delays.poll_next_unpin(cx) {
            return Poll::Ready(Some(DownloadsOutput::DelayCompleted(delay)));
        }

        // Check if there is any download ready to be processed.
        match self.downloads.poll_next_unpin(cx) {
            Poll::Ready(Some(result)) => match result {
                Ok((Some((_, block)), download)) => {
                    self.targets.remove(&download.target);
                    Poll::Ready(Some(DownloadsOutput::BlockReceived { block, download }))
                }
                Ok((None, download)) => {
                    self.targets.remove(&download.target);
                    Poll::Ready(Some(DownloadsOutput::DownloadCompleted(download)))
                }
                Err((error, download)) => {
                    self.targets.remove(&download.target);
                    Poll::Ready(Some(DownloadsOutput::Error { error, download }))
                }
            },
            Poll::Ready(None) => {
                info!("All downloads have been done");
                // Finish the Stream even if there are delays in progress.
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<'a, NodeId, Block> Downloads<'a, NodeId, Block>
where
    NodeId: Debug + Send + Unpin + 'a,
    Block: Unpin + 'a,
{
    pub fn new(delay_duration: Duration) -> Self {
        Self {
            downloads: FuturesUnordered::new(),
            targets: HashSet::new(),
            delays: FuturesUnordered::new(),
            delay_duration,
        }
    }

    /// Register a download to be processed.
    ///
    /// If there is already any registered download for the same target,
    /// the download is ignored and a delay is scheduled for the peer.
    pub fn add_download(&mut self, download: Download<NodeId, Block>) {
        if !self.targets.insert(download.target) {
            debug!(
                "Download for target {:?} already exists. Delaying the peer {:?}",
                download.target, download.peer
            );
            self.add_delay(Delay {
                peer: download.peer,
                latest_downloaded_block: download.last,
            });
            return;
        }

        // Register a future that reads only a single block from the stream.
        self.downloads.push(Box::pin(async move {
            let mut download = download;
            match download.next().await {
                Some(Ok((block_id, block))) => {
                    download.last = Some(block_id);
                    Ok((Some((block_id, block)), download))
                }
                Some(Err(e)) => Err((e, download)),
                None => Ok((None, download)),
            }
        }));
    }

    // Register a delay for a peer with the configured duration.
    pub fn add_delay(&self, delay: Delay<NodeId>) {
        let duration = self.delay_duration;
        self.delays.push(Box::pin(async move {
            tokio::time::sleep(duration).await;
            delay
        }));
    }

    pub const fn targets(&self) -> &HashSet<HeaderId> {
        &self.targets
    }
}

impl<NodeId, Block> Downloads<'_, NodeId, Block> {
    /// Returns true if there is no download and no delay in progress.
    pub fn is_empty(&self) -> bool {
        self.num_peers() == 0
    }

    /// Returns the number of peers currently registered
    /// (either for download or for delay).
    fn num_peers(&self) -> usize {
        self.downloads.len() + self.delays.len()
    }
}

/// Output of the [`Downloads::poll_next`].
pub enum DownloadsOutput<NodeId, Block> {
    DelayCompleted(Delay<NodeId>),
    BlockReceived {
        block: Block,
        download: Download<NodeId, Block>,
    },
    DownloadCompleted(Download<NodeId, Block>),
    Error {
        error: DynError,
        download: Download<NodeId, Block>,
    },
}

/// A download from a specific peer.
///
/// It implements [`futures::Stream`] that yields blocks downloaded.
/// It continues until the target block is reached or the stream ends.
pub struct Download<NodeId, Block> {
    peer: NodeId,
    /// The target block this download aims to reach.
    target: HeaderId,
    /// A stream of blocks that may continue up to [`Self::target`].
    stream: BoxedStream<Result<(HeaderId, Block), DynError>>,
    /// The last block that was read from [`Self::stream`].
    /// [`None`] if no blocks were read yet.
    last: Option<HeaderId>,
}

impl<NodeId, Block> Download<NodeId, Block> {
    pub fn new(
        peer: NodeId,
        target: HeaderId,
        stream: BoxedStream<Result<(HeaderId, Block), DynError>>,
    ) -> Self {
        Self {
            peer,
            target,
            stream,
            last: None,
        }
    }

    pub const fn peer(&self) -> &NodeId {
        &self.peer
    }

    pub const fn last(&self) -> Option<HeaderId> {
        self.last
    }
}

impl<NodeId, Block> Stream for Download<NodeId, Block>
where
    NodeId: Unpin,
    Block: Unpin,
{
    type Item = Result<(HeaderId, Block), DynError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Check if the target block has already been reached.
        if let Some(last) = self.last
            && last == self.target
        {
            return Poll::Ready(None);
        }

        // Check if there is a block ready to be returned.
        match self.stream.poll_next_unpin(cx) {
            Poll::Ready(Some(result)) => match result {
                Ok((id, block)) => {
                    self.last = Some(id);
                    Poll::Ready(Some(Ok((id, block))))
                }
                Err(e) => Poll::Ready(Some(Err(e))),
            },
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[expect(clippy::missing_fields_in_debug, reason = "BoxedStream")]
impl<NodeId, Block> Debug for Download<NodeId, Block>
where
    NodeId: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Download")
            .field("peer", &self.peer)
            .field("target", &self.target)
            .field("last", &self.last)
            .finish()
    }
}

/// A delay for a peer that has no download at the moment.
pub struct Delay<NodeId> {
    peer: NodeId,
    /// The latest block that was downloaded from the peer.
    /// This is used for the next download attempt.
    latest_downloaded_block: Option<HeaderId>,
}

impl<NodeId> Delay<NodeId> {
    pub const fn new(peer: NodeId, latest_downloaded_block: Option<HeaderId>) -> Self {
        Self {
            peer,
            latest_downloaded_block,
        }
    }

    pub const fn peer(&self) -> &NodeId {
        &self.peer
    }

    pub const fn latest_downloaded_block(&self) -> Option<HeaderId> {
        self.latest_downloaded_block
    }
}

#[cfg(test)]
mod tests {
    use futures::{FutureExt as _, StreamExt as _, stream};

    use super::*;

    #[tokio::test]
    async fn download_empty_stream() {
        let peer: TestNodeId = 1;
        let target = header_id(1);
        let stream = block_stream(vec![]);
        let mut download = Download::new(peer, target, stream);

        assert!(download.next().await.is_none());
    }

    #[tokio::test]
    async fn download_blocks_until_target() {
        let peer: TestNodeId = 1;
        let target = header_id(3);
        let stream = block_stream(vec![
            Ok((header_id(1), 100)),
            Ok((header_id(2), 200)),
            Ok((header_id(3), 300)),
            // This should not be returned since target is the 3.
            Ok((header_id(4), 400)),
        ]);
        let mut download = Download::new(peer, target, stream);

        // Get first block
        let (id, block) = download.next().await.unwrap().unwrap();
        assert_eq!(id, header_id(1));
        assert_eq!(block, 100);
        assert_eq!(download.last, Some(header_id(1)));

        // Get second block
        let (id, block) = download.next().await.unwrap().unwrap();
        assert_eq!(id, header_id(2));
        assert_eq!(block, 200);
        assert_eq!(download.last, Some(header_id(2)));

        // Get third block (target)
        let (id, block) = download.next().await.unwrap().unwrap();
        assert_eq!(id, header_id(3));
        assert_eq!(block, 300);
        assert_eq!(download.last, Some(header_id(3)));

        // Should stop here since target is reached
        assert!(download.next().await.is_none());
    }

    #[tokio::test]
    async fn download_blocks_if_no_target_in_stream() {
        let peer: TestNodeId = 1;
        let target = header_id(4);
        let stream = block_stream(vec![
            Ok((header_id(1), 100)),
            Ok((header_id(2), 200)),
            // Target (4) is not in the stream.
        ]);
        let mut download = Download::new(peer, target, stream);

        // Get first block
        let (id, block) = download.next().await.unwrap().unwrap();
        assert_eq!(id, header_id(1));
        assert_eq!(block, 100);
        assert_eq!(download.last, Some(header_id(1)));

        // Get second block
        let (id, block) = download.next().await.unwrap().unwrap();
        assert_eq!(id, header_id(2));
        assert_eq!(block, 200);
        assert_eq!(download.last, Some(header_id(2)));

        // Should stop here even though target is not reached
        assert!(download.next().await.is_none());
    }

    #[tokio::test]
    async fn download_with_error() {
        let peer: TestNodeId = 1;
        let target = header_id(3);
        let stream = block_stream(vec![
            Ok((header_id(1), 100)),
            Err(DynError::from("test error")),
        ]);
        let mut download = Download::new(peer, target, stream);

        // Get first block
        let (id, block) = download.next().await.unwrap().unwrap();
        assert_eq!(id, header_id(1));
        assert_eq!(block, 100);
        assert_eq!(download.last, Some(header_id(1)));

        // Get error
        let result = download.next().await.unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn add_single_download() {
        let mut downloads = Downloads::new(Duration::from_millis(1));
        let target = header_id(2);
        let download = Download::new(
            1,
            target,
            block_stream(vec![Ok((header_id(1), 100)), Ok((header_id(2), 200))]),
        );

        // Add download to Downloads
        downloads.add_download(download);
        assert!(downloads.targets().contains(&target));
        assert_eq!(downloads.num_peers(), 1);

        // Should yield a BlockReceived output
        let DownloadsOutput::BlockReceived { block, download } = downloads.next().await.unwrap()
        else {
            panic!("Expected BlockReceived output");
        };
        assert!(!downloads.targets().contains(&target));
        assert_eq!(block, 100);

        // Add download to Downloads again
        downloads.add_download(download);
        assert!(downloads.targets().contains(&target));
        assert_eq!(downloads.num_peers(), 1);

        // Should yield a BlockReceived output
        let DownloadsOutput::BlockReceived { block, download } = downloads.next().await.unwrap()
        else {
            panic!("Expected BlockReceived output");
        };
        assert!(!downloads.targets().contains(&target));
        assert_eq!(downloads.num_peers(), 0);
        assert_eq!(block, 200);

        // Add download to Downloads again
        downloads.add_download(download);
        assert!(downloads.targets().contains(&target));
        assert_eq!(downloads.num_peers(), 1);

        // Should yield a DownloadCompleted output
        assert!(matches!(
            downloads.next().await,
            Some(DownloadsOutput::DownloadCompleted(_))
        ));
        assert!(!downloads.targets().contains(&target));
        assert_eq!(downloads.num_peers(), 0);

        // Should yield a None since no download is in the Downloads.
        assert!(downloads.next().await.is_none());
    }

    #[tokio::test]
    async fn add_single_download_with_error() {
        let mut downloads = Downloads::new(Duration::from_millis(1));
        let target = header_id(2);
        let download = Download::new(
            1,
            target,
            block_stream(vec![
                Ok((header_id(1), 100)),
                Err(DynError::from("test error")),
            ]),
        );

        // Add download to Downloads
        downloads.add_download(download);
        assert!(downloads.targets().contains(&target));
        assert_eq!(downloads.num_peers(), 1);

        // Should yield a BlockReceived output
        let DownloadsOutput::BlockReceived { block, download } = downloads.next().await.unwrap()
        else {
            panic!("Expected BlockReceived output");
        };
        assert!(!downloads.targets().contains(&target));
        assert_eq!(downloads.num_peers(), 0);
        assert_eq!(block, 100);

        // Add download to Downloads again
        downloads.add_download(download);
        assert!(downloads.targets().contains(&target));
        assert_eq!(downloads.num_peers(), 1);

        // Should yield a Error output
        let DownloadsOutput::Error { .. } = downloads.next().await.unwrap() else {
            panic!("Expected BlockReceived output");
        };
        assert!(!downloads.targets().contains(&target));
        assert_eq!(downloads.num_peers(), 0);

        // Should yield a None since no download is in the Downloads.
        assert!(downloads.next().await.is_none());
    }

    #[tokio::test]
    async fn add_multiple_downloads() {
        let mut downloads = Downloads::new(Duration::from_millis(1));

        // Download 1: Single block
        let peer1: TestNodeId = 1;
        let target1 = header_id(1);
        let download1 = Download::new(peer1, target1, block_stream(vec![Ok((header_id(1), 100))]));

        // Download 2: Two blocks
        let peer2: TestNodeId = 2;
        let target2 = header_id(3);
        let download2 = Download::new(
            peer2,
            target2,
            block_stream(vec![Ok((header_id(2), 200)), Ok((header_id(3), 300))]),
        );

        // Add all downloads to Downloads
        downloads.add_download(download1);
        downloads.add_download(download2);
        assert!(downloads.targets().contains(&target1));
        assert!(downloads.targets().contains(&target2));
        assert_eq!(downloads.num_peers(), 2);

        let mut expected_blocks = HashSet::<TestBlock>::from([100, 200, 300]);

        // Should yield a BlockReceived output
        let DownloadsOutput::BlockReceived { block, download } = downloads.next().await.unwrap()
        else {
            panic!("Expected BlockReceived output");
        };
        assert_eq!(downloads.num_peers(), 1);
        assert!(expected_blocks.remove(&block));

        downloads.add_download(download);
        assert_eq!(downloads.num_peers(), 2);

        match downloads.next().await {
            Some(DownloadsOutput::BlockReceived { block, download }) => {
                // The returned block should be one of the expected blocks.
                assert_eq!(downloads.num_peers(), 1);
                assert!(expected_blocks.remove(&block));
                downloads.add_download(download);
                assert_eq!(downloads.num_peers(), 2);
            }
            Some(DownloadsOutput::DownloadCompleted(download)) => {
                // If a download is completed at this point, it should be for peer1.
                assert_eq!(downloads.num_peers(), 1);
                assert_eq!(download.peer(), &peer1);
            }
            _ => panic!("Expected BlockReceived or DownloadCompleted output"),
        }

        match downloads.next().await {
            Some(DownloadsOutput::BlockReceived { block, download }) => {
                assert!(expected_blocks.remove(&block));
                downloads.add_download(download);
            }
            Some(DownloadsOutput::DownloadCompleted(download)) => {
                // Any peer can complete at this point.
                assert!(HashSet::from([peer1, peer2]).contains(download.peer()));
            }
            _ => panic!("Expected BlockReceived or DownloadCompleted output"),
        }

        match downloads.next().await {
            Some(DownloadsOutput::BlockReceived { block, download }) => {
                assert!(expected_blocks.remove(&block));
                downloads.add_download(download);
            }
            Some(DownloadsOutput::DownloadCompleted(download)) => {
                // Any peer can complete at this point.
                assert!(HashSet::from([peer1, peer2]).contains(download.peer()));
            }
            _ => panic!("Expected BlockReceived or DownloadCompleted output"),
        }

        // Since all expected blocks have been downloaded,
        // the next output should be DownloadCompleted.
        match downloads.next().await {
            Some(DownloadsOutput::DownloadCompleted(download)) => {
                assert_eq!(downloads.num_peers(), 0);
                assert!(HashSet::from([peer1, peer2]).contains(download.peer()));
            }
            _ => panic!("Expected BlockReceived or DownloadCompleted output"),
        }
    }

    #[tokio::test]
    async fn downloads_duplicate_targets() {
        let mut downloads = Downloads::new(Duration::from_millis(1));

        // Download 1
        let peer1: TestNodeId = 1;
        let target = header_id(1);
        let download1 = Download::new(peer1, target, block_stream(vec![Ok((header_id(1), 100))]));

        // Download 2: with the same target
        let peer2: TestNodeId = 2;
        let download2 = Download::new(peer2, target, block_stream(vec![Ok((header_id(1), 100))]));

        // Add all downloads to Downloads
        downloads.add_download(download1);
        downloads.add_download(download2);
        // Should only have one target.
        assert_eq!(downloads.targets(), &HashSet::from([target]));
        // But, should have two peers (one for download and one for delay).
        assert_eq!(downloads.num_peers(), 2);
        // One peer should be delayed due to the duplicate target.
        assert_eq!(downloads.delays.len(), 1);

        // Should yield a BlockReceived output from peer1
        let DownloadsOutput::BlockReceived { block, download } = downloads.next().await.unwrap()
        else {
            panic!("Expected BlockReceived output");
        };
        assert_eq!(downloads.num_peers(), 1);
        assert_eq!(block, 100);
        assert_eq!(download.peer(), &peer1);

        downloads.add_download(download);
        assert_eq!(downloads.num_peers(), 2);

        // Should yield a DownloadCompleted output from peer1
        let DownloadsOutput::DownloadCompleted(download) = downloads.next().await.unwrap() else {
            panic!("Expected DownloadedCompleted output");
        };
        assert_eq!(downloads.num_peers(), 1);
        assert_eq!(download.peer(), &peer1);

        // Should yield a None since no download is in progress,
        // even though there is a delay for peer2.
        assert_eq!(downloads.num_peers(), 1);
        assert_eq!(downloads.delays.len(), 1);
        assert!(downloads.next().await.is_none());
    }

    #[tokio::test]
    async fn downloads_complete_with_delay_in_progress() {
        let mut downloads = Downloads::new(Duration::from_secs(1));

        // Add a download for peer1
        let peer1: TestNodeId = 1;
        // An empty stream for simplicity
        let download = Download::new(peer1, header_id(1), block_stream(vec![]));
        downloads.add_download(download);

        // Add a delay for peer2
        let peer2: TestNodeId = 2;
        downloads.add_delay(Delay::new(peer2, None));

        // Should yield a DownloadCompleted output from peer1
        let DownloadsOutput::DownloadCompleted(download) = downloads.next().await.unwrap() else {
            panic!("Expected DownloadedCompleted output");
        };
        assert_eq!(download.peer(), &peer1);

        // Should yield a None since no download is in progress,
        // even though there is a delay for peer2.
        assert_eq!(downloads.delays.len(), 1);
        assert!(downloads.next().await.is_none());
    }

    #[tokio::test]
    async fn delay_completed_before_downloads_completed() {
        let mut downloads = Downloads::new(Duration::from_secs(1));

        // Add a slow download for peer1
        let peer1: TestNodeId = 1;
        // An empty stream for simplicity
        let download = Download::new(
            peer1,
            header_id(1),
            slow_block_stream(vec![Ok((header_id(1), 100))], Duration::from_secs(2)),
        );
        downloads.add_download(download);

        // Add a delay for peer2 that is not slower than the download for peer1
        let peer2: TestNodeId = 2;
        downloads.add_delay(Delay::new(peer2, None));

        // Should yield a DelayCompleted output from peer2
        let DownloadsOutput::DelayCompleted(delay) = downloads.next().await.unwrap() else {
            panic!("Expected DownloadedCompleted output");
        };
        assert_eq!(delay.peer(), &peer2);
        assert_eq!(delay.latest_downloaded_block(), None);

        // Should yield a BlockReceived output from peer1
        let DownloadsOutput::BlockReceived { block, download } = downloads.next().await.unwrap()
        else {
            panic!("Expected BlockReceived output");
        };
        assert_eq!(block, 100);
        assert_eq!(download.peer(), &peer1);

        // Should yield a None since no download is in progress.
        // (we didn't add the download back to the Downloads)
        assert_eq!(downloads.delays.len(), 0);
        assert!(downloads.next().await.is_none());
    }

    type TestNodeId = usize;
    type TestBlock = usize;

    fn header_id(id: u8) -> HeaderId {
        HeaderId::from([id; 32])
    }

    fn block_stream(
        blocks: Vec<Result<(HeaderId, TestBlock), DynError>>,
    ) -> BoxedStream<Result<(HeaderId, TestBlock), DynError>> {
        Box::new(stream::iter(blocks))
    }

    fn slow_block_stream(
        blocks: Vec<Result<(HeaderId, TestBlock), DynError>>,
        interval: Duration,
    ) -> BoxedStream<Result<(HeaderId, TestBlock), DynError>> {
        Box::new(stream::iter(blocks).then(move |block| {
            async move {
                tokio::time::sleep(interval).await;
                block
            }
            .boxed()
        }))
    }
}
