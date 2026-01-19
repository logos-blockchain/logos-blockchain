use futures::AsyncWriteExt as _;
use libp2p::{PeerId, Stream, StreamProtocol};
use libp2p_stream::Control;
use serde::{Serialize, de::DeserializeOwned};

use crate::libp2p::{errors::ChainSyncError, packing::pack_to_writer};

pub async fn send_message<M: Serialize + DeserializeOwned + Sync>(
    peer_id: PeerId,
    mut stream: &mut Stream,
    message: &M,
) -> Result<(), ChainSyncError> {
    pack_to_writer(message, &mut stream)
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))?;

    stream
        .flush()
        .await
        .map_err(|e| ChainSyncError::from((peer_id, e)))?;

    Ok(())
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
