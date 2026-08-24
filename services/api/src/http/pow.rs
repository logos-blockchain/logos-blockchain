use std::fmt::{Debug, Display};

use lb_core::mantle::transactions::hash::TxHash;
use lb_pow_service::{
    ClaimableRewardsInfo,
    api::{PoWServiceApi, PoWServiceData},
};
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use serde::{Deserialize, Serialize};

use crate::http::DynError;

pub async fn start_mining<PoW, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<(), DynError>
where
    PoW: PoWServiceData,
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<PoW>,
{
    PoWServiceApi::<PoW, RuntimeServiceId>::new(handle.relay().await?)
        .start_mining()
        .await?;
    Ok(())
}

pub async fn stop_mining<PoW, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<(), DynError>
where
    PoW: PoWServiceData,
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<PoW>,
{
    PoWServiceApi::<PoW, RuntimeServiceId>::new(handle.relay().await?)
        .stop_mining()
        .await?;
    Ok(())
}

pub async fn claim<PoW, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<PoWClaimResponseBody, DynError>
where
    PoW: PoWServiceData,
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<PoW>,
{
    let tx_hash = PoWServiceApi::<PoW, RuntimeServiceId>::new(handle.relay().await?)
        .claim()
        .await?;
    Ok(PoWClaimResponseBody { tx_hash })
}

pub async fn claimable_rewards<PoW, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<ClaimableRewardsInfo, DynError>
where
    PoW: PoWServiceData,
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<PoW>,
{
    Ok(
        PoWServiceApi::<PoW, RuntimeServiceId>::new(handle.relay().await?)
            .claimable_rewards()
            .await?,
    )
}

/// The id of the reward-claim transaction submitted by a claim request, or
/// `null` when there were no rewards to claim.
#[derive(Serialize, Deserialize)]
pub struct PoWClaimResponseBody {
    pub tx_hash: Option<TxHash>,
}
