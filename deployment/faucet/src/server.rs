use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    response::IntoResponse,
    routing::post,
};
use lb_groth16::fr_from_bytes;
use lb_key_management_system_keys::keys::ZkPublicKey;
use reqwest::StatusCode;

use crate::faucet::Faucet;

async fn transfer_funds(
    State(faucet): State<Arc<Faucet>>,
    Path(key_id): Path<String>,
) -> impl IntoResponse {
    let bytes = match hex::decode(&key_id) {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid hex: {e}")).into_response(),
    };

    let recipient_pk = match fr_from_bytes(&bytes) {
        Ok(fr) => ZkPublicKey::new(fr),
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid key format: {e:?}"),
            )
                .into_response();
        }
    };

    match faucet.transfer_to_pk(recipient_pk).await {
        Ok(tx) => (StatusCode::OK, tx).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub fn faucet_app(faucet: Arc<Faucet>) -> Router {
    Router::new()
        .route("/faucet/:pk", post(transfer_funds))
        .with_state(faucet)
}
