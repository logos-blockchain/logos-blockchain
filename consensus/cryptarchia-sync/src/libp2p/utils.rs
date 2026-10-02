use futures::AsyncWriteExt as _;
use lb_binary_codec::bincode::BoundedSerializeOp;
use libp2p::{PeerId, Stream, StreamProtocol};
use libp2p_stream::{Control, OpenStreamError};
use serde::de::DeserializeOwned;

use crate::libp2p::{errors::ChainSyncError, packing::pack_to_writer};

pub async fn send_message<M: BoundedSerializeOp + DeserializeOwned + Sync>(
    peer_id: PeerId,
    stream: &mut Stream,
    message: &M,
) -> Result<(), ChainSyncError> {
    pack_to_writer(message, stream)
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))?;

    stream
        .flush()
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))?;

    Ok(())
}

/// Opens a stream to `peer_id` with the first of `protocols` it speaks, the
/// preferred first.
///
/// # Panics
///
/// If `protocols` is empty.
pub async fn open_stream(
    peer_id: PeerId,
    control: &mut Control,
    protocols: &[StreamProtocol],
) -> Result<Stream, ChainSyncError> {
    let (last, preferred) = protocols
        .split_last()
        .expect("chain sync speaks at least one protocol");
    for protocol in preferred {
        match control.open_stream(peer_id, protocol.clone()).await {
            Err(OpenStreamError::UnsupportedProtocol(_)) => {}
            result => return result.map_err(|e| ChainSyncError::from((peer_id, e))),
        }
    }
    control
        .open_stream(peer_id, last.clone())
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))
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
