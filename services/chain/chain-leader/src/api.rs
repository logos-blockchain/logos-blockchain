use std::marker::PhantomData;

use overwatch::services::{ServiceData, relay::OutboundRelay};
use tokio::sync::oneshot;

use crate::LeaderMsg;

pub struct ChainLeaderSerivceApi<ChainLeaderService, RuntimeServiceId>
where
    ChainLeaderService: ChainLeaderServiceData,
{
    relay: OutboundRelay<ChainLeaderService::Message>,
    _phantom: PhantomData<RuntimeServiceId>,
}

pub trait ChainLeaderServiceData: ServiceData<Message = LeaderMsg> + Send + 'static {}

impl<T> ChainLeaderServiceData for T where T: ServiceData<Message = LeaderMsg> + Send + 'static {}

impl<ChainLeaderService, RuntimeServiceId>
    ChainLeaderSerivceApi<ChainLeaderService, RuntimeServiceId>
where
    ChainLeaderService: ChainLeaderServiceData,
    RuntimeServiceId: Sync,
{
    #[must_use]
    pub const fn new(relay: OutboundRelay<ChainLeaderService::Message>) -> Self {
        Self {
            relay,
            _phantom: PhantomData,
        }
    }

    pub async fn claim(&self) -> Result<(), ApiError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.relay
            .send(LeaderMsg::Claim { sender: resp_tx })
            .await
            .map_err(|(relay_err, _)| {
                ApiError::CommsFailure(format!("{relay_err} while sending Claim"))
            })?;

        resp_rx
            .await
            .map_err(|relay_err| {
                ApiError::CommsFailure(format!("{relay_err} while receiving Claim response"))
            })?
            .map_err(|e| ApiError::ChainLeaderServiceError(Box::new(e)))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Failed to establish connection to chain-leader-service: {0}")]
    CommsFailure(String),
    #[error("Chain leader service error: {0}")]
    ChainLeaderServiceError(#[from] Box<crate::Error>),
}
