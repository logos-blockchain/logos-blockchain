use std::fmt::{Debug, Display};

use axum::{Json, extract::State, response::Response};
use logos_blockchain_api::http::da::{self, DaDispersal};
use logos_blockchain_da_dispersal::{adapters::network::DispersalNetworkAdapter, backend::DispersalBackend};
use logos_blockchain_da_network_core::SubnetworkId;
use logos_blockchain_http_api_common::{bodies::dispersal::DispersalRequestBody, paths};
use logos_blockchain_libp2p::PeerId;
use logos_blockchain_node::make_request_and_return_response;
use overwatch::{overwatch::handle::OverwatchHandle, services::AsServiceId};
use serde::Serialize;
use logos_blockchain_subnetworks_assignations::MembershipHandler;

#[utoipa::path(
    post,
    path = paths::DISPERSE_DATA,
    responses(
        (status = 200, description = "Disperse data in DA network"),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn disperse_data<Backend, NetworkAdapter, Membership, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(dispersal_req): Json<DispersalRequestBody>,
) -> Response
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    Backend: DispersalBackend<NetworkAdapter = NetworkAdapter> + Send + Sync + 'static,
    Backend::Settings: Clone + Send + Sync,
    Backend::BlobId: Serialize,
    NetworkAdapter: DispersalNetworkAdapter<SubnetworkId = Membership::NetworkId> + Send,
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + AsServiceId<DaDispersal<Backend, NetworkAdapter, Membership, RuntimeServiceId>>
        + 'static,
{
    make_request_and_return_response!(da::disperse_data::<
        Backend,
        NetworkAdapter,
        Membership,
        RuntimeServiceId,
    >(
        &handle,
        dispersal_req.channel_id,
        dispersal_req.parent_msg_id,
        dispersal_req.signer,
        dispersal_req.data
    ))
}
