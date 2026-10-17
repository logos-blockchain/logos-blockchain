use futures::{TryStreamExt as _, stream::BoxStream};
use lb_log_targets::cryptarchia;
use libp2p::{PeerId, Stream as Libp2pStream};
use tokio::sync::mpsc;
use tracing::error;

use crate::{
    BlocksProviderResponse, BlocksUnavailableReason, DynError, ProviderResponse,
    TipProviderResponse,
    libp2p::{
        errors::{ChainSyncError, ChainSyncErrorKind},
        messages::{BlocksResponse, RequestMessage},
        packing::unpack_from_reader,
        utils::{close_stream, send_message},
    },
    messages::{SerialisedBlock, TipResponse},
};

const LOG_TARGET: &str = cryptarchia::sync::libp2p::PROVIDER;

pub const MAX_ADDITIONAL_BLOCKS: usize = 5;

pub struct Provider;

/// A request read from a stream a peer opened, and the stream to answer it on.
pub struct ReceivedRequest {
    pub peer_id: PeerId,
    pub stream: Libp2pStream,
    pub request: RequestMessage,
}

impl Provider {
    pub async fn receive_request(
        peer_id: PeerId,
        mut stream: Libp2pStream,
    ) -> Result<ReceivedRequest, ChainSyncError> {
        let request: RequestMessage = unpack_from_reader(&mut stream)
            .await
            .map_err(|e| ChainSyncError::from((peer_id, e)))?;

        Ok(ReceivedRequest {
            peer_id,
            stream,
            request,
        })
    }

    pub async fn provide_tip(
        mut reply_receiver: mpsc::Receiver<TipProviderResponse>,
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
                let response = TipResponse::Failure(reason);
                send_message(peer_id, &mut libp2p_stream, &response).await
            }
        };

        drop(close_stream(peer_id, libp2p_stream).await);

        send_result
    }

    pub async fn provide_blocks(
        mut reply_receiver: mpsc::Receiver<BlocksProviderResponse>,
        peer_id: PeerId,
        mut libp2p_stream: Libp2pStream,
    ) -> Result<(), ChainSyncError> {
        let response = reply_receiver.recv().await.ok_or_else(|| ChainSyncError {
            peer: peer_id,
            kind: ChainSyncErrorKind::ChannelReceiveError(
                "Failed to receive blocks stream from channel".to_owned(),
            ),
        })?;

        match response {
            ProviderResponse::Available(stream) => {
                Self::send_blocks(peer_id, stream, libp2p_stream).await
            }
            ProviderResponse::Unavailable { reason } => {
                let response = BlocksResponse::Failure(reason);
                send_message(peer_id, &mut libp2p_stream, &response).await?;
                drop(close_stream(peer_id, libp2p_stream).await);
                Ok(())
            }
        }
    }

    async fn send_blocks(
        peer_id: PeerId,
        stream: BoxStream<'static, Result<SerialisedBlock, DynError>>,
        mut libp2p_stream: Libp2pStream,
    ) -> Result<(), ChainSyncError> {
        let result = stream
            .map_err(|e| ChainSyncError {
                peer: peer_id,
                kind: ChainSyncErrorKind::BlockProviderError(format!(
                    "Failed to receive block from stream: {e}"
                )),
            })
            .try_fold(&mut libp2p_stream, async |stream, block| {
                let message = BlocksResponse::Block(block);
                send_message(peer_id, stream, &message).await?;
                Ok(stream)
            })
            .await;

        let final_result = match result {
            Ok(_) => {
                let message = BlocksResponse::NoMoreBlocks;
                send_message(peer_id, &mut libp2p_stream, &message).await
            }
            Err(e) => {
                error!(target: LOG_TARGET, "Failed to send blocks to peer {}: {}", peer_id, e);
                let message = BlocksResponse::Failure(BlocksUnavailableReason::Unknown);
                drop(send_message(peer_id, &mut libp2p_stream, &message).await);
                Err(e)
            }
        };

        drop(close_stream(peer_id, libp2p_stream).await);

        final_result
    }
}
