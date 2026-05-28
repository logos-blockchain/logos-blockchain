use std::fmt::{Debug, Display};

use axum::{Json, extract::State, response::Response};
use futures::StreamExt as _;
use lb_api_service::http::{consensus, libp2p, mantle};
use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction as _, TxHash},
};
use lb_libp2p::{Multiaddr, PeerId};
use lb_network_service::{NetworkService, backends::libp2p::Libp2p as NetworkBackend};
use lb_tx_service::MempoolMsg;
use overwatch::{DynError, overwatch::OverwatchHandle, services::AsServiceId};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::backend::TestHttpCryptarchiaService;
use crate::{
    generic_services::{SdpService, TxMempoolService},
    make_request_and_return_response,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialPeerRequestBody {
    pub addr: Multiaddr,
}

pub async fn get_sdp_declarations<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<TestHttpCryptarchiaService<RuntimeServiceId>>
        + AsServiceId<SdpService<RuntimeServiceId>>
        + AsServiceId<TxMempoolService<RuntimeServiceId>>,
{
    make_request_and_return_response!(mantle::get_sdp_declarations::<RuntimeServiceId>(&handle))
}

pub async fn test_mempool_view<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<TestHttpCryptarchiaService<RuntimeServiceId>>
        + AsServiceId<SdpService<RuntimeServiceId>>
        + AsServiceId<TxMempoolService<RuntimeServiceId>>,
{
    make_request_and_return_response!(current_tip_mempool_view::<RuntimeServiceId>(&handle))
}

async fn current_tip_mempool_view<RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<Vec<TxHash>, DynError>
where
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<TestHttpCryptarchiaService<RuntimeServiceId>>
        + AsServiceId<SdpService<RuntimeServiceId>>
        + AsServiceId<TxMempoolService<RuntimeServiceId>>,
{
    let consensus = consensus::cryptarchia_info::<RuntimeServiceId>(handle).await?;

    mempool_view_at::<RuntimeServiceId>(handle, consensus.cryptarchia_info.tip).await
}

async fn mempool_view_at<RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    ancestor_hint: HeaderId,
) -> Result<Vec<TxHash>, DynError>
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<TxMempoolService<RuntimeServiceId>>,
{
    let relay = handle.relay::<TxMempoolService<RuntimeServiceId>>().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(MempoolMsg::View {
            ancestor_hint,
            reply_channel: sender,
        })
        .await
        .map_err(|(error, _)| error)?;

    let txs = receiver.await?;

    Ok(txs.map(|tx: SignedMantleTx| tx.hash()).collect().await)
}

pub async fn dial_peer<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(req): Json<DialPeerRequestBody>,
) -> Response
where
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<NetworkService<NetworkBackend, RuntimeServiceId>>
        + AsServiceId<TestHttpCryptarchiaService<RuntimeServiceId>>
        + AsServiceId<SdpService<RuntimeServiceId>>
        + AsServiceId<TxMempoolService<RuntimeServiceId>>,
{
    make_request_and_return_response!(async move {
        let peer_id: PeerId = libp2p::connect_peer::<RuntimeServiceId>(&handle, req.addr).await?;
        Ok::<PeerId, DynError>(peer_id)
    })
}
