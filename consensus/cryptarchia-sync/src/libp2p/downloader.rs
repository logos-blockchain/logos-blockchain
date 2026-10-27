use std::{
    collections::VecDeque,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{
    Stream, StreamExt as _,
    stream::{self, Fuse, FusedStream as _},
};
use lb_log_targets::cryptarchia;
use libp2p::{PeerId, StreamProtocol};
use libp2p_stream::Control;
use tokio::{sync::oneshot, time, time::Duration};
use tracing::{debug, warn};

use crate::{
    BlocksRequest, TipResponse,
    libp2p::{
        behaviour::{BlocksResponseReader, BoxedStream, TipResponseReader},
        errors::{ChainSyncError, ChainSyncErrorKind},
        messages::{BlocksResponse, RequestMessage},
        packing::unpack_from_reader,
        utils::{self, open_stream, send_message},
    },
    messages::SerialisedBlock,
};

const LOG_TARGET: &str = cryptarchia::sync::libp2p::DOWNLOADER;

/// The maximum number of block bytes read ahead of the consumer of a
/// download. Reading ahead lets the providing peer finish the batch, and free
/// its download slot, while blocks are still being applied.
const DOWNLOAD_BUFFER_BYTES: usize = 64 * 1024 * 1024;

pub struct Downloader;

impl Downloader {
    pub async fn send_tip_request(
        peer_id: PeerId,
        control: &mut Control,
        protocol_name: StreamProtocol,
        reply_sender: oneshot::Sender<Result<TipResponse, ChainSyncError>>,
    ) -> Result<TipResponseReader, ChainSyncError> {
        let mut stream = open_stream(peer_id, control, protocol_name).await?;

        let tip_request = RequestMessage::Tip;
        send_message(peer_id, &mut stream, &tip_request).await?;

        Ok(TipResponseReader::new(peer_id, stream, reply_sender))
    }

    pub async fn send_blocks_request(
        peer_id: PeerId,
        mut control: Control,
        request: BlocksRequest,
        protocol_name: StreamProtocol,
        reply_sender: oneshot::Sender<BoxedStream<Result<SerialisedBlock, ChainSyncError>>>,
    ) -> Result<BlocksResponseReader, ChainSyncError> {
        let mut stream = open_stream(peer_id, &mut control, protocol_name).await?;

        let blocks_request = RequestMessage::Blocks(request);

        send_message(peer_id, &mut stream, &blocks_request).await?;

        Ok(BlocksResponseReader::new(peer_id, stream, reply_sender))
    }

    pub async fn receive_tip_response(
        tip_response_reader: TipResponseReader,
        timeout: Duration,
    ) -> Result<(), ChainSyncError> {
        let TipResponseReader {
            mut stream,
            peer_id,
            reply_channel,
        } = tip_response_reader;

        let response = time::timeout(timeout, unpack_from_reader::<TipResponse, _>(&mut stream))
            .await
            .map_err(|e| {
                warn!(target: LOG_TARGET, "Timeout while receiving tip from peer {}", peer_id);
                ChainSyncError::from((peer_id, e))
            })?
            .map_err(|e| ChainSyncError::from((peer_id, e)))
            .and_then(|response| match response {
                tip @ TipResponse::Tip { .. } => Ok(tip),
                TipResponse::Failure(reason) => Err(ChainSyncError {
                    peer: peer_id,
                    kind: ChainSyncErrorKind::RequestTipError(reason),
                }),
            });

        if let Err(e) = reply_channel.send(response) {
            debug!(
                target: LOG_TARGET,
                "Tip requester dropped before receiving response from peer {peer_id}: {e:?}"
            );
        }

        utils::close_stream(peer_id, stream).await
    }

    /// Receives the blocks response from the peer and delivers it to the
    /// caller of `request_blocks` as a stream of blocks. The response frames
    /// are read lazily as the caller polls the stream.
    pub fn receive_blocks_response(
        blocks_response_reader: BlocksResponseReader,
        timeout: Duration,
    ) -> Result<(), ChainSyncError> {
        let libp2p_stream = blocks_response_reader.stream;
        let peer_id = blocks_response_reader.peer_id;
        let reply_channel = blocks_response_reader.reply_channel;

        #[expect(
            closure_returning_async_block,
            reason = "Signature expected by `try_unfold`"
        )]
        let stream = stream::try_unfold(libp2p_stream, move |mut stream| async move {
            let response = time::timeout(timeout, unpack_from_reader(&mut stream)).await;

            match response {
                Ok(Ok(BlocksResponse::Block(block))) => Ok(Some((block, stream))),
                Ok(Ok(BlocksResponse::NoMoreBlocks)) => {
                    utils::close_stream(peer_id, stream).await?;
                    Ok(None)
                }
                Ok(Ok(BlocksResponse::Failure(reason))) => {
                    utils::close_stream(peer_id, stream).await?;

                    Err(ChainSyncError {
                        peer: peer_id,
                        kind: ChainSyncErrorKind::BlockProviderUnavailable(reason),
                    })
                }
                Ok(Err(e)) => {
                    drop(utils::close_stream(peer_id, stream).await);
                    Err(ChainSyncError::from((peer_id, e)))
                }
                Err(e) => {
                    drop(utils::close_stream(peer_id, stream).await);
                    Err(ChainSyncError::from((peer_id, e)))
                }
            }
        });

        let boxed_stream: BoxedStream<_> = Box::new(ReadAhead::new(Box::pin(stream)));
        reply_channel
            .send(boxed_stream)
            .map_err(|_| ChainSyncError {
                peer: peer_id,
                kind: ChainSyncErrorKind::ChannelSendError(
                    "Failed to send blocks stream".to_owned(),
                ),
            })
    }
}

/// A stream of downloaded blocks that reads ahead of its consumer.
///
/// Each poll takes every block the network has already delivered, up to
/// [`DOWNLOAD_BUFFER_BYTES`], and yields the oldest one. The transport keeps
/// receiving between polls, so the providing peer sends at network speed and
/// frees its provider slot while blocks are still being applied by the
/// consumer.
struct ReadAhead<S> {
    inner: Fuse<S>,
    buffer: VecDeque<Result<SerialisedBlock, ChainSyncError>>,
    buffered_bytes: usize,
}

impl<S: Stream> ReadAhead<S> {
    fn new(inner: S) -> Self {
        Self {
            inner: inner.fuse(),
            buffer: VecDeque::new(),
            buffered_bytes: 0,
        }
    }
}

impl<S> Stream for ReadAhead<S>
where
    S: Stream<Item = Result<SerialisedBlock, ChainSyncError>> + Unpin,
{
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        // Always read at least one block, however large.
        while this.buffer.is_empty() || this.buffered_bytes < DOWNLOAD_BUFFER_BYTES {
            match this.inner.poll_next_unpin(cx) {
                Poll::Ready(Some(item)) => {
                    this.buffered_bytes += buffered_len(&item);
                    this.buffer.push_back(item);
                }
                Poll::Ready(None) | Poll::Pending => break,
            }
        }

        match this.buffer.pop_front() {
            Some(item) => {
                this.buffered_bytes -= buffered_len(&item);
                Poll::Ready(Some(item))
            }
            None if this.inner.is_terminated() => Poll::Ready(None),
            None => Poll::Pending,
        }
    }
}

fn buffered_len(item: &Result<SerialisedBlock, ChainSyncError>) -> usize {
    item.as_ref().map_or(0, SerialisedBlock::len)
}
