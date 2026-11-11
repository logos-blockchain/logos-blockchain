use futures::StreamExt as _;
use lb_core::mantle::{TxHash, traits::Hashable as _};
use lb_tx_service::{MempoolMetrics, MempoolMsg, tx::service::openapi::Status};

use super::{Error, Handle, cryptarchia, request};
use crate::{MempoolService, ValidatedTx};

pub async fn add_tx(handle: &Handle, tx: ValidatedTx) -> Result<(), Error> {
    request::<MempoolService, _>(handle, |reply_channel| MempoolMsg::Add {
        key: tx.hash(),
        payload: tx,
        reply_channel,
    })
    .await?
    .map_err(Error::internal)
}

pub async fn metrics(handle: &Handle) -> Result<MempoolMetrics, Error> {
    request::<MempoolService, _>(handle, |reply_channel| MempoolMsg::Metrics {
        reply_channel,
    })
    .await
}

pub async fn status(handle: &Handle, items: Vec<TxHash>) -> Result<Vec<Status>, Error> {
    request::<MempoolService, _>(handle, |reply_channel| MempoolMsg::Status {
        items,
        reply_channel,
    })
    .await
}

/// The transactions the mempool holds as seen from the node's current tip.
pub async fn view(handle: &Handle) -> Result<Vec<TxHash>, Error> {
    let ancestor_hint = cryptarchia::info(handle).await?.cryptarchia_info.tip;
    let transactions = request::<MempoolService, _>(handle, |reply_channel| MempoolMsg::View {
        ancestor_hint,
        reply_channel,
    })
    .await?;
    Ok(transactions
        .map(|tx: ValidatedTx| tx.hash())
        .collect()
        .await)
}
