use std::time::Duration;

use futures::{TryStreamExt as _, stream::BoxStream};
use lb_binary_codec::bincode::BoundedSerializeOp;
use lb_log_targets::cryptarchia;
use libp2p::{PeerId, Stream as Libp2pStream};
use serde::de::DeserializeOwned;
use tokio::{sync::mpsc, time};
use tracing::error;

use crate::{
    BlocksResponse, BlocksUnavailableReason, DynError, ProviderResponse, TipResponse,
    libp2p::{
        errors::{ChainSyncError, ChainSyncErrorKind},
        messages::{DownloadBlocksResponse, RequestMessage},
        packing::unpack_from_reader,
        utils::{close_stream, send_message},
    },
    messages::{GetTipResponse, SerialisedBlock},
};

const LOG_TARGET: &str = cryptarchia::sync::libp2p::PROVIDER;

pub const MAX_ADDITIONAL_BLOCKS: usize = 5;

pub struct Provider;

pub type ReceivingRequestStream = (PeerId, Libp2pStream, RequestMessage);

impl Provider {
    pub async fn process_request(
        peer_id: PeerId,
        mut stream: Libp2pStream,
        timeout: Duration,
    ) -> Result<ReceivingRequestStream, ChainSyncError> {
        let request: RequestMessage = time::timeout(timeout, unpack_from_reader(&mut stream))
            .await
            .map_err(|e| ChainSyncError::from((peer_id, e)))?
            .map_err(|e| ChainSyncError::from((peer_id, e)))?;

        Ok((peer_id, stream, request))
    }

    pub async fn provide_tip(
        mut reply_receiver: mpsc::Receiver<TipResponse>,
        peer_id: PeerId,
        mut libp2p_stream: Libp2pStream,
    ) -> Result<(), ChainSyncError> {
        let response = reply_receiver.recv().await.ok_or_else(|| ChainSyncError {
            peer: peer_id,
            kind: ChainSyncErrorKind::ChannelReceiveError(
                "Failed to receive tip from channel".to_owned(),
            ),
        })?;

        let send_result = match response {
            ProviderResponse::Available(tip) => {
                send_message(peer_id, &mut libp2p_stream, &tip).await
            }
            ProviderResponse::Unavailable { reason } => {
                let response = GetTipResponse::Failure(reason);
                send_message(peer_id, &mut libp2p_stream, &response).await
            }
        };

        drop(close_stream(peer_id, libp2p_stream).await);

        send_result
    }

    /// Serves one block download request. The whole response, from waiting
    /// for the block provider until the stream is closed, is bounded by
    /// `stream_timeout`, and each write by `send_timeout`.
    pub async fn provide_blocks(
        reply_receiver: mpsc::Receiver<BlocksResponse>,
        peer_id: PeerId,
        libp2p_stream: Libp2pStream,
        send_timeout: Duration,
        stream_timeout: Duration,
    ) -> Result<(), ChainSyncError> {
        time::timeout(
            stream_timeout,
            Self::respond_to_blocks_request(reply_receiver, peer_id, libp2p_stream, send_timeout),
        )
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))?
    }

    async fn respond_to_blocks_request(
        mut reply_receiver: mpsc::Receiver<BlocksResponse>,
        peer_id: PeerId,
        mut libp2p_stream: Libp2pStream,
        send_timeout: Duration,
    ) -> Result<(), ChainSyncError> {
        let response = reply_receiver.recv().await.ok_or_else(|| ChainSyncError {
            peer: peer_id,
            kind: ChainSyncErrorKind::ChannelReceiveError(
                "Failed to receive blocks stream from channel".to_owned(),
            ),
        })?;

        match response {
            ProviderResponse::Available(stream) => {
                Self::send_blocks(peer_id, stream, libp2p_stream, send_timeout).await
            }
            ProviderResponse::Unavailable { reason } => {
                let response = DownloadBlocksResponse::Failure(reason);
                send_message(peer_id, &mut libp2p_stream, &response).await?;
                drop(close_stream(peer_id, libp2p_stream).await);
                Ok(())
            }
        }
    }

    /// Sends every block of `stream` to the peer. Each write is bounded by
    /// `send_timeout`, so a peer that stops reading cannot hold the download
    /// slot indefinitely.
    async fn send_blocks(
        peer_id: PeerId,
        stream: BoxStream<'static, Result<SerialisedBlock, DynError>>,
        mut libp2p_stream: Libp2pStream,
        send_timeout: Duration,
    ) -> Result<(), ChainSyncError> {
        let result = stream
            .map_err(|e| ChainSyncError {
                peer: peer_id,
                kind: ChainSyncErrorKind::ReceivingBlocksError(format!(
                    "Failed to receive block from stream: {e}"
                )),
            })
            .try_fold(&mut libp2p_stream, async |stream, block| {
                let message = DownloadBlocksResponse::Block(block);
                send_message_with_timeout(peer_id, stream, &message, send_timeout).await?;
                Ok(stream)
            })
            .await;

        let final_result = match result {
            Ok(_) => {
                let message = DownloadBlocksResponse::NoMoreBlocks;
                send_message_with_timeout(peer_id, &mut libp2p_stream, &message, send_timeout).await
            }
            Err(e) => {
                error!(target: LOG_TARGET, "Failed to send blocks to peer {}: {}", peer_id, e);
                let message = DownloadBlocksResponse::Failure(BlocksUnavailableReason::Unknown);
                drop(
                    send_message_with_timeout(peer_id, &mut libp2p_stream, &message, send_timeout)
                        .await,
                );
                Err(e)
            }
        };

        drop(close_stream(peer_id, libp2p_stream).await);

        final_result
    }
}

async fn send_message_with_timeout<M: BoundedSerializeOp + DeserializeOwned + Sync>(
    peer_id: PeerId,
    stream: &mut Libp2pStream,
    message: &M,
    timeout: Duration,
) -> Result<(), ChainSyncError> {
    time::timeout(timeout, send_message(peer_id, stream, message))
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))?
}
