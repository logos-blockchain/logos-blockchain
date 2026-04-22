use std::fmt::{Debug, Display};

use axum::{Json, extract::State, response::Response};
use lb_api_service::http::{libp2p, mantle};
use lb_libp2p::{Multiaddr, PeerId};
use lb_network_service::{NetworkService, backends::libp2p::Libp2p as NetworkBackend};
use overwatch::{overwatch::OverwatchHandle, services::AsServiceId};
use serde::{Deserialize, Serialize};

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
        Ok::<PeerId, overwatch::DynError>(peer_id)
    })
}
