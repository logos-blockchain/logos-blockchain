use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse as _, Response},
};
use lb_api_common::{
    bodies::wallet::{
        fund::WalletFundRequestBody,
        sign::{WalletSignTxEd25519RequestBody, WalletSignTxZkRequestBody},
        transfer_funds::WalletTransferFundsRequestBody,
    },
    paths,
};
use lb_core::header::HeaderId;
use lb_key_management_system_service::keys::ZkPublicKey;
use serde::Deserialize;

use crate::api::{
    calls::{Handle, wallet},
    errors::ErrorBody,
};

#[derive(Deserialize)]
pub struct TipQuery {
    tip: Option<HeaderId>,
}

#[utoipa::path(
get,
path = paths::wallet::BALANCE,
responses(
    (status = 200, description = "Get wallet balance"),
    (status = 500, description = "Internal server error", body = ErrorBody),
)
)]
pub async fn get_balance(
    State(handle): State<Handle>,
    Path(address): Path<ZkPublicKey>,
    Query(query): Query<TipQuery>,
) -> Response {
    make_request_and_return_response!(wallet::balance(&handle, query.tip, address))
}

#[utoipa::path(
get,
path = paths::LEADER_AGED_NOTES,
responses(
    (status = 200, description = "Get the wallet notes eligible to lead"),
    (status = 500, description = "Internal server error", body = ErrorBody),
)
)]
pub async fn get_leader_aged_notes(
    State(handle): State<Handle>,
    Query(query): Query<TipQuery>,
) -> Response {
    make_request_and_return_response!(wallet::leader_aged_notes(&handle, query.tip))
}

#[utoipa::path(
get,
path = paths::LEADER_CLAIM_VOUCHERS,
responses(
    (status = 200, description = "Get claimable wallet vouchers"),
    (status = 500, description = "Internal server error", body = ErrorBody),
)
)]
pub async fn get_claimable_vouchers(
    State(handle): State<Handle>,
    Query(query): Query<TipQuery>,
) -> Response {
    make_request_and_return_response!(wallet::claimable_vouchers(&handle, query.tip))
}

#[utoipa::path(
post,
path = paths::wallet::TRANSACTIONS_TRANSFER_FUNDS,
responses(
    (status = 200, description = "Make transfer"),
    (status = 500, description = "Internal server error", body = ErrorBody),
)
)]
pub async fn post_transactions_transfer_funds(
    State(handle): State<Handle>,
    Json(body): Json<WalletTransferFundsRequestBody>,
) -> Response {
    match wallet::transfer_funds(&handle, body).await {
        Ok(response) => (StatusCode::CREATED, Json(response)).into_response(),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    post,
    path = paths::wallet::SIGN_TX_ED25519,
    responses(
        (status = 200, description = "Signed transaction"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn sign_tx_ed25519(
    State(handle): State<Handle>,
    Json(req): Json<WalletSignTxEd25519RequestBody>,
) -> Response {
    make_request_and_return_response!(wallet::sign_tx_ed25519(&handle, req))
}

#[utoipa::path(
    post,
    path = paths::wallet::SIGN_TX_ZK,
    responses(
        (status = 200, description = "Signed transaction"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn sign_tx_zk(
    State(handle): State<Handle>,
    Json(req): Json<WalletSignTxZkRequestBody>,
) -> Response {
    make_request_and_return_response!(wallet::sign_tx_zk(&handle, req))
}

#[utoipa::path(
    post,
    path = paths::wallet::FUND,
    responses(
        (status = 200, description = "Funded transaction with fee transfer proof"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn fund(
    State(handle): State<Handle>,
    Json(req): Json<WalletFundRequestBody>,
) -> Response {
    make_request_and_return_response!(wallet::fund(&handle, req))
}
