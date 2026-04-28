use futures::{TryStreamExt as _, stream::BoxStream};
use libp2p::{PeerId, Stream as Libp2pStream};
use tokio::sync::mpsc;
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

pub const MAX_ADDITIONAL_BLOCKS: usize = 5;

pub struct Provider;

pub type ReceivingRequestStream = (PeerId, Libp2pStream, RequestMessage);

impl Provider {
    pub async fn process_request(
        peer_id: PeerId,
        mut stream: Libp2pStream,
    ) -> Result<ReceivingRequestStream, ChainSyncError> {
        let request: RequestMessage = unpack_from_reader(&mut stream)
            .await
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

    pub async fn provide_blocks(
        mut reply_receiver: mpsc::Receiver<BlocksResponse>,
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
                let response = DownloadBlocksResponse::Failure(reason);
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
                kind: ChainSyncErrorKind::ReceivingBlocksError(format!(
                    "Failed to receive block from stream: {e}"
                )),
            })
            .try_fold(&mut libp2p_stream, async |stream, block| {
                let message = DownloadBlocksResponse::Block(block);
                send_message(peer_id, stream, &message).await?;
                Ok(stream)
            })
            .await;

        let final_result = match result {
            Ok(_) => {
                let message = DownloadBlocksResponse::NoMoreBlocks;
                send_message(peer_id, &mut libp2p_stream, &message).await
            }
            Err(e) => {
                error!("Failed to send blocks to peer {}: {}", peer_id, e);
                let message = DownloadBlocksResponse::Failure(BlocksUnavailableReason::Unknown(
                    e.to_string(),
                ));
                drop(send_message(peer_id, &mut libp2p_stream, &message).await);
                Err(e)
            }
        };

        drop(close_stream(peer_id, libp2p_stream).await);

        final_result
    }
}
