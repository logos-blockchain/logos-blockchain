use axum::{
    Extension,
    extract::{Query, State},
    response::Response,
};
use lb_api_common::{bodies::chain::ChainForkResponseBody, paths};
use lb_chain_service::ChainServiceInfo;
use lb_core::{era::ForkDigest, header::HeaderId};
use serde::Deserialize;

use crate::api::{
    calls::{self, Handle, cryptarchia, time},
    errors::ErrorBody,
};

#[utoipa::path(
    get,
    path = paths::CRYPTARCHIA_INFO,
    responses(
        (status = 200, description = "Query consensus information", body = ChainServiceInfo),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn cryptarchia_info(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(cryptarchia::info(&handle))
}

/// The era in force comes from the time service's clock, and its fork digest
/// from the deployment, handed to the API backend like the chain ID.
#[utoipa::path(
    get,
    path = paths::CHAIN_FORK,
    responses(
        (status = 200, description = "The fork digest a transaction signed now carries", body = ChainForkResponseBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn chain_fork(
    State(handle): State<Handle>,
    Extension(fork_digests): Extension<Vec<ForkDigest>>,
) -> Response {
    make_request_and_return_response!(async {
        let fork_digest = time::fork_digest_in_force(&handle, &fork_digests).await?;
        Ok::<_, calls::Error>(ChainForkResponseBody { fork_digest })
    })
}

#[derive(Deserialize)]
pub struct GasPricesQuery {
    tip: Option<HeaderId>,
}

#[utoipa::path(
    get,
    path = paths::MANTLE_GAS_PRICES,
    responses(
        (status = 200, description = "Get the gas prices from the ledger state at the tip"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn get_gas_prices(
    State(handle): State<Handle>,
    Query(query): Query<GasPricesQuery>,
) -> Response {
    make_request_and_return_response!(cryptarchia::gas_prices(&handle, query.tip))
}

#[utoipa::path(
    get,
    path = paths::MANTLE_LIMITS,
    responses(
        (status = 200, description = "Get what a transaction must fit in the era of the current slot"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn get_limits(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(cryptarchia::limits(&handle))
}
