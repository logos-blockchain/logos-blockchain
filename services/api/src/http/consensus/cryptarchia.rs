use std::fmt::{Debug, Display};

use futures::{StreamExt as _, TryStreamExt as _};
use lb_chain_service::{ConsensusMsg, CryptarchiaConsensus, CryptarchiaInfo};
use lb_core::{header::HeaderId, mantle::SignedMantleTx};
use lb_ledger::LedgerState;
use lb_storage_service::backends::rocksdb::RocksBackend;
use lb_time_service::backends::ntp::NtpTimeBackend;
use overwatch::{overwatch::handle::OverwatchHandle, services::AsServiceId};
use tokio::sync::oneshot;

use crate::http::DynError;

pub type Cryptarchia<RuntimeServiceId> =
    CryptarchiaConsensus<SignedMantleTx, RocksBackend, NtpTimeBackend, RuntimeServiceId>;

pub async fn cryptarchia_info<RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<CryptarchiaInfo, DynError>
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    relay
        .send(ConsensusMsg::Info { tx: sender })
        .await
        .map_err(|(e, _)| e)?;

    Ok(receiver.await?)
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
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    relay
        .send(ConsensusMsg::GetHeaders {
            from_descendant,
            to_ancestor,
            tx: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    let stream = receiver.await?;
    Ok(stream.take(HEADERS_LIMIT).try_collect().await?)
}

pub async fn cryptarchia_ledger_state<RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<LedgerState, DynError>
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    let info = cryptarchia_info(handle).await?;

    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    relay
        .send(ConsensusMsg::GetLedgerState {
            block_id: info.tip,
            tx: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await?
        .ok_or_else(|| "ledger state for tip must exist".into())
}
