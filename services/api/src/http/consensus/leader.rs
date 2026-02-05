use std::fmt::{Debug, Display};

use lb_chain_leader_service::api::{ChainLeaderSerivceApi, ChainLeaderServiceData};
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};

use crate::http::DynError;

pub async fn claim<ChainLeader, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<(), DynError>
where
    ChainLeader: ChainLeaderServiceData,
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<ChainLeader>,
{
    Ok(
        ChainLeaderSerivceApi::<ChainLeader, RuntimeServiceId>::new(handle.relay().await?)
            .claim()
            .await?,
    )
}
