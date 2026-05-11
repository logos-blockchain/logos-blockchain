#![allow(clippy::needless_for_each, reason = "Utoipa implementation")]

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    paths(
        crate::api::handlers::mantle_metrics,
        crate::api::handlers::mantle_status,
        crate::api::handlers::cryptarchia_info,
        crate::api::handlers::cryptarchia_headers,
        crate::api::handlers::cryptarchia_lib_stream,
        crate::api::handlers::libp2p_info,
        crate::api::handlers::add_tx,
        crate::api::handlers::channel,
        crate::api::handlers::channel_deposit,
        crate::api::handlers::post_declaration,
        crate::api::handlers::post_activity,
        crate::api::handlers::post_withdrawal,
        crate::api::handlers::leader_claim,
        crate::api::handlers::immutable_blocks,
        crate::api::handlers::block,
        crate::api::handlers::blocks_stream,
        crate::api::handlers::transaction,
        crate::api::handlers::wallet::get_balance,
        crate::api::handlers::wallet::post_transactions_transfer_funds,
    ),
    components(schemas(schema::Status, schema::MempoolMetrics)),
    tags()
)]
pub struct ApiDoc;

pub mod schema {
    use lb_tx_service::{MempoolMetrics as DomainMempoolMetrics, backend::Status as DomainStatus};
    use serde::Serialize;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    #[serde(transparent)]
    pub struct MempoolMetrics(pub DomainMempoolMetrics);

    #[derive(ToSchema, Serialize)]
    #[serde(transparent)]
    pub struct Status(pub DomainStatus);
}
