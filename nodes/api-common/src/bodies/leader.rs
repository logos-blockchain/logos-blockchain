use lb_core::mantle::transactions::hash::TxHash;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Serialize, Deserialize, ToSchema)]
pub struct LeaderClaimResponseBody {
    pub tx_hash: TxHash,
}
