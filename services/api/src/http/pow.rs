use std::fmt::{Debug, Display};

use lb_core::mantle::transactions::hash::TxHash;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_pow_service::{
    ClaimableRewardsInfo,
    api::{PoWServiceApi, PoWServiceData},
};
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

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

pub async fn start_auto_claim<PoW, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<(), DynError>
where
    PoW: PoWServiceData,
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<PoW>,
{
    PoWServiceApi::<PoW, RuntimeServiceId>::new(handle.relay().await?)
        .start_auto_claim()
        .await?;
    Ok(())
}

pub async fn stop_auto_claim<PoW, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<(), DynError>
where
    PoW: PoWServiceData,
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<PoW>,
{
    PoWServiceApi::<PoW, RuntimeServiceId>::new(handle.relay().await?)
        .stop_auto_claim()
        .await?;
    Ok(())
}

/// Claims the mined rewards, paying them to `claim_address`.
///
/// `None` defers to the node's auto-claim configuration and pays whichever
/// target is currently furthest below its threshold; it fails when no target
/// is configured or all of them are already satisfied.
pub async fn claim<PoW, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    claim_address: Option<ZkPublicKey>,
) -> Result<PoWClaimResponseBody, DynError>
where
    PoW: PoWServiceData,
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<PoW>,
{
    let tx_hash = PoWServiceApi::<PoW, RuntimeServiceId>::new(handle.relay().await?)
        .claim(claim_address)
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

/// Where a claim request pays the rewards.
///
/// The whole body is optional, and so is the key inside it: both omitted mean
/// "use the node's auto-claim target".
#[derive(Serialize, Deserialize, Default, ToSchema)]
pub struct PoWClaimRequestBody {
    #[serde(default)]
    pub claim_address: Option<ZkPublicKey>,
}

/// The id of the reward-claim transaction submitted by a claim request, or
/// `null` when there were no rewards to claim.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct PoWClaimResponseBody {
    pub tx_hash: Option<TxHash>,
}
