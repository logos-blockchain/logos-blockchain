use std::marker::PhantomData;

use lb_core::mantle::transactions::hash::TxHash;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use tokio::sync::oneshot;

use crate::service::{ClaimableRewardsInfo, PoWServiceMessage};

/// Marker trait for the `PoW` service, used to parametrize [`PoWServiceApi`]
/// over the concrete service type while pinning its message type.
pub trait PoWServiceData: ServiceData<Message = PoWServiceMessage> + Send + 'static {}

impl<T> PoWServiceData for T where T: ServiceData<Message = PoWServiceMessage> + Send + 'static {}

/// Typed wrapper over the `PoW` service relay, exposing its operations as async
/// methods instead of raw [`PoWServiceMessage`]s.
pub struct PoWServiceApi<PoWService, RuntimeServiceId>
where
    PoWService: PoWServiceData,
{
    relay: OutboundRelay<PoWService::Message>,
    _id: PhantomData<RuntimeServiceId>,
}

impl<PoWService, RuntimeServiceId> Clone for PoWServiceApi<PoWService, RuntimeServiceId>
where
    PoWService: PoWServiceData,
{
    fn clone(&self) -> Self {
        Self {
            relay: self.relay.clone(),
            _id: PhantomData,
        }
    }
}

impl<PoWService, RuntimeServiceId> PoWServiceApi<PoWService, RuntimeServiceId>
where
    PoWService: PoWServiceData,
    RuntimeServiceId: Sync,
{
    #[must_use]
    pub const fn new(relay: OutboundRelay<PoWService::Message>) -> Self {
        Self {
            relay,
            _id: PhantomData,
        }
    }

    /// Enable mining. Fire-and-forget: mining is a boolean toggle that carries
    /// no response. Note it is not persisted, so a restart clears it.
    pub async fn start_mining(&self) -> Result<(), ApiError> {
        self.relay
            .send(PoWServiceMessage::StartMining)
            .await
            .map_err(|(relay_err, _)| {
                ApiError::CommsFailure(format!("{relay_err} while sending StartMining"))
            })
    }

    /// Disable mining. Fire-and-forget.
    pub async fn stop_mining(&self) -> Result<(), ApiError> {
        self.relay
            .send(PoWServiceMessage::StopMining)
            .await
            .map_err(|(relay_err, _)| {
                ApiError::CommsFailure(format!("{relay_err} while sending StopMining"))
            })
    }

    /// Build and publish a reward-claim transaction for the currently
    /// claimable tickets, returning the id of the submitted transaction (or
    /// `None` when there is nothing to claim).
    pub async fn claim(&self) -> Result<Option<TxHash>, ApiError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.relay
            .send(PoWServiceMessage::Claim { response: resp_tx })
            .await
            .map_err(|(relay_err, _)| {
                ApiError::CommsFailure(format!("{relay_err} while sending Claim"))
            })?;

        resp_rx
            .await
            .map_err(|relay_err| {
                ApiError::CommsFailure(format!("{relay_err} while receiving Claim response"))
            })?
            .map_err(ApiError::ClaimFailed)
    }

    /// Report the rewards this node can currently claim.
    pub async fn claimable_rewards(&self) -> Result<ClaimableRewardsInfo, ApiError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.relay
            .send(PoWServiceMessage::ClaimableRewardsInfo { response: resp_tx })
            .await
            .map_err(|(relay_err, _)| {
                ApiError::CommsFailure(format!("{relay_err} while sending ClaimableRewardsInfo"))
            })?;

        resp_rx.await.map_err(|relay_err| {
            ApiError::CommsFailure(format!(
                "{relay_err} while receiving ClaimableRewardsInfo response"
            ))
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Failed to establish connection to pow-service: {0}")]
    CommsFailure(String),
    #[error("Failed to claim PoW rewards: {0}")]
    ClaimFailed(overwatch::DynError),
}
