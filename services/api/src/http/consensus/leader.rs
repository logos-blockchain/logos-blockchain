use std::fmt::{Debug, Display};

use lb_chain_leader_service::api::{ChainLeaderSerivceApi, ChainLeaderServiceData};
use lb_core::mantle::TxHash;
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use serde::{Deserialize, Serialize};

use crate::http::DynError;

pub async fn claim<ChainLeader, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<LeaderClaimResponseBody, DynError>
where
    ChainLeader: ChainLeaderServiceData,
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<ChainLeader>,
{
    let tx_hash =
        ChainLeaderSerivceApi::<ChainLeader, RuntimeServiceId>::new(handle.relay().await?)
            .claim()
            .await?;
    Ok(LeaderClaimResponseBody { tx_hash })
}

#[derive(Serialize, Deserialize)]
pub struct LeaderClaimResponseBody {
    pub tx_hash: TxHash,
}
