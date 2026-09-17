use std::fmt::{Debug, Display};

use futures::{StreamExt as _, TryStreamExt as _};
use lb_chain_service::{ChainServiceInfo, CryptarchiaConsensus, api::CryptarchiaServiceApi};
use lb_core::{
    header::HeaderId,
    mantle::{
        SignedOps, ledger::verification_mode::StandardMode, transactions::states::Preverified,
    },
};
use lb_ledger::LedgerState;
use lb_time_service::backends::ntp::NtpTimeBackend;
use overwatch::{overwatch::handle::OverwatchHandle, services::AsServiceId};

use crate::http::DynError;

pub type Cryptarchia<RuntimeServiceId> =
    CryptarchiaConsensus<SignedOps<Preverified, StandardMode>, NtpTimeBackend, RuntimeServiceId>;

pub async fn cryptarchia_info<RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<ChainServiceInfo, DynError>
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    let chain_api =
        CryptarchiaServiceApi::<Cryptarchia<RuntimeServiceId>>::from_overwatch_handle(handle).await;
    Ok(chain_api.info().await?)
}

const HEADERS_LIMIT: usize = 512;

pub async fn cryptarchia_headers<RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    from_descendant: Option<HeaderId>,
    to_ancestor: Option<HeaderId>,
) -> Result<Vec<HeaderId>, DynError>
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    let chain_api =
        CryptarchiaServiceApi::<Cryptarchia<RuntimeServiceId>>::from_overwatch_handle(handle).await;
    let stream = chain_api.get_headers(from_descendant, to_ancestor).await?;
    Ok(stream.take(HEADERS_LIMIT).try_collect().await?)
}

pub async fn cryptarchia_ledger_state<RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<LedgerState, DynError>
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    let chain_api =
        CryptarchiaServiceApi::<Cryptarchia<RuntimeServiceId>>::from_overwatch_handle(handle).await;
    let ChainServiceInfo {
        cryptarchia_info, ..
    } = chain_api.info().await?;

    chain_api
        .get_ledger_state(cryptarchia_info.tip)
        .await?
        .ok_or_else(|| "ledger state for tip must exist".into())
}
