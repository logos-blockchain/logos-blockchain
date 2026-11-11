use axum::{Json, extract::State, response::Response};
use lb_api_common::paths;
use lb_libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::{
    calls::{Handle, network},
    errors::ErrorBody,
};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DialPeerRequestBody {
    /// Multiaddress of the peer to dial.
    #[schema(value_type = String)]
    pub addr: Multiaddr,
}

#[utoipa::path(
    get,
    path = paths::NETWORK_INFO,
    responses(
        (status = 200, description = "Query the network information", body = lb_network_service::backends::libp2p::Libp2pInfo),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn libp2p_info(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(network::info(&handle))
}

#[utoipa::path(
    post,
    path = paths::DIAL_PEER,
    request_body = DialPeerRequestBody,
    responses(
        (status = 200, description = "Dial a network peer", body = String),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn dial_peer(
    State(handle): State<Handle>,
    Json(req): Json<DialPeerRequestBody>,
) -> Response {
    make_request_and_return_response!(network::connect_peer(&handle, req.addr))
}
