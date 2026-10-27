use futures::AsyncWriteExt as _;
use lb_binary_codec::bincode::BoundedSerializeOp;
use libp2p::{PeerId, Stream, StreamProtocol};
use libp2p_stream::Control;
use serde::{Serialize, de::DeserializeOwned};

use crate::libp2p::{
    errors::ChainSyncError,
    packing::{pack_to_writer, pack_to_writer_up_to},
};

pub async fn send_message<M: BoundedSerializeOp + DeserializeOwned + Sync>(
    peer_id: PeerId,
    stream: &mut Stream,
    message: &M,
) -> Result<(), ChainSyncError> {
    pack_to_writer(message, stream)
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))?;

    flush(peer_id, stream).await
}

/// Sends a message of at most `max` bytes, a maximum known only at runtime.
pub async fn send_message_up_to<M: Serialize + Sync>(
    peer_id: PeerId,
    stream: &mut Stream,
    message: &M,
    max_bytes: usize,
) -> Result<(), ChainSyncError> {
    pack_to_writer_up_to(message, max_bytes, stream)
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))?;

    flush(peer_id, stream).await
}

async fn flush(peer_id: PeerId, stream: &mut Stream) -> Result<(), ChainSyncError> {
    stream
        .flush()
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))
}

pub async fn open_stream(
    peer_id: PeerId,
    control: &mut Control,
    protocol_name: StreamProtocol,
) -> Result<Stream, ChainSyncError> {
    let stream = control
        .open_stream(peer_id, protocol_name)
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))?;
    Ok(stream)
}

pub async fn close_stream(peer_id: PeerId, mut stream: Stream) -> Result<(), ChainSyncError> {
    stream
        .flush()
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))?;

    stream
        .close()
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))?;
    Ok(())
}
