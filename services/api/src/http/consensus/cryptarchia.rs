use std::fmt::{Debug, Display};

use logos_blockchain_chain_service::{ConsensusMsg, CryptarchiaConsensus, CryptarchiaInfo};
use logos_blockchain_core::{header::HeaderId, mantle::SignedMantleTx};
use logos_blockchain_storage_service::backends::rocksdb::RocksBackend;
use overwatch::{overwatch::handle::OverwatchHandle, services::AsServiceId};
use tokio::sync::oneshot;

use crate::http::DynError;

pub type Cryptarchia<RuntimeServiceId> =
    CryptarchiaConsensus<SignedMantleTx, RocksBackend, RuntimeServiceId>;

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

pub async fn cryptarchia_headers<RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    from: Option<HeaderId>,
    to: Option<HeaderId>,
) -> Result<Vec<HeaderId>, DynError>
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    relay
        .send(ConsensusMsg::GetHeaders {
            from,
            to,
            tx: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    Ok(receiver.await?)
}
