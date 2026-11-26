use lb_api_common::bodies::pow::{PoWClaimRequestBody, PoWClaimResponseBody};
use lb_pow_service::{
    AutoClaimSettings, ClaimableRewardsInfo, PoWMiningSettings, PoWStatus,
    api::{ApiError as PoWApiError, PoWServiceApi},
};

use super::{Error, Handle};
use crate::{PoWService, RuntimeServiceId};

impl From<PoWApiError> for Error {
    fn from(error: PoWApiError) -> Self {
        match error {
            PoWApiError::CommsFailure(_) => Self::Unavailable(Box::new(error)),
            PoWApiError::SettingsRejected(_) => Self::InvalidInput(error.to_string()),
            PoWApiError::ClaimFailed(_) => Self::Internal(Box::new(error)),
        }
    }
}

async fn pow_api(handle: &Handle) -> Result<PoWServiceApi<PoWService, RuntimeServiceId>, Error> {
    Ok(PoWServiceApi::new(handle.relay::<PoWService>().await?))
}

pub async fn start_mining(handle: &Handle) -> Result<(), Error> {
    Ok(pow_api(handle).await?.start_mining().await?)
}

pub async fn stop_mining(handle: &Handle) -> Result<(), Error> {
    Ok(pow_api(handle).await?.stop_mining().await?)
}

pub async fn start_auto_claim(handle: &Handle) -> Result<(), Error> {
    Ok(pow_api(handle).await?.start_auto_claim().await?)
}

pub async fn stop_auto_claim(handle: &Handle) -> Result<(), Error> {
    Ok(pow_api(handle).await?.stop_auto_claim().await?)
}

pub async fn claim(
    handle: &Handle,
    request: PoWClaimRequestBody,
) -> Result<PoWClaimResponseBody, Error> {
    let tx_hash = pow_api(handle).await?.claim(request.claim_address).await?;
    Ok(PoWClaimResponseBody { tx_hash })
}

pub async fn claimable_rewards(handle: &Handle) -> Result<ClaimableRewardsInfo, Error> {
    Ok(pow_api(handle).await?.claimable_rewards().await?)
}

pub async fn status(handle: &Handle) -> Result<PoWStatus, Error> {
    Ok(pow_api(handle).await?.status().await?)
}

pub async fn set_mining_settings(
    handle: &Handle,
    settings: PoWMiningSettings,
) -> Result<(), Error> {
    Ok(pow_api(handle).await?.set_mining_settings(settings).await?)
}

pub async fn set_auto_claim_settings(
    handle: &Handle,
    settings: AutoClaimSettings,
) -> Result<(), Error> {
    Ok(pow_api(handle)
        .await?
        .set_auto_claim_settings(settings)
        .await?)
}
