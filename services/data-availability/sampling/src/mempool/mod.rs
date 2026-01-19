pub mod sampling;

use std::pin::Pin;

use futures::Stream;
use lb_core::{da::BlobId, sdp::SessionNumber};
use lb_tx_service::backend::MempoolError;
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};

#[derive(thiserror::Error, Debug)]
pub enum MempoolAdapterError {
    #[error("Mempool responded with and error: {0}")]
    Mempool(#[from] MempoolError),
    #[error("Channel receive error: {0}")]
    ChannelRecv(#[from] tokio::sync::oneshot::error::RecvError),
    #[error("Other mempool adapter error: {0}")]
    Other(DynError),
}

pub struct Blob {
    pub blob_id: BlobId,
    pub session: SessionNumber,
}

#[async_trait::async_trait]
pub trait DaMempoolAdapter {
    type MempoolService: ServiceData;
    type Tx;

    fn new(outbound_relay: OutboundRelay<<Self::MempoolService as ServiceData>::Message>) -> Self;

    async fn subscribe(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Blob> + Send>>, MempoolAdapterError>;
}
