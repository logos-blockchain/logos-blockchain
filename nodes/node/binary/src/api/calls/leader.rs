use lb_chain_leader_service::api::{ApiError as LeaderApiError, ChainLeaderSerivceApi};
use lb_http_api_common::bodies::leader::LeaderClaimResponseBody;

use super::{Error, Handle};
use crate::{CryptarchiaLeaderService, RuntimeServiceId};

impl From<LeaderApiError> for Error {
    fn from(error: LeaderApiError) -> Self {
        match error {
            LeaderApiError::CommsFailure(_) => Self::Unavailable(Box::new(error)),
            LeaderApiError::ChainLeaderServiceError(_) => Self::Internal(Box::new(error)),
        }
    }
}

pub async fn claim(handle: &Handle) -> Result<LeaderClaimResponseBody, Error> {
    let tx_hash = ChainLeaderSerivceApi::<CryptarchiaLeaderService, RuntimeServiceId>::new(
        handle.relay::<CryptarchiaLeaderService>().await?,
    )
    .claim()
    .await?;
    Ok(LeaderClaimResponseBody { tx_hash })
}
