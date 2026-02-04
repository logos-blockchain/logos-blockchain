use std::marker::PhantomData;

use lb_core::block::Block;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use tokio::sync::oneshot;

use crate::Message;

pub struct ChainNetworkServiceApi<ChainNetworkService, RuntimeServiceId>
where
    ChainNetworkService: ChainNetworkServiceData,
{
    relay: OutboundRelay<ChainNetworkService::Message>,
    _phantom: PhantomData<RuntimeServiceId>,
}

impl<ChainNetworkService, RuntimeServiceId>
    ChainNetworkServiceApi<ChainNetworkService, RuntimeServiceId>
where
    ChainNetworkService: ChainNetworkServiceData<Tx: Send + Sync>,
    RuntimeServiceId: Sync,
{
    #[must_use]
    pub const fn new(relay: OutboundRelay<ChainNetworkService::Message>) -> Self {
        Self {
            relay,
            _phantom: PhantomData,
        }
    }

    pub async fn apply_block_and_reconcile_mempool(
        &self,
        block: Block<ChainNetworkService::Tx>,
    ) -> Result<(), ApiError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.relay
            .send(Message::ApplyBlockAndReconcileMempool {
                block,
                resp: resp_tx,
            })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!(
                    "{relay_error} while sending ApplyBlockAndReconcileMempool"
                ))
            })?;

        Ok(resp_rx.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!(
                "{relay_error} while receiving ApplyBlockAndReconcileMempool response"
            ))
        })??)
    }
}

pub trait ChainNetworkServiceData:
    ServiceData<Message = Message<Self::Tx>> + Send + 'static
{
    type Tx;
}

impl<T, Tx> ChainNetworkServiceData for T
where
    T: ServiceData<Message = Message<Tx>> + Send + 'static,
{
    type Tx = Tx;
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Failed to establish connection to chain-network-service: {0}")]
    CommsFailure(String),
    #[error("Chain network service error: {0}")]
    ChainNetworkServiceError(#[from] crate::Error),
}
